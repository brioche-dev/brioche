use std::sync::Arc;

use anyhow::Context as _;
use futures::TryStreamExt as _;
use tracing::Instrument;

use crate::brioche::{
    artifact::{
        CompleteArtifact, Directory, DirectoryError, DirectoryListing, File, Meta, UnpackArtifact,
        WithMeta,
    },
    Brioche,
};

#[tracing::instrument(skip(brioche, unpack), fields(file_artifact = %unpack.file.hash(), archive = ?unpack.archive, compression = ?unpack.compression))]
pub async fn resolve_unpack(
    brioche: &Brioche,
    meta: &Arc<Meta>,
    unpack: UnpackArtifact,
) -> anyhow::Result<Directory> {
    let file = super::resolve(brioche, *unpack.file).await?;
    let CompleteArtifact::File(File {
        content_blob: blob_id,
        ..
    }) = file.value
    else {
        anyhow::bail!("expected archive to be a file");
    };

    tracing::debug!(%blob_id, archive = ?unpack.archive, compression = ?unpack.compression, "starting unpack");

    let job_id = brioche.reporter.add_job(crate::reporter::NewJob::Unpack);

    let archive_path = crate::brioche::blob::blob_path(brioche, blob_id);
    let archive_file = tokio::fs::File::open(&archive_path).await?;
    let uncompressed_archive_size = archive_file.metadata().await?.len();
    let archive_file = tokio::io::BufReader::new(archive_file);

    let decompressed_archive_file = unpack.compression.decompress(archive_file);

    let mut archive = tokio_tar::Archive::new(decompressed_archive_file);
    let mut archive_entries = archive.entries()?;
    let mut directory = DirectoryListing::default();

    let save_blobs_future = async {
        while let Some(archive_entry) = archive_entries.try_next().await? {
            let entry_path = bstr::BString::new(archive_entry.path_bytes().into_owned());
            let entry_mode = archive_entry.header().mode()?;

            let position = archive_entry.raw_file_position();
            let estimated_progress = position as f64 / (uncompressed_archive_size as f64).max(1.0);
            let progress_percent = (estimated_progress * 100.0).min(99.0) as u8;
            brioche.reporter.update_job(
                job_id,
                crate::reporter::UpdateJob::Unpack { progress_percent },
            );

            let entry_artifact = match archive_entry.header().entry_type() {
                tokio_tar::EntryType::Regular => {
                    let entry_blob_id = crate::brioche::blob::save_blob_from_reader(
                        brioche,
                        archive_entry,
                        crate::brioche::blob::SaveBlobOptions::new(),
                    )
                    .await?;
                    let executable = entry_mode & 0o100 != 0;

                    Some(CompleteArtifact::File(File {
                        content_blob: entry_blob_id,
                        executable,
                        resources: Directory::default(),
                    }))
                }
                tokio_tar::EntryType::Symlink => {
                    let link_name = archive_entry.link_name_bytes().with_context(|| {
                        format!(
                            "unsupported tar archive: no link name for symlink entry at {}",
                            entry_path
                        )
                    })?;

                    Some(CompleteArtifact::Symlink {
                        target: link_name.into_owned().into(),
                    })
                }
                tokio_tar::EntryType::Link => {
                    let link_name = archive_entry.link_name_bytes().with_context(|| {
                        format!(
                            "unsupported tar archive: no link name for hardlink entry at {}",
                            entry_path
                        )
                    })?;
                    let linked_entry = directory
                        .get(brioche, link_name.as_ref())
                        .await?
                        .with_context(|| {
                            format!(
                            "unsupported tar archive: could not find target for link entry at {}",
                            entry_path
                        )
                        })?;

                    Some(linked_entry.value.clone())
                }
                tokio_tar::EntryType::Directory => {
                    Some(CompleteArtifact::Directory(Directory::default()))
                }
                tokio_tar::EntryType::XGlobalHeader | tokio_tar::EntryType::XHeader => {
                    // Ignore
                    None
                }
                other => {
                    anyhow::bail!(
                        "unsupported tar archive: unsupported entry type {:?} at {}",
                        other,
                        entry_path
                    );
                }
            };

            let insert_result = match entry_artifact {
                Some(entry_artifact) => directory
                    .insert(
                        brioche,
                        &entry_path,
                        Some(WithMeta::new(entry_artifact, meta.clone())),
                    )
                    .await
                    .map(|_| ()),
                None => Ok(()),
            };
            match insert_result {
                Ok(_) => {}
                Err(DirectoryError::EmptyPath { .. }) => {
                    tracing::debug!("skipping empty path in tar archive");
                    // Tarfiles can have entries pointing to the root path, which
                    // we can safely ignore
                }
                Err(error) => {
                    return Err(error.into());
                }
            }
        }

        brioche.reporter.update_job(
            job_id,
            crate::reporter::UpdateJob::Unpack {
                progress_percent: 100,
            },
        );

        anyhow::Ok(())
    }
    .instrument(tracing::info_span!("save_blobs"));

    save_blobs_future.await?;

    let directory = Directory::create(brioche, &directory).await?;
    Ok(directory)
}
