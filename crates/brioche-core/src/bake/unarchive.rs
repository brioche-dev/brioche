use std::{collections::BTreeMap, sync::Arc};

use anyhow::Context as _;
use bstr::BString;
use futures::TryStreamExt as _;
use tracing::Instrument;

use crate::{
    recipe::{Artifact, Directory, File, Meta, Unarchive, WithMeta},
    Brioche,
};

#[tracing::instrument(skip(brioche, unarchive), fields(file_recipe = %unarchive.file.hash(), archive = ?unarchive.archive, compression = ?unarchive.compression))]
pub async fn bake_unarchive(
    brioche: &Brioche,
    scope: &super::BakeScope,
    meta: &Arc<Meta>,
    unarchive: Unarchive,
) -> anyhow::Result<Directory> {
    let file = super::bake(brioche, *unarchive.file, scope).await?;
    let Artifact::File(File {
        content_blob: blob_hash,
        ..
    }) = file.value
    else {
        anyhow::bail!("expected archive to be a file");
    };

    tracing::debug!(%blob_hash, archive = ?unarchive.archive, compression = ?unarchive.compression, "starting unarchive");

    let job_id = brioche.reporter.add_job(crate::reporter::NewJob::Unarchive);

    let archive_path = {
        let permit = crate::blob::get_save_blob_permit().await?;
        crate::blob::blob_path(brioche, permit, blob_hash).await?
    };
    let archive_file = tokio::fs::File::open(&archive_path).await?;
    let uncompressed_archive_size = archive_file.metadata().await?.len();
    let archive_file = tokio::io::BufReader::new(archive_file);

    let decompressed_archive_file = unarchive.compression.decompress(archive_file);

    let mut archive = tokio_tar::Archive::new(decompressed_archive_file);
    let mut archive_entries = archive.entries()?;
    let mut directory_entries = BTreeMap::<BString, WithMeta<Artifact>>::new();
    let mut buffer = Vec::new();

    let save_blobs_future = async {
        while let Some(archive_entry) = archive_entries.try_next().await? {
            let entry_path = bstr::BString::new(archive_entry.path_bytes().into_owned());
            let entry_mode = archive_entry.header().mode()?;

            let position = archive_entry.raw_file_position();
            let estimated_progress = position as f64 / (uncompressed_archive_size as f64).max(1.0);
            let progress_percent = (estimated_progress * 100.0).min(99.0) as u8;
            brioche.reporter.update_job(
                job_id,
                crate::reporter::UpdateJob::Unarchive { progress_percent },
            );

            let entry_artifact = match archive_entry.header().entry_type() {
                tokio_tar::EntryType::Regular => {
                    let mut permit = crate::blob::get_save_blob_permit().await?;
                    let entry_blob_hash = crate::blob::save_blob_from_reader(
                        brioche,
                        &mut permit,
                        archive_entry,
                        crate::blob::SaveBlobOptions::new(),
                        &mut buffer,
                    )
                    .await?;
                    let executable = entry_mode & 0o100 != 0;

                    Some(Artifact::File(File {
                        content_blob: entry_blob_hash,
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

                    Some(Artifact::Symlink {
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
                    let linked_entry =
                        directory_entries.get(link_name.as_ref()).with_context(|| {
                            format!(
                            "unsupported tar archive: could not find target for link entry at {}",
                            entry_path
                        )
                        })?;

                    Some(linked_entry.value.clone())
                }
                tokio_tar::EntryType::Directory => Some(Artifact::Directory(Directory::default())),
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

            let entry_path = crate::fs_utils::logical_path_bytes(&entry_path);
            let Ok(entry_path) = entry_path else {
                continue;
            };
            if entry_path.is_empty() {
                continue;
            }
            let Some(entry_artifact) = entry_artifact else {
                continue;
            };

            directory_entries.insert(
                entry_path.into(),
                WithMeta::new(entry_artifact, meta.clone()),
            );
        }

        brioche.reporter.update_job(
            job_id,
            crate::reporter::UpdateJob::Unarchive {
                progress_percent: 100,
            },
        );

        anyhow::Ok(())
    }
    .instrument(tracing::info_span!("save_blobs"));

    save_blobs_future.await?;

    let directory = Directory::create(brioche, &directory_entries).await?;

    Ok(directory)
}
