//! Methods for reading and writing artifacts in the cache with a custom
//! archive format, which is designed to fit well with Brioche's internal
//! `Archive` type, and is designed to work well when used in an object
//! storage system such as S3.
//!
//! When there's a lot of data within an artifact, the data may be chunked
//! into separate files. The archive then records each chunk, making it
//! act as an "index" to re-assemble the data. This both deduplicates data
//! to reduce the space used in the cache, and helps to divide an artifact
//! up into similarly-sized chunks that can be fetched in parallel.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    ops::Range,
    sync::Arc,
};

use futures::{StreamExt as _, TryStreamExt as _};
use object_store::ObjectStoreExt as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{
    BriocheResources, BriocheState,
    blob::{BlobHash, SaveBlobOptions},
    recipe::{
        File, Recipe, RecipeRef, Symlink,
        build::{
            ArtifactBuilder, ArtifactPath, ArtifactPathComponent, build_artifact,
            insert_into_artifact, insert_or_replace_in_artifact,
        },
    },
    reporter::{
        JobId,
        job::{CacheFetchKind, JobContext, NewJob, UpdateJob},
    },
};

const MARKER: &[u8; 32] = b"brioche_artifact_archive_v0     ";

const BLOBS_CHUNKING_THRESHOLD: u64 = 2_097_152;
const CDC_MIN_CHUNK_SIZE: usize = 524_288;
const CDC_AVG_CHUNK_SIZE: usize = 1_048_576;
const CDC_MAX_CHUNK_SIZE: usize = 8_388_608;

