use std::{collections::BTreeMap, sync::Arc};

use anyhow::Context as _;
use bstr::BString;

use crate::{
    blob::BlobHash,
    recipe::{
        ArchiveFormat, Artifact, CompressionFormat, Directory, File, Meta, Unarchive, WithMeta,
    },
    reporter::job::{NewJob, UpdateJob},
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

    let job_id = brioche.reporter.add_job(NewJob::Unarchive {
        started_at: std::time::Instant::now(),
    });

    let archive_path = {
        let mut permit = crate::blob::get_save_blob_permit().await?;
        crate::blob::blob_path(brioche, &mut permit, blob_hash).await?
    };
    let archive_file = tokio::fs::File::open(&archive_path).await?;
    let uncompressed_archive_size = archive_file.metadata().await?.len();
    let archive_file = tokio::io::BufReader::new(archive_file);

    let (entry_tx, mut entry_rx) = tokio::sync::mpsc::channel(16);

    let mut permit = crate::blob::get_save_blob_permit().await?;
    let process_archive_task = tokio::task::spawn_blocking({
        let brioche = brioche.clone();
        move || {
            match unarchive.archive {
                ArchiveFormat::Tar => {
                    let decompressed_archive_file = unarchive.compression.decompress(archive_file);
                    let decompressed_archive_file =
                        tokio_util::io::SyncIoBridge::new(decompressed_archive_file);

                    let mut archive = tar::Archive::new(decompressed_archive_file);

                    let mut buffer = Vec::new();

                    for archive_entry in archive.entries()? {
                        let archive_entry = archive_entry?;
                        let entry_path =
                            bstr::BString::new(archive_entry.path_bytes().into_owned());
                        let entry_mode = archive_entry.header().mode()?;

                        let position = archive_entry.raw_file_position();
                        let estimated_progress =
                            position as f64 / (uncompressed_archive_size as f64).max(1.0);
                        let progress_percent = (estimated_progress * 100.0).min(99.0) as u8;
                        brioche.reporter.update_job(
                            job_id,
                            UpdateJob::Unarchive {
                                progress_percent,
                                finished_at: None,
                            },
                        );

                        let entry = match archive_entry.header().entry_type() {
                            tar::EntryType::Regular => {
                                let entry_blob_hash = crate::blob::save_blob_from_reader_sync(
                                    &brioche,
                                    &mut permit,
                                    archive_entry,
                                    crate::blob::SaveBlobOptions::new(),
                                    &mut buffer,
                                )?;
                                let executable = entry_mode & 0o100 != 0;

                                ArchiveEntry::File {
                                    content_blob: entry_blob_hash,
                                    executable,
                                }
                            }
                            tar::EntryType::Symlink => {
                                let link_name =
                                    archive_entry.link_name_bytes().with_context(|| {
                                        format!(
                                "unsupported tar archive: no link name for symlink entry at {}",
                                entry_path
                            )
                                    })?;

                                ArchiveEntry::Symlink {
                                    target: link_name.into_owned().into(),
                                }
                            }
                            tar::EntryType::Link => {
                                let link_name =
                                    archive_entry.link_name_bytes().with_context(|| {
                                        format!(
                                "unsupported tar archive: no link name for hardlink entry at {}",
                                entry_path
                            )
                                    })?;

                                ArchiveEntry::Link {
                                    link_name: link_name.into_owned().into(),
                                }
                            }
                            tar::EntryType::Directory => ArchiveEntry::Directory,
                            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader => {
                                // Ignore
                                continue;
                            }
                            other => {
                                anyhow::bail!(
                                    "unsupported tar archive: unsupported entry type {:?} at {}",
                                    other,
                                    entry_path
                                );
                            }
                        };

                        entry_tx.blocking_send((entry_path, entry))?;
                    }
                }
                ArchiveFormat::Zip => {
                    anyhow::ensure!(
                        unarchive.compression == CompressionFormat::None,
                        "zip archives with an extra layer of compression are not supported"
                    );

                    let mut archive =
                        zip::ZipArchive::new(tokio_util::io::SyncIoBridge::new(archive_file))?;
                    let mut buffer = Vec::new();

                    for i in 0..archive.len() {
                        let mut archive_file = archive.by_index(i)?;
                        let entry_path = match archive_file.enclosed_name() {
                            Some(path) => path,
                            None => {
                                anyhow::bail!(
                                    "unsupported zip archive: zip archive contains an invalid file path: {:?}",
                                    archive_file.name(),
                                );
                            }
                        };
                        let entry_path = bstr::BString::new(
                            entry_path.as_os_str().as_encoded_bytes().to_owned(),
                        );

                        let entry = if archive_file.is_dir() {
                            ArchiveEntry::Directory
                        } else if archive_file.is_file() {
                            let entry_blob_hash = crate::blob::save_blob_from_reader_sync(
                                &brioche,
                                &mut permit,
                                &mut archive_file,
                                crate::blob::SaveBlobOptions::new(),
                                &mut buffer,
                            )?;

                            ArchiveEntry::File {
                                content_blob: entry_blob_hash,
                                executable: archive_file
                                    .unix_mode()
                                    .is_some_and(|x| x & 0o100 != 0),
                            }
                        } else if archive_file.is_symlink() {
                            ArchiveEntry::Symlink {
                                target: bstr::BString::new(
                                    std::io::read_to_string(&mut archive_file)?
                                        .as_bytes()
                                        .to_owned(),
                                ),
                            }
                        } else {
                            unreachable!()
                        };

                        entry_tx.blocking_send((entry_path, entry))?;
                    }
                }
            };

            anyhow::Ok(())
        }
    });
    let process_archive_task = async {
        process_archive_task.await??;
        anyhow::Result::<()>::Ok(())
    };

    let build_directory_fut = async {
        let mut directory_entries = BTreeMap::<BString, WithMeta<Artifact>>::new();

        while let Some((entry_path, entry)) = entry_rx.recv().await {
            let entry_artifact = match entry {
                ArchiveEntry::File {
                    content_blob,
                    executable,
                } => Some(Artifact::File(File {
                    content_blob,
                    executable,
                    resources: Directory::default(),
                })),
                ArchiveEntry::Symlink { target } => Some(Artifact::Symlink { target }),
                ArchiveEntry::Link { link_name } => {
                    let linked_entry = directory_entries.get(&link_name).with_context(|| {
                        format!(
                            "unsupported tar archive: could not find target for link entry at {}",
                            entry_path
                        )
                    })?;
                    Some(linked_entry.value.clone())
                }
                ArchiveEntry::Directory => Some(Artifact::Directory(Directory::default())),
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
            UpdateJob::Unarchive {
                progress_percent: 100,
                finished_at: Some(std::time::Instant::now()),
            },
        );

        let directory = Directory::create(brioche, &directory_entries).await?;

        anyhow::Ok(directory)
    };

    let (_, directory) = tokio::try_join!(process_archive_task, build_directory_fut)?;

    Ok(directory)
}

enum ArchiveEntry {
    File {
        content_blob: BlobHash,
        executable: bool,
    },
    Symlink {
        target: BString,
    },
    Link {
        link_name: BString,
    },
    Directory,
}
