use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    ops::Range,
};

use anyhow::Context as _;
use futures::{StreamExt as _, TryStreamExt as _};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{
    blob::{BlobHash, SaveBlobOptions},
    recipe::{Artifact, Recipe},
    Brioche,
};

const MARKER: &[u8; 32] = b"brioche_artifact_archive_v0     ";

const BLOBS_CHUNKING_THRESHOLD: u64 = 2_097_152;
const CDC_MIN_CHUNK_SIZE: u32 = 524_288;
const CDC_AVG_CHUNK_SIZE: u32 = 1_048_576;
const CDC_MAX_CHUNK_SIZE: u32 = 8_388_608;

pub async fn write_artifact_archive(
    brioche: &Brioche,
    artifact: Artifact,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin + Send),
) -> anyhow::Result<()> {
    writer.write_all(MARKER).await?;

    let mut artifact_blobs = BTreeSet::<BlobHash>::new();

    let mut queue = VecDeque::from_iter([(ArtifactPath::default(), artifact)]);
    while let Some((path, artifact)) = queue.pop_front() {
        match artifact {
            Artifact::File(file) => {
                artifact_blobs.insert(file.content_blob);

                writer.write_all(b"f").await?;
                write_path(&path, writer).await?;
                let executable_tag = if file.executable { b"x+" } else { b"x-" };
                writer.write_all(executable_tag).await?;
                writer.write_all(file.content_blob.as_bytes()).await?;

                if !file.resources.is_empty() {
                    queue.push_back((
                        path.child(ArtifactPathComponent::FileResources),
                        Artifact::Directory(file.resources),
                    ));
                }
            }
            Artifact::Symlink { target } => {
                let target_len: u32 = target.len().try_into().context("symlink target too long")?;

                writer.write_all(b"s").await?;
                write_path(&path, writer).await?;
                writer.write_u32(target_len).await?;
                writer.write_all(target.as_slice()).await?;
            }
            Artifact::Directory(directory) => {
                if directory.is_empty() {
                    writer.write_all(b"d").await?;
                    write_path(&path, writer).await?;
                } else {
                    let directory_entries = directory.entries(brioche).await?;

                    for (name, entry) in directory_entries {
                        let entry_path = path
                            .clone()
                            .child(ArtifactPathComponent::DirectoryEntry(name.clone()));
                        queue.push_back((entry_path, entry));
                    }
                }
            }
        }
    }

    let mut blobs = tokio::task::spawn_blocking({
        let brioche = brioche.clone();

        move || {
            let mut blobs = vec![];

            for blob_hash in artifact_blobs {
                let blob_path = crate::blob::local_blob_path(&brioche, blob_hash);

                let metadata = std::fs::metadata(&blob_path);
                let metadata = match metadata {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        continue;
                    }
                    Err(error) => {
                        return Err(error.into());
                    }
                };

                blobs.push((blob_hash, metadata.len()));
            }

            anyhow::Ok(blobs)
        }
    })
    .await??;

    blobs.sort_by_key(|(blob_hash, length)| (*length, *blob_hash));

    let mut blobs_total_length = 0;
    for (blob_hash, length) in &blobs {
        blobs_total_length += length;

        writer.write_all(b"b").await?;
        writer.write_all(blob_hash.as_bytes()).await?;
        writer.write_u64(*length).await?;
    }

    if blobs_total_length >= BLOBS_CHUNKING_THRESHOLD {
        writer.write_all(b"C").await?;

        let (blobs_reader, mut blobs_writer) =
            tokio::io::simplex(CDC_MAX_CHUNK_SIZE.try_into().unwrap());

        let read_blobs_task = tokio::spawn({
            let brioche = brioche.clone();
            async move {
                for (blob_hash, _) in blobs {
                    let blob_path = crate::blob::local_blob_path(&brioche, blob_hash);
                    let mut blob_reader = tokio::fs::File::open(blob_path).await?;
                    tokio::io::copy(&mut blob_reader, &mut blobs_writer).await?;
                }

                blobs_writer.shutdown().await?;

                anyhow::Ok(())
            }
        });

        let mut chunks = fastcdc::v2020::AsyncStreamCDC::new(
            blobs_reader,
            CDC_MIN_CHUNK_SIZE,
            CDC_AVG_CHUNK_SIZE,
            CDC_MAX_CHUNK_SIZE,
        );
        let chunks = chunks.as_stream();
        let mut chunks = std::pin::pin!(chunks);

        while let Some(chunk) = chunks.try_next().await? {
            let chunk_length: u64 = chunk.length.try_into().context("chunk too long")?;

            let chunk_hash = blake3::hash(&chunk.data);
            let chunk_compressed = zstd::encode_all(&chunk.data[..], 0)?;
            let chunk_compressed_filename = format!("{chunk_hash}.zst");
            let chunk_path =
                object_store::path::Path::from_iter(["chunks", &chunk_compressed_filename]);

            let result = brioche
                .cache_object_store
                .put_opts(
                    &chunk_path,
                    chunk_compressed.into(),
                    object_store::PutOptions {
                        mode: object_store::PutMode::Create,
                        ..Default::default()
                    },
                )
                .await;

            match result {
                Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
                Err(error) => {
                    return Err(error.into());
                }
            };

            writer.write_all(b"c").await?;
            writer.write_all(chunk_hash.as_bytes()).await?;
            writer.write_u64(chunk_length).await?;
        }

        read_blobs_task.await??;
    } else {
        writer.write_all(b"D").await?;

        for (blob_hash, _) in blobs {
            let blob_path = crate::blob::local_blob_path(brioche, blob_hash);
            let mut blob_reader = tokio::fs::File::open(blob_path).await?;

            tokio::io::copy(&mut blob_reader, writer).await?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct ArtifactPath {
    pub components: Vec<ArtifactPathComponent>,
}

impl ArtifactPath {
    fn child(mut self, component: ArtifactPathComponent) -> Self {
        self.components.push(component);
        self
    }

    fn display_pretty(&self) -> String {
        let mut display_pretty = String::new();
        for component in &self.components {
            match component {
                ArtifactPathComponent::DirectoryEntry(name) => {
                    display_pretty.push('/');
                    display_pretty.push_str(&urlencoding::encode_binary(name));
                }
                ArtifactPathComponent::FileResources => {
                    display_pretty.push('$');
                }
            }
        }

        display_pretty
    }
}

#[derive(Debug, Clone)]
pub enum ArtifactPathComponent {
    DirectoryEntry(bstr::BString),
    FileResources,
}

async fn write_path(
    path: &ArtifactPath,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> anyhow::Result<()> {
    let num_components: u32 = path
        .components
        .len()
        .try_into()
        .context("too many path components")?;
    writer.write_u32(num_components).await?;
    for component in &path.components {
        match component {
            ArtifactPathComponent::DirectoryEntry(name) => {
                let name_len: u16 = name
                    .len()
                    .try_into()
                    .context("directory entry name too long")?;

                writer.write_all(b"/").await?;
                writer.write_u16(name_len).await?;
                writer.write_all(name.as_slice()).await?;
            }
            ArtifactPathComponent::FileResources => {
                writer.write_all(b"r").await?;
            }
        }
    }

    Ok(())
}

struct ArtifactEntry {
    path: ArtifactPath,
    node: ArtifactNode,
}

enum ArtifactNode {
    File {
        executable: bool,
        content_blob: BlobHash,
    },
    Symlink {
        target: bstr::BString,
    },
    Directory,
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
    brioche: &Brioche,
    mut reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> anyhow::Result<Artifact> {
    let mut marker = [0; MARKER.len()];
    reader.read_exact(&mut marker).await?;
    if marker != *MARKER {
        return Err(anyhow::anyhow!("invalid artifact archive marker"));
    }

    let mut entries = vec![];
    let mut artifact_blobs = BTreeSet::new();
    let mut blobs = BTreeMap::new();
    let mut blob_offset = 0;
    let mut chunk_offset = 0;
    let mut data = None;

    loop {
        let mut tag = [0; 1];
        let read_len = reader.read(&mut tag).await?;

        if read_len == 0 {
            break;
        }

        match &tag {
            b"f" => {
                let path = read_path(reader).await?;

                let mut executable_tag = [0; 2];
                reader.read_exact(&mut executable_tag).await?;
                let executable = match &executable_tag {
                    b"x+" => true,
                    b"x-" => false,
                    _ => {
                        anyhow::bail!(
                            "invalid executable flag while reading file entry: {}",
                            path.display_pretty()
                        );
                    }
                };

                let mut content_blob = [0; blake3::OUT_LEN];
                reader.read_exact(&mut content_blob).await?;
                let content_blob = blake3::Hash::from_bytes(content_blob);
                let content_blob = BlobHash::from_blake3(content_blob);

                artifact_blobs.insert(content_blob);
                entries.push(ArtifactEntry {
                    path,
                    node: ArtifactNode::File {
                        executable,
                        content_blob,
                    },
                });
            }
            b"s" => {
                let path = read_path(reader).await?;

                let target_len: usize = reader.read_u32().await?.try_into()?;
                let mut target = vec![0; target_len];
                reader.read_exact(&mut target).await?;
                let target = bstr::BString::new(target);

                entries.push(ArtifactEntry {
                    path,
                    node: ArtifactNode::Symlink { target },
                });
            }
            b"d" => {
                let path = read_path(reader).await?;
                entries.push(ArtifactEntry {
                    path,
                    node: ArtifactNode::Directory,
                });
            }
            b"b" => {
                let mut blob_hash = [0; blake3::OUT_LEN];
                reader.read_exact(&mut blob_hash).await?;
                let blob_hash = blake3::Hash::from_bytes(blob_hash);
                let blob_hash = BlobHash::from_blake3(blob_hash);

                let length = reader.read_u64().await?;

                let is_blob_needed = artifact_blobs.remove(&blob_hash);
                anyhow::ensure!(
                    is_blob_needed,
                    "archive artifact included a duplicate blob or extra blob: {blob_hash}"
                );

                let blob_end_offset = blob_offset + length;
                blobs.insert(blob_hash, blob_offset..blob_end_offset);

                blob_offset += length;
            }
            b"D" => {
                data = Some(DataEntry::Inline);
                break;
            }
            b"C" => {
                anyhow::ensure!(
                    data.is_none(),
                    "unexpected chunks tag while reading artifact archive"
                );

                data = Some(DataEntry::Chunks {
                    chunks: BTreeMap::new(),
                });
            }
            b"c" => {
                let Some(DataEntry::Chunks { chunks }) = &mut data else {
                    anyhow::bail!("unexpected chunk entry while reading artifact archive");
                };

                let mut chunk_hash = [0; blake3::OUT_LEN];
                reader.read_exact(&mut chunk_hash).await?;
                let chunk_hash = blake3::Hash::from_bytes(chunk_hash);

                let length = reader.read_u64().await?;

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
            tag => {
                anyhow::bail!(
                    "unexpected tag byte encountered while reading artifact archive: {tag:?}",
                );
            }
        }
    }

    let Some(data) = data else {
        return Err(anyhow::anyhow!(
            "unexpected end of file while reading artifact archive"
        ));
    };

    anyhow::ensure!(
        !entries.is_empty(),
        "artifact archive does not have any entries"
    );

    let needed_blobs = tokio::task::spawn_blocking({
        let brioche = brioche.clone();
        move || {
            let mut needed_blobs = vec![];
            for (blob_hash, range) in blobs {
                let blob_path = crate::blob::local_blob_path(&brioche, blob_hash);
                if !blob_path.try_exists()? {
                    needed_blobs.push((blob_hash, range));
                }
            }

            needed_blobs.sort_by_key(|(_, range)| range.start);

            anyhow::Ok(needed_blobs)
        }
    })
    .await??;

    match data {
        DataEntry::Inline => {
            let mut permit = crate::blob::get_save_blob_permit().await?;

            let mut inline_offset = 0;
            let mut buffer = vec![];
            for (blob_hash, range) in needed_blobs {
                let skip_length = range.start.checked_sub(inline_offset);
                match skip_length {
                    Some(0) => {}
                    Some(skip_length) => {
                        let mut skip_reader = (&mut reader).take(skip_length);
                        tokio::io::copy(&mut skip_reader, &mut tokio::io::sink()).await?;
                    }
                    None => {
                        unreachable!("needed blobs are out of order");
                    }
                }

                let length = range.end.checked_sub(range.start);
                let Some(length) = length else {
                    unreachable!("needed blob range is invalid");
                };
                let blob_reader = (&mut reader).take(length);
                crate::blob::save_blob_from_reader(
                    brioche,
                    &mut permit,
                    blob_reader,
                    SaveBlobOptions::new().expected_blob_hash(Some(blob_hash)),
                    &mut buffer,
                )
                .await?;
                inline_offset = range.end;
            }
        }
        DataEntry::Chunks { chunks } => {
            let mut single_chunks = HashMap::<_, Vec<_>>::new();
            let mut multi_chunks = HashMap::new();

            for (blob_hash, blob_range) in needed_blobs {
                let head_chunk = chunks.range(..=blob_range.start).next_back();
                let Some((_, head_chunk)) = head_chunk else {
                    anyhow::bail!("no chunk found containing data range for blob {blob_hash}");
                };
                let head_chunk = head_chunk.clone();
                let rest_chunks = chunks
                    .range((blob_range.start + 1)..blob_range.end)
                    .map(|(_, chunk)| chunk.clone())
                    .collect::<Vec<_>>();

                if rest_chunks.is_empty() {
                    let chunk_blobs = single_chunks.entry(head_chunk).or_default();
                    chunk_blobs.push((blob_hash, blob_range));
                } else {
                    let chunks = [head_chunk]
                        .into_iter()
                        .chain(rest_chunks)
                        .map(|chunk| {
                            let start_range =
                                std::cmp::max(chunk.artifact_range.start, blob_range.start);
                            let end_range = std::cmp::min(chunk.artifact_range.end, blob_range.end);
                            (chunk, start_range..end_range)
                        })
                        .collect::<Vec<_>>();
                    multi_chunks.insert(blob_hash, chunks);
                }
            }

            let fetches =
                single_chunks
                    .into_iter()
                    .map(|(chunk, blobs)| BlobsFetch::BlobsFromChunk { chunk, blobs })
                    .chain(multi_chunks.into_iter().map(|(blob_hash, chunks)| {
                        BlobsFetch::BlobFromChunks { blob_hash, chunks }
                    }));

            futures::stream::iter(fetches)
                .map(Ok)
                .try_for_each_concurrent(50, |fetch| {
                    fetch_blobs_from_chunks(brioche.clone(), fetch)
                })
                .await?;
        }
    }

    let mut result: Option<ArtifactBuilder> = None;
    for entry in entries {
        insert_into_artifact(&mut result, &entry.path, &entry.path.components, entry.node)?;
    }

    let result = result.context("no artifact entries in archive")?;
    let result = result.build(brioche).await?;
    Ok(result)
}

async fn fetch_blobs_from_chunks(brioche: Brioche, fetch: BlobsFetch) -> anyhow::Result<()> {
    let mut permit = crate::blob::get_save_blob_permit().await?;

    match fetch {
        BlobsFetch::BlobsFromChunk { chunk, blobs } => {
            let chunk_compressed_filename = format!("{}.zst", chunk.hash);
            let chunk_path =
                object_store::path::Path::from_iter(["chunks", &chunk_compressed_filename]);

            let chunk_object = brioche.cache_object_store.get(&chunk_path).await?;
            let chunk_stream_compressed = chunk_object.into_stream();
            let chunk_reader_compressed =
                tokio_util::io::StreamReader::new(chunk_stream_compressed);
            let mut chunk_reader =
                async_compression::tokio::bufread::ZstdDecoder::new(chunk_reader_compressed);

            let mut artifact_offset = chunk.artifact_range.start;
            for (blob_hash, range) in blobs {
                let skip_length = artifact_offset.checked_sub(range.start);

                match skip_length {
                    Some(0) => {}
                    Some(skip_length) => {
                        let mut skip_reader = (&mut chunk_reader).take(skip_length);
                        tokio::io::copy(&mut skip_reader, &mut tokio::io::sink()).await?;
                    }
                    None => {
                        unreachable!("blobs in ChunkFetch are out of order");
                    }
                }

                let length = range.end.checked_sub(range.start);
                let Some(length) = length else {
                    unreachable!("blob range is invalid");
                };
                let blob_reader = (&mut chunk_reader).take(length);
                crate::blob::save_blob_from_reader(
                    &brioche,
                    &mut permit,
                    blob_reader,
                    SaveBlobOptions::new().expected_blob_hash(Some(blob_hash)),
                    &mut vec![],
                )
                .await?;

                artifact_offset = range.end;
            }
        }
        BlobsFetch::BlobFromChunks { blob_hash, chunks } => {
            let (blob_reader, mut blob_writer) = tokio::io::simplex(1_048_576);

            let writer_task = tokio::spawn({
                let brioche = brioche.clone();
                async move {
                    for (chunk, range) in chunks {
                        let chunk_compressed_filename = format!("{}.zst", chunk.hash);
                        let chunk_path = object_store::path::Path::from_iter([
                            "chunks",
                            &chunk_compressed_filename,
                        ]);

                        let chunk_object = brioche.cache_object_store.get(&chunk_path).await?;
                        let chunk_stream_compressed = chunk_object.into_stream();
                        let chunk_reader_compressed =
                            tokio_util::io::StreamReader::new(chunk_stream_compressed);
                        let mut chunk_reader = async_compression::tokio::bufread::ZstdDecoder::new(
                            chunk_reader_compressed,
                        );

                        let skip_length = range.start.checked_sub(chunk.artifact_range.start);
                        match skip_length {
                            Some(0) => {}
                            Some(skip_length) => {
                                let mut skip_reader = (&mut chunk_reader).take(skip_length);
                                tokio::io::copy(&mut skip_reader, &mut tokio::io::sink()).await?;
                            }
                            None => {
                                unreachable!("chunks in ChunkFetch are out of order");
                            }
                        }

                        let length = range.end.checked_sub(range.start);
                        let Some(length) = length else {
                            unreachable!("chunk range is invalid");
                        };
                        let mut chunk_reader = chunk_reader.take(length);
                        tokio::io::copy(&mut chunk_reader, &mut blob_writer).await?;
                    }

                    blob_writer.shutdown().await?;

                    anyhow::Ok(())
                }
            });

            crate::blob::save_blob_from_reader(
                &brioche,
                &mut permit,
                blob_reader,
                SaveBlobOptions::new().expected_blob_hash(Some(blob_hash)),
                &mut vec![],
            )
            .await?;

            writer_task.await??;
        }
    }

    Ok(())
}

fn insert_into_artifact(
    container: &mut Option<ArtifactBuilder>,
    full_path: &ArtifactPath,
    components: &[ArtifactPathComponent],
    artifact: ArtifactNode,
) -> anyhow::Result<()> {
    match components {
        [] => {
            anyhow::ensure!(
                container.is_none(),
                "archive entry tried to override path {:?}",
                full_path.display_pretty()
            );
            *container = Some(ArtifactBuilder::from_node(artifact));
        }
        [ArtifactPathComponent::DirectoryEntry(name), rest @ ..] => {
            let container = container.get_or_insert_with(ArtifactBuilder::empty_dir);
            let ArtifactBuilder::Directory { entries } = container else {
                anyhow::bail!(
                    "path {:?} descends into non-directory",
                    full_path.display_pretty()
                );
            };
            let entry = entries.entry(name.to_owned()).or_default();
            insert_into_artifact(entry, full_path, rest, artifact)?;
        }
        [ArtifactPathComponent::FileResources, rest @ ..] => {
            let Some(container) = container else {
                anyhow::bail!(
                    "path {:?} tried to add resource to a file that doesn't exist",
                    full_path.display_pretty()
                );
            };
            let ArtifactBuilder::File { resources, .. } = container else {
                anyhow::bail!(
                    "path {:?} tried to add resource to a non-file",
                    full_path.display_pretty()
                );
            };

            insert_into_artifact(resources.as_mut(), full_path, rest, artifact)?;
        }
    }

    Ok(())
}

enum ArtifactBuilder {
    File {
        executable: bool,
        content_blob: BlobHash,
        resources: Box<Option<ArtifactBuilder>>,
    },
    Symlink {
        target: bstr::BString,
    },
    Directory {
        entries: HashMap<bstr::BString, Option<ArtifactBuilder>>,
    },
}

impl ArtifactBuilder {
    fn from_node(node: ArtifactNode) -> Self {
        match node {
            ArtifactNode::File {
                executable,
                content_blob,
            } => Self::File {
                executable,
                content_blob,
                resources: Box::new(Some(ArtifactBuilder::Directory {
                    entries: HashMap::new(),
                })),
            },
            ArtifactNode::Symlink { target } => Self::Symlink { target },
            ArtifactNode::Directory => Self::Directory {
                entries: HashMap::new(),
            },
        }
    }

    fn empty_dir() -> Self {
        Self::Directory {
            entries: HashMap::new(),
        }
    }

    async fn build(self, brioche: &Brioche) -> anyhow::Result<Artifact> {
        match self {
            ArtifactBuilder::File {
                executable,
                content_blob,
                resources,
            } => {
                let resources = resources.unwrap_or_else(ArtifactBuilder::empty_dir);
                let resources = Box::pin(resources.build(brioche)).await?;
                let Artifact::Directory(resources) = resources else {
                    anyhow::bail!("file resources builder did not return a directory");
                };

                Ok(Artifact::File(crate::recipe::File {
                    executable,
                    content_blob,
                    resources,
                }))
            }
            ArtifactBuilder::Symlink { target } => Ok(Artifact::Symlink { target }),
            ArtifactBuilder::Directory { entries } => {
                let mut entry_artifact_hashes = BTreeMap::new();
                let mut recipes = vec![];
                let entries = entries
                    .into_iter()
                    .filter_map(|(name, entry)| Some((name, entry?)));
                for (name, entry) in entries {
                    let entry = Box::pin(entry.build(brioche)).await?;
                    entry_artifact_hashes.insert(name, entry.hash());
                    recipes.push(Recipe::from(entry))
                }

                crate::recipe::save_recipes(brioche, recipes).await?;

                Ok(Artifact::Directory(crate::recipe::Directory::from_entries(
                    entry_artifact_hashes,
                )))
            }
        }
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
) -> anyhow::Result<ArtifactPath> {
    let num_components = reader.read_u32().await?.try_into()?;

    let mut components = vec![];
    for _ in 0..num_components {
        let mut tag = [0; 1];
        reader.read_exact(&mut tag).await?;
        match &tag {
            b"/" => {
                let name_len: usize = reader.read_u16().await?.into();
                let mut name = vec![0; name_len];
                reader.read_exact(&mut name).await?;
                components.push(ArtifactPathComponent::DirectoryEntry(bstr::BString::new(
                    name,
                )));
            }
            b"r" => {
                components.push(ArtifactPathComponent::FileResources);
            }
            tag => {
                anyhow::bail!("invalid tag byte encountered while reading path: {:?}", tag);
            }
        }
    }

    Ok(ArtifactPath { components })
}