#[expect(clippy::similar_names)]
pub async fn write_artifact_archive(
    brioche: &BriocheState,
    artifact_ref: RecipeRef,
    store: &Arc<dyn object_store::ObjectStore>,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin + Send),
) -> Result<(), WriteArtifactError> {
    // Write the marker for a valid archive
    writer.write_all(MARKER).await?;

    // Track all the blobs we need to add in the archive
    let mut artifact_blobs = BTreeSet::<BlobHash>::new();

    // Use a queue to walk through the artifact, starting from the root
    let mut queue = VecDeque::from_iter([(ArtifactPath::default(), artifact_ref)]);
    while let Some((path, artifact_ref)) = queue.pop_front() {
        let artifact = brioche.recipes.get_recipe(artifact_ref);
        match &**artifact {
            Recipe::File(File {
                content_blob,
                executable,
                resources,
            }) => {
                // Save the file's content as a blob we need to write
                artifact_blobs.insert(*content_blob);

                // Write the file entry: tag, path, executable bit, blob hash
                writer.write_all(b"f").await?;
                write_path(&path, writer).await?;
                let executable_tag = if *executable { b"x+" } else { b"x-" };
                writer.write_all(executable_tag).await?;
                writer.write_all(content_blob.as_bytes()).await?;

                // If the file has any resources, enqueue it.
                if let Some(resources) = resources {
                    queue.push_back((
                        path.join_one(ArtifactPathComponent::FileResources),
                        *resources,
                    ));
                }
            }
            Recipe::Symlink(Symlink { target }) => {
                let target_len: u32 = target.len().try_into().map_err(|error| {
                    WriteArtifactError::TryFromIntError {
                        error,
                        message: "symlink target too long".into(),
                    }
                })?;

                // Write the symlink entry: tag, path, target path
                writer.write_all(b"s").await?;
                write_path(&path, writer).await?;
                writer.write_u32(target_len).await?;
                writer.write_all(target.as_slice()).await?;
            }
            Recipe::Directory(directory) => {
                if directory.entries.is_empty() {
                    // Write an empty directory entry: tag, path. Non-empty
                    // directories don't need an entry, since they get created
                    // implicitly by its sub-entries.

                    writer.write_all(b"d").await?;
                    write_path(&path, writer).await?;
                } else {
                    // Enqueue each entry within the directory
                    for (name, entry_ref) in &directory.entries {
                        let entry_path =
                            path.join_one(ArtifactPathComponent::DirectoryEntry(name.clone()));
                        queue.push_back((entry_path, *entry_ref));
                    }
                }
            }
            recipe => {
                return Err(WriteArtifactError::NotAnArtifact {
                    path,
                    recipe_ref: artifact_ref,
                    recipe_kind: recipe.kind(),
                });
            }
        }
    }

    // Get the list of blobs in the archive plus their lengths, in the
    // order to store them in the archive
    let blobs = tokio::task::spawn_blocking({
        let brioche = brioche.resources.clone();
        move || {
            let mut blobs = vec![];

            // Read the size of each blob from the filesystem
            for blob_hash in artifact_blobs {
                let blob_path = crate::blob::local_blob_path(&brioche, blob_hash);

                let metadata = std::fs::metadata(&blob_path).map_err(|error| {
                    WriteArtifactError::IoErrorMessage {
                        error,
                        message: format!("failed to read metadata at '{}'", blob_path.display())
                            .into(),
                    }
                })?;

                blobs.push((blob_hash, metadata.len()));
            }

            // Sort blobs by their lengths as the order to include them in
            // the archive. This works well with content defined chunking, as
            // two similar archives are more likely to have blobs grouped
            // together similarly. In practice, this turns out to save a lot
            // of space compared to sorting by blob hash alone
            blobs.sort_by_key(|(blob_hash, length)| (*length, *blob_hash));

            Ok::<_, WriteArtifactError>(blobs)
        }
    })
    .await
    .unwrap()?;

    let mut blobs_total_length = 0;
    for (blob_hash, length) in &blobs {
        blobs_total_length += length;

        // Write an entry for the blob: tag, blob hash, blob length
        writer.write_all(b"b").await?;
        writer.write_all(blob_hash.as_bytes()).await?;
        writer.write_u64(*length).await?;
    }

    if blobs_total_length >= BLOBS_CHUNKING_THRESHOLD {
        // Lots of data for the blobs, so divide the data into chunks

        // Write a "start chunk" tag. Following this will be a list of chunks
        writer.write_all(b"C").await?;

        let (blobs_reader, mut blobs_writer) = tokio::io::simplex(CDC_MAX_CHUNK_SIZE);

        let read_blobs_task = tokio::spawn({
            let brioche = brioche.resources.clone();
            async move {
                let result = async {
                    // Write each blob to the writer, in order
                    for (blob_hash, _) in blobs {
                        let blob_path = crate::blob::local_blob_path(&brioche, blob_hash);
                        let mut blob_reader =
                            tokio::fs::File::open(&blob_path).await.map_err(|error| {
                                WriteArtifactError::IoErrorMessage {
                                    error,
                                    message: format!("failed to open '{}'", blob_path.display())
                                        .into(),
                                }
                            })?;
                        tokio::io::copy(&mut blob_reader, &mut blobs_writer)
                            .await
                            .map_err(|error| WriteArtifactError::IoErrorMessage {
                                error,
                                message: format!(
                                    "failed to copy blob at '{}'",
                                    blob_path.display()
                                )
                                .into(),
                            })?;
                    }

                    Ok::<_, WriteArtifactError>(())
                }
                .await;

                // Shut down the writer, even if we bail early
                blobs_writer.shutdown().await?;

                result
            }
        });

        // Use the FastCDC algorithm to divide the blob data into chunks
        let mut chunks = fastcdc::v2020::AsyncStreamCDC::new(
            blobs_reader,
            CDC_MIN_CHUNK_SIZE,
            CDC_AVG_CHUNK_SIZE,
            CDC_MAX_CHUNK_SIZE,
        );
        let chunks = chunks.as_stream();
        let mut chunks = std::pin::pin!(chunks);

        while let Some(chunk) = chunks.try_next().await? {
            let chunk_length: u64 =
                chunk
                    .length
                    .try_into()
                    .map_err(|error| WriteArtifactError::TryFromIntError {
                        error,
                        message: "chunk too long".into(),
                    })?;

            // Store the chunk in the cache based on its hash. Note that
            // we're chunking the concatenation of all the blobs together,
            // meaning chunks can be made of multiple blobs or parts of blobs.
            let chunk_hash = blake3::hash(&chunk.data);
            let chunk_compressed_filename = format!("{chunk_hash}.zst");
            let chunk_path =
                object_store::path::Path::from_iter(["chunks", &chunk_compressed_filename]);

            // Compress the chunk data
            let chunk_compressed = zstd::encode_all(&chunk.data[..], 0)?;

            // Try to write the compressed chunk to the cache
            let result = crate::object_store_utils::put_opts_with_retry(
                store,
                &chunk_path,
                chunk_compressed.into(),
                object_store::PutOptions {
                    mode: object_store::PutMode::Create,
                    ..Default::default()
                },
                crate::object_store_utils::PUT_NUM_RETRIES,
                crate::object_store_utils::PUT_RETRY_DELAY,
            )
            .await;

            match result {
                Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {
                    // Chunk was created or already exists
                }
                Err(error) => {
                    return Err(WriteArtifactError::PutObjectError {
                        error,
                        path: chunk_path,
                    });
                }
            }

            // Write an entry for this chunk: tag, chunk hash, chunk length
            writer.write_all(b"c").await?;
            writer.write_all(chunk_hash.as_bytes()).await?;
            writer.write_u64(chunk_length).await?;
        }

        read_blobs_task.await.unwrap()?;
    } else {
        // Not much data for all the blobs, so append it directly to the
        // archive

        // Write an "inline data" tag. Following this will be the raw
        // blob data.
        writer.write_all(b"D").await?;

        // Write each blob to the archive, in order
        for (blob_hash, _) in blobs {
            let blob_path = crate::blob::local_blob_path(&brioche.resources, blob_hash);
            let mut blob_reader = tokio::fs::File::open(blob_path).await?;

            tokio::io::copy(&mut blob_reader, writer).await?;
        }
    }

    Ok(())
}

async fn write_path(
    path: &ArtifactPath,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), WriteArtifactError> {
    // Write the number of components, followed by each component
    let num_components: u32 =
        path.components
            .len()
            .try_into()
            .map_err(|error| WriteArtifactError::TryFromIntError {
                error,
                message: "too many path components".into(),
            })?;
    writer.write_u32(num_components).await?;

    for component in &path.components {
        match component {
            ArtifactPathComponent::DirectoryEntry(name) => {
                let name_len: u16 =
                    name.len()
                        .try_into()
                        .map_err(|error| WriteArtifactError::TryFromIntError {
                            error,
                            message: "directory entry name too long".into(),
                        })?;

                // Write a directory entry component: tag, name length, name
                writer.write_all(b"/").await?;
                writer.write_u16(name_len).await?;
                writer.write_all(name.as_slice()).await?;
            }
            ArtifactPathComponent::FileResources => {
                // Write a "file resource" component: just the tag
                writer.write_all(b"r").await?;
            }
        }
    }

    Ok(())
}

pub enum ArtifactNode {
    File {
        executable: bool,
        content_blob: BlobHash,
    },
    Symlink {
        target: bstr::BString,
    },
    Directory,
}

struct ArtifactEntry {
    path: ArtifactPath,
    node: ArtifactNode,
}

pub enum DataEntry {
    Inline,
    Chunks { chunks: BTreeMap<u64, ChunkEntry> },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChunkEntry {
    pub hash: blake3::Hash,
    pub artifact_range: Range<u64>,
}

pub async fn read_artifact_archive(
    brioche: &mut BriocheState,
    store: &Arc<dyn object_store::ObjectStore>,
    fetch_kind: CacheFetchKind,
    context: JobContext,
    mut reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<RecipeRef, ReadArtifactError> {
    let job_id = brioche.resources.reporter.add_job(
        NewJob::CacheFetch {
            kind: fetch_kind,
            downloaded_bytes: None,
            total_bytes: None,
            started_at: std::time::Instant::now(),
        },
        context,
    );

    // Read and validate the marker from the archive
    let mut marker = [0; MARKER.len()];
    reader.read_exact(&mut marker).await?;
    if marker != *MARKER {
        return Err(ReadArtifactError::InvalidMarker);
    }

    let mut entries = vec![];
    let mut references = vec![];
    let mut artifact_blobs = BTreeSet::new();
    let mut blobs = BTreeMap::new();
    let mut blob_offset = 0;
    let mut chunk_offset = 0;
    let mut data = None;

    loop {
        // Read the next tag from the archive, or exit if we've hit the end
        let mut tag = [0; 1];
        let read_len = reader.read(&mut tag).await?;
        if read_len == 0 {
            break;
        }

        match &tag {
            b"f" => {
                // File tag: read the path, executable bit, and blob hash

                let path = read_path(reader).await?;

                let mut executable_tag = [0; 2];
                reader.read_exact(&mut executable_tag).await?;
                let executable = match &executable_tag {
                    b"x+" => true,
                    b"x-" => false,
                    _ => {
                        return Err(ReadArtifactError::InvalidFileTag {
                            path,
                            tag: executable_tag,
                        });
                    }
                };

                let mut content_blob = [0; blake3::OUT_LEN];
                reader.read_exact(&mut content_blob).await?;
                let content_blob = blake3::Hash::from_bytes(content_blob);
                let content_blob = BlobHash::from_blake3(content_blob.into());

                // Add the file entry
                entries.push(ArtifactEntry {
                    path,
                    node: ArtifactNode::File {
                        executable,
                        content_blob,
                    },
                });

                // Record that we need this blob for the artifact
                artifact_blobs.insert(content_blob);
            }
            b"s" => {
                // Symlink tag: read the path and target path

                let path = read_path(reader).await?;

                let target_len: usize = reader.read_u32().await?.try_into().map_err(|error| {
                    ReadArtifactError::TryFromIntError {
                        error,
                        message: format!(
                            "invalid symlink target length at '{}'",
                            path.display_pretty()
                        )
                        .into(),
                    }
                })?;
                let mut target = vec![0; target_len];
                reader.read_exact(&mut target).await?;
                let target = bstr::BString::new(target);

                // Add the symlink entry
                entries.push(ArtifactEntry {
                    path,
                    node: ArtifactNode::Symlink { target },
                });
            }
            b"d" => {
                // Directory tag: read the path (this is only expected for
                // empty directories)

                let path = read_path(reader).await?;

                // Add the directory entry
                entries.push(ArtifactEntry {
                    path,
                    node: ArtifactNode::Directory,
                });
            }
            b"b" => {
                // Blob tag: read the blob hash and the blob length

                let mut blob_hash = [0; blake3::OUT_LEN];
                reader.read_exact(&mut blob_hash).await?;
                let blob_hash = blake3::Hash::from_bytes(blob_hash);
                let blob_hash = BlobHash::from_blake3(blob_hash.into());

                let length = reader.read_u64().await?;

                // Ensure we need this blob from the entries we've read
                let is_blob_needed = artifact_blobs.remove(&blob_hash);
                if !is_blob_needed {
                    return Err(ReadArtifactError::UnexpectedBlob { blob_hash });
                }

                // Calculate the range of this blob within the archive's data
                let blob_end_offset = blob_offset + length;
                blobs.insert(blob_hash, blob_offset..blob_end_offset);

                blob_offset += length;
            }
            b"D" => {
                // Inline data tag. Once we've read this, we're done reading
                // artifact entries and we're ready to read the blob data
                // from the end of the archive

                data = Some(DataEntry::Inline);
                break;
            }
            b"C" => {
                // "Start chunks" tag. Following this will be the list of
                // chunk entries

                if data.is_some() {
                    return Err(ReadArtifactError::UnexpectedChunksTag);
                }

                data = Some(DataEntry::Chunks {
                    chunks: BTreeMap::new(),
                });
            }
            b"c" => {
                // Chunk tag: read the chunk hash and chunk length

                // Get the list of chunks. This also validates that we
                // encountered a "start chunks" tag (b"C") already.
                let Some(DataEntry::Chunks { chunks }) = &mut data else {
                    return Err(ReadArtifactError::UnexpectedChunkEntry);
                };

                let mut chunk_hash = [0; blake3::OUT_LEN];
                reader.read_exact(&mut chunk_hash).await?;
                let chunk_hash = blake3::Hash::from_bytes(chunk_hash);

                let length = reader.read_u64().await?;

                // Calculate the range of artifact data this chunk covers
                let chunk_end_offset = chunk_offset + length;
                chunks.insert(
                    chunk_offset,
                    ChunkEntry {
                        hash: chunk_hash,
                        artifact_range: chunk_offset..chunk_end_offset,
                    },
                );

                chunk_offset += length;
            }
            b"R" => {
                // Reference tag: read the target path and the source path

                let target_path = read_path(reader).await?;
                let source_path = read_path(reader).await?;

                // Store the reference to process after all entries are read
                references.push((target_path, source_path));
            }
            &[tag] => {
                // Unknown tag

                return Err(ReadArtifactError::UnknownEntryTag { tag });
            }
        }
    }

    let Some(data) = data else {
        return Err(ReadArtifactError::UnexpectedEndOfFile);
    };

    if entries.is_empty() {
        return Err(ReadArtifactError::EmptyArtifact);
    }

    // Determine which blobs we need to read from the archive. We skip over
    // any blobs that we already have locally
    let (needed_blobs, empty_blobs) = tokio::task::spawn_blocking({
        let brioche = brioche.resources.clone();
        move || {
            let mut needed_blobs = vec![];
            let mut empty_blobs = vec![];
            for (blob_hash, range) in blobs {
                let blob_path = crate::blob::local_blob_path(&brioche, blob_hash);
                let blob_path_exists =
                    blob_path
                        .try_exists()
                        .map_err(|error| ReadArtifactError::IoErrorMessage {
                            error,
                            message: format!(
                                "failed to get blob at path '{}'",
                                blob_path.display()
                            )
                            .into(),
                        })?;
                if !blob_path_exists {
                    if range.is_empty() {
                        // Blob doesn't exist locally but is empty. We'll
                        // create it separately from the rest to avoid
                        // special-case rules around ranges
                        empty_blobs.push(blob_hash);
                    } else {
                        // Blob doesn't exist locally, so add it to the list
                        // we need from the archive
                        needed_blobs.push((blob_hash, range));
                    }
                }
            }

            // Make sure the blob is sorted based on the order it appears
            // in the archive, since we can't rewind the reader
            needed_blobs.sort_by_key(|(_, range)| range.start);

            Ok::<_, ReadArtifactError>((needed_blobs, empty_blobs))
        }
    })
    .await
    .unwrap()?;

    let total_needed_bytes = needed_blobs
        .iter()
        .map(|(_, range)| range.end.saturating_sub(range.start))
        .sum::<u64>();
    brioche.resources.reporter.update_job(
        job_id,
        UpdateJob::CacheFetchUpdate {
            downloaded_bytes: None,
            total_bytes: Some(total_needed_bytes),
        },
    );

    // If the archive contained a hash for an empty blob, create it separately
    // since we don't need to read the archive to create it. Normally, this
    // loop will run at most once, but could run multiple times if a malformed
    // archive has different hashes for the empty blob
    for blob_hash in empty_blobs {
        let mut permit = crate::blob::get_save_blob_permit().await;

        // Create the empty blob and validate the hash
        crate::blob::save_blob(
            &brioche.resources,
            &mut permit,
            &[],
            SaveBlobOptions::new().expected_blob_hash(Some(blob_hash)),
        )
        .await?;
    }

    match data {
        DataEntry::Inline => {
            // The archive data is inline in the archive, so we're reading
            // from the same reader right at the end of the archive

            let mut permit = crate::blob::get_save_blob_permit().await;

            let mut inline_offset = 0;
            let mut buffer = vec![];
            for (blob_hash, range) in needed_blobs {
                // Advance the reader to the start of the next needed blob
                let Some(skip_length) = range.start.checked_sub(inline_offset) else {
                    unreachable!("needed blobs are out of order");
                };
                reader_consume_exact(reader, skip_length).await?;

                let length = range.end.checked_sub(range.start);
                let Some(length) = length else {
                    unreachable!("needed blob range is invalid");
                };

                // Read the blob
                let blob_reader = (&mut reader).take(length);
                crate::blob::save_blob_from_reader(
                    &brioche.resources,
                    &mut permit,
                    blob_reader,
                    SaveBlobOptions::new().expected_blob_hash(Some(blob_hash)),
                    &mut buffer,
                )
                .await?;

                brioche.resources.reporter.update_job(
                    job_id,
                    UpdateJob::CacheFetchAdd {
                        downloaded_bytes: Some(length),
                    },
                );

                inline_offset = range.end;
            }
        }
        DataEntry::Chunks { chunks } => {
            // The archive data is broken up into chunks, so we need to
            // determine which parts of which chunks we need to read

            // For each blob, determine if we can read it by reading from
            // a single chunk or if we need to read it from multiple chunks.
            // We also group blobs that can all be read from the same chunk.
            let mut single_chunks = HashMap::<_, Vec<_>>::new();
            let mut multi_chunks = HashMap::new();

            for (blob_hash, blob_range) in needed_blobs {
                // Get the chunk containing the first byte of the blob
                let head_chunk = chunks.range(..=blob_range.start).next_back();
                let Some((_, head_chunk)) = head_chunk else {
                    return Err(ReadArtifactError::NoChunkFoundForBlob { blob_hash });
                };
                let head_chunk = head_chunk.clone();

                // Get any extra the chunks needed to read the rest of the
                // blob. This excludes the head chunk that we already found
                let rest_chunks = if blob_range.end > blob_range.start {
                    chunks
                        .range((blob_range.start + 1)..blob_range.end)
                        .map(|(_, chunk)| chunk.clone())
                        .collect::<Vec<_>>()
                } else {
                    vec![]
                };

                if rest_chunks.is_empty() {
                    // This blob comes entirely from one chunk, so add it
                    // to the list of blobs that only need this single chunk

                    let chunk_blobs = single_chunks.entry(head_chunk).or_default();
                    chunk_blobs.push((blob_hash, blob_range));
                } else {
                    // The blob comes from multiple chunks, so add all the
                    // chunks to a list for this blob

                    let chunks = std::iter::once(head_chunk)
                        .chain(rest_chunks)
                        .map(|chunk| {
                            // Get the range of bytes needed from this chunk
                            // for this blob. Indices are relative to all
                            // of the artifact's data

                            let start_range =
                                std::cmp::max(chunk.artifact_range.start, blob_range.start);
                            let end_range = std::cmp::min(chunk.artifact_range.end, blob_range.end);

                            (chunk, start_range..end_range)
                        })
                        .collect::<Vec<_>>();
                    multi_chunks.insert(blob_hash, chunks);
                }
            }

            // Build a list of things to fetch from the blobs and chunks
            let fetches =
                single_chunks
                    .into_iter()
                    .map(|(chunk, blobs)| BlobsFetch::BlobsFromChunk { chunk, blobs })
                    .chain(multi_chunks.into_iter().map(|(blob_hash, chunks)| {
                        BlobsFetch::BlobFromChunks { blob_hash, chunks }
                    }));

            // Fetch all the blobs from the chunks concurrently
            let concurrent_chunk_fetches = brioche
                .resources
                .cache_client
                .max_concurrent_chunk_fetches
                .unwrap_or(100);
            futures::stream::iter(fetches)
                .map(Ok)
                .try_for_each_concurrent(concurrent_chunk_fetches, |fetch| {
                    fetch_blobs_from_chunks(&brioche.resources, store.clone(), job_id, fetch)
                })
                .await?;
        }
    }

    // Now that we have all the blobs, build the final artifact

    let mut result: Option<ArtifactBuilder> = None;
    for entry in entries {
        insert_into_artifact(
            &mut result,
            &entry.path,
            artifact_builder_from_node(entry.node),
        )?;
    }

    // Record each reference as a placeholder in the tree. Sources are
    // resolved lazily during artifact construction, so insertion order
    // doesn't matter.
    for (target_path, source_path) in references {
        insert_or_replace_in_artifact(
            &mut result,
            &target_path,
            ArtifactBuilder::Reference { source_path },
        )?;
    }

    let Some(result_root) = result else {
        return Err(ReadArtifactError::EmptyArtifact);
    };

    let result = build_artifact(brioche, &result_root)?;

    brioche.resources.reporter.update_job(
        job_id,
        UpdateJob::CacheFetchUpdate {
            downloaded_bytes: Some(total_needed_bytes),
            total_bytes: Some(total_needed_bytes),
        },
    );

    let Ok(()) = crate::recipe::commit_recipes(&brioche.resources).await;

    brioche.resources.reporter.update_job(
        job_id,
        UpdateJob::CacheFetchFinish {
            finished_at: std::time::Instant::now(),
        },
    );

    Ok(result)
}

async fn fetch_blobs_from_chunks(
    brioche: &BriocheResources,
    store: Arc<dyn object_store::ObjectStore>,
    job_id: JobId,
    fetch: BlobsFetch,
) -> Result<(), ReadArtifactError> {
    let mut permit = crate::blob::get_save_blob_permit().await;

    match fetch {
        BlobsFetch::BlobsFromChunk { chunk, blobs } => {
            // Fetch one or more blobs from a single chunk. Because the
            // chunk is compressed, we have to read from the start

            // Get the chunk object from the cache
            let chunk_compressed_filename = format!("{}.zst", chunk.hash);
            let chunk_path =
                object_store::path::Path::from_iter(["chunks", &chunk_compressed_filename]);
            let chunk_object = store.get(&chunk_path).await.map_err(|error| {
                ReadArtifactError::GetObjectError {
                    error,
                    path: chunk_path,
                }
            })?;
            let chunk_stream_compressed = chunk_object.into_stream();
            let chunk_reader_compressed =
                tokio_util::io::StreamReader::new(chunk_stream_compressed);
            let chunk_reader_buffered = tokio::io::BufReader::new(chunk_reader_compressed);
            let mut chunk_reader =
                async_compression::tokio::bufread::ZstdDecoder::new(chunk_reader_buffered);

            let mut artifact_offset = chunk.artifact_range.start;
            for (blob_hash, range) in blobs {
                // Advance the reader to the start of the next blob
                let Some(skip_length) = range.start.checked_sub(artifact_offset) else {
                    panic!("blobs in BlobsFetch are out of order");
                };
                reader_consume_exact(&mut chunk_reader, skip_length).await?;

                let Some(length) = range.end.checked_sub(range.start) else {
                    unreachable!("blob range is invalid");
                };

                // Read the blob from the chunk
                let blob_reader = (&mut chunk_reader).take(length);
                crate::blob::save_blob_from_reader(
                    brioche,
                    &mut permit,
                    blob_reader,
                    SaveBlobOptions::new().expected_blob_hash(Some(blob_hash)),
                    &mut vec![],
                )
                .await?;

                brioche.reporter.update_job(
                    job_id,
                    UpdateJob::CacheFetchAdd {
                        downloaded_bytes: Some(length),
                    },
                );

                artifact_offset = range.end;
            }
        }
        BlobsFetch::BlobFromChunks { blob_hash, chunks } => {
            // Fetch a blob from two or more chunks. Because the
            // chunk is compressed, we have to read from the start

            let (blob_reader, mut blob_writer) = tokio::io::simplex(1_048_576);

            let reporter = brioche.reporter.clone();
            let writer_task = tokio::spawn(async move {
                let result = async {
                    // Read each part of each chunk from the cache to the
                    // writer, which reassembles the original blob
                    for (chunk, range) in chunks {
                        let chunk_compressed_filename = format!("{}.zst", chunk.hash);
                        let chunk_path = object_store::path::Path::from_iter([
                            "chunks",
                            &chunk_compressed_filename,
                        ]);

                        let chunk_object = store.get(&chunk_path).await.map_err(|error| {
                            ReadArtifactError::GetObjectError {
                                error,
                                path: chunk_path,
                            }
                        })?;
                        let chunk_stream_compressed = chunk_object.into_stream();
                        let chunk_reader_compressed =
                            tokio_util::io::StreamReader::new(chunk_stream_compressed);
                        let chunk_reader_buffered =
                            tokio::io::BufReader::new(chunk_reader_compressed);
                        let mut chunk_reader = async_compression::tokio::bufread::ZstdDecoder::new(
                            chunk_reader_buffered,
                        );

                        // Advance the reader to the part of the chunk
                        // needed for the blob
                        let Some(skip_length) = range.start.checked_sub(chunk.artifact_range.start)
                        else {
                            unreachable!("chunks in ChunkFetch are out of order");
                        };
                        reader_consume_exact(&mut chunk_reader, skip_length).await?;

                        let length = range.end.checked_sub(range.start);
                        let Some(length) = length else {
                            unreachable!("chunk range is invalid");
                        };

                        // Write the needed range from the chunk to the writer
                        let mut chunk_reader = chunk_reader.take(length);
                        tokio::io::copy(&mut chunk_reader, &mut blob_writer).await?;
                        reporter.update_job(
                            job_id,
                            UpdateJob::CacheFetchAdd {
                                downloaded_bytes: Some(length),
                            },
                        );
                    }

                    Ok::<_, ReadArtifactError>(())
                }
                .await;

                // Shut down the writer, even if we bail early
                blob_writer.shutdown().await?;

                result
            });

            // Save the blob from the combined parts of the chunks
            crate::blob::save_blob_from_reader(
                brioche,
                &mut permit,
                blob_reader,
                SaveBlobOptions::new().expected_blob_hash(Some(blob_hash)),
                &mut vec![],
            )
            .await?;

            brioche.reporter.update_job(
                job_id,
                UpdateJob::CacheFetchAdd {
                    downloaded_bytes: None,
                },
            );

            writer_task.await.unwrap()?;
        }
    }

    Ok(())
}

fn artifact_builder_from_node(node: ArtifactNode) -> ArtifactBuilder {
    match node {
        ArtifactNode::File {
            executable,
            content_blob,
        } => ArtifactBuilder::File {
            executable,
            content_blob,
            resources: Box::new(Some(ArtifactBuilder::Directory {
                entries: HashMap::new(),
            })),
        },
        ArtifactNode::Symlink { target } => ArtifactBuilder::Symlink { target },
        ArtifactNode::Directory => ArtifactBuilder::Directory {
            entries: HashMap::new(),
        },
    }
}

enum BlobsFetch {
    BlobsFromChunk {
        chunk: ChunkEntry,
        blobs: Vec<(BlobHash, Range<u64>)>,
    },
    BlobFromChunks {
        blob_hash: BlobHash,
        chunks: Vec<(ChunkEntry, Range<u64>)>,
    },
}

async fn read_path(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<ArtifactPath, ReadArtifactError> {
    // Read the number of components, then read each component
    let num_components = reader.read_u32().await?.try_into().map_err(|error| {
        ReadArtifactError::TryFromIntError {
            error,
            message: "invalid number of path components".into(),
        }
    })?;

    let mut components = vec![];
    for _ in 0..num_components {
        // Read the component tag
        let mut tag = [0; 1];
        reader.read_exact(&mut tag).await?;

        match &tag {
            b"/" => {
                // Directory entry component: read the name
                let name_len: usize = reader.read_u16().await?.into();
                let mut name = vec![0; name_len];
                reader.read_exact(&mut name).await?;

                components.push(ArtifactPathComponent::DirectoryEntry(bstr::BString::new(
                    name,
                )));
            }
            b"r" => {
                // File resource component (no data except for the tag)

                components.push(ArtifactPathComponent::FileResources);
            }
            &[tag] => {
                return Err(ReadArtifactError::InvalidPathTag { tag });
            }
        }
    }

    Ok(ArtifactPath { components })
}

async fn reader_consume_exact(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
    length: u64,
) -> std::io::Result<()> {
    if length == 0 {
        return Ok(());
    }

    let mut consume_reader = reader.take(length);
    let consumed = tokio::io::copy(&mut consume_reader, &mut tokio::io::sink()).await?;
    if consumed == length {
        Ok(())
    } else {
        Err(std::io::ErrorKind::UnexpectedEof.into())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WriteArtifactError {
    #[error("{message}: {error}")]
    IoErrorMessage {
        #[source]
        error: std::io::Error,

        message: Cow<'static, str>,
    },

    #[error("{message}: {error}")]
    TryFromIntError {
        #[source]
        error: std::num::TryFromIntError,

        message: Cow<'static, str>,
    },

    #[error("expected artifact at '{}', but was {recipe_kind:?}", .path.display_pretty())]
    NotAnArtifact {
        path: ArtifactPath,
        recipe_ref: RecipeRef,
        recipe_kind: crate::recipe::RecipeKind,
    },

    #[error("error putting object at '{path}': {error}")]
    PutObjectError {
        #[source]
        error: object_store::Error,
        path: object_store::path::Path,
    },

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    FastCdcError(#[from] fastcdc::v2020::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ReadArtifactError {
    #[error("{message}: {error}")]
    IoErrorMessage {
        #[source]
        error: std::io::Error,

        message: Cow<'static, str>,
    },

    #[error("invalid artifact archive marker")]
    InvalidMarker,

    #[error("invalid tag byte encountered while reading path: 0x{tag:02x}")]
    InvalidPathTag { tag: u8 },

    #[error("invalid tag for file entry '{}': {tag:02x?}", .path.display_pretty())]
    InvalidFileTag { path: ArtifactPath, tag: [u8; 2] },

    #[error("artifact archive included a duplicate blob or extra blob: {blob_hash}")]
    UnexpectedBlob { blob_hash: BlobHash },

    #[error("unexpected 'chunks' tag while reading artifact archive")]
    UnexpectedChunksTag,

    #[error("unexpected chunk entry while reading artifact archive")]
    UnexpectedChunkEntry,

    #[error("unknown entry tag in artifact: {tag:02x?}")]
    UnknownEntryTag { tag: u8 },

    #[error("unexpected end of file while reading artifact archive")]
    UnexpectedEndOfFile,

    #[error("artifact archive does not have any entries")]
    EmptyArtifact,

    #[error("no chunk found containing data range for blob {blob_hash}")]
    NoChunkFoundForBlob { blob_hash: BlobHash },

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error("{message}: {error}")]
    TryFromIntError {
        #[source]
        error: std::num::TryFromIntError,

        message: Cow<'static, str>,
    },

    #[error("error getting object at '{path}': {error}")]
    GetObjectError {
        #[source]
        error: object_store::Error,
        path: object_store::path::Path,
    },

    #[error(transparent)]
    SaveBlobError(#[from] crate::blob::SaveBlobError),

    #[error(transparent)]
    ArtifactInsertError(#[from] crate::recipe::build::InsertError),

    #[error(transparent)]
    BuildArtifactError(#[from] crate::recipe::build::BuildArtifactError),
}
