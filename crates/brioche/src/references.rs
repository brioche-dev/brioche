use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use crate::{
    artifact::{ArtifactHash, CompleteProcessArtifact, LazyArtifact, ProcessArtifact},
    blob::BlobId,
    Brioche,
};

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArtifactReferences {
    pub blobs: Vec<BlobId>,
    pub artifacts: HashMap<ArtifactHash, Arc<LazyArtifact>>,
}

pub async fn artifact_references(
    brioche: &Brioche,
    references: &mut ArtifactReferences,
    artifact: LazyArtifact,
) -> anyhow::Result<()> {
    let mut unvisited = VecDeque::from_iter([artifact]);
    while let Some(artifact) = unvisited.pop_front() {
        let hash = artifact.hash();
        match references.artifacts.entry(hash) {
            std::collections::hash_map::Entry::Occupied(_) => {
                // Already visited
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Arc::new(artifact.clone()));

                match artifact {
                    LazyArtifact::File {
                        content_blob,
                        executable: _,
                        resources,
                    } => {
                        references.blobs.push(content_blob);
                        unvisited.push_back(resources.value);
                    }
                    LazyArtifact::Directory(directory) => {
                        let listing = directory.listing(brioche).await?;
                        for (_, artifact) in listing.entries {
                            unvisited.push_back(LazyArtifact::from(artifact.value));
                        }
                    }
                    LazyArtifact::Symlink { .. } => {}
                    LazyArtifact::Download(_) => todo!(),
                    LazyArtifact::Unpack(unpack) => {
                        unvisited.push_back(unpack.file.value);
                    }
                    LazyArtifact::Process(process) => {
                        let ProcessArtifact {
                            command,
                            args,
                            env,
                            work_dir,
                            output_scaffold,
                            platform: _,
                        } = process;

                        let templates = [command].into_iter().chain(args).chain(env.into_values());

                        for template in templates {
                            for component in template.components {
                                match component {
                                    crate::artifact::ProcessTemplateComponent::Input {
                                        artifact,
                                    } => {
                                        unvisited.push_back(artifact.value);
                                    }
                                    crate::artifact::ProcessTemplateComponent::Literal {
                                        ..
                                    }
                                    | crate::artifact::ProcessTemplateComponent::OutputPath
                                    | crate::artifact::ProcessTemplateComponent::ResourcesDir
                                    | crate::artifact::ProcessTemplateComponent::HomeDir
                                    | crate::artifact::ProcessTemplateComponent::WorkDir
                                    | crate::artifact::ProcessTemplateComponent::TempDir => {}
                                }
                            }
                        }

                        unvisited.push_back(work_dir.value);
                        if let Some(output_scaffold) = output_scaffold {
                            unvisited.push_back(output_scaffold.value);
                        }
                    }
                    LazyArtifact::CompleteProcess(process) => {
                        let CompleteProcessArtifact {
                            command,
                            args,
                            env,
                            work_dir,
                            output_scaffold,
                            platform: _,
                        } = process;

                        let templates = [command].into_iter().chain(args).chain(env.into_values());

                        for template in templates {
                            for component in template.components {
                                match component {
                                    crate::artifact::CompleteProcessTemplateComponent::Input {
                                        artifact,
                                    } => {
                                        unvisited.push_back(LazyArtifact::from(artifact.value));
                                    }
                                    crate::artifact::CompleteProcessTemplateComponent::Literal {
                                        ..
                                    }
                                    | crate::artifact::CompleteProcessTemplateComponent::OutputPath
                                    | crate::artifact::CompleteProcessTemplateComponent::ResourcesDir
                                    | crate::artifact::CompleteProcessTemplateComponent::HomeDir
                                    | crate::artifact::CompleteProcessTemplateComponent::WorkDir
                                    | crate::artifact::CompleteProcessTemplateComponent::TempDir => {}
                                }
                            }
                        }

                        unvisited.push_back(LazyArtifact::from(work_dir));
                        if let Some(output_scaffold) = output_scaffold {
                            unvisited.push_back(LazyArtifact::from(*output_scaffold));
                        }
                    }
                    LazyArtifact::CreateFile {
                        content: _,
                        executable: _,
                        resources,
                    } => {
                        unvisited.push_back(resources.value);
                    }
                    LazyArtifact::CreateDirectory(directory) => {
                        for (_, artifact) in directory.entries {
                            unvisited.push_back(artifact.value);
                        }
                    }
                    LazyArtifact::Cast { artifact, to: _ } => {
                        unvisited.push_back(artifact.value);
                    }
                    LazyArtifact::Merge { directories } => {
                        for directory in directories {
                            unvisited.push_back(directory.value);
                        }
                    }
                    LazyArtifact::Peel {
                        directory,
                        depth: _,
                    } => {
                        unvisited.push_back(directory.value);
                    }
                    LazyArtifact::Get { directory, path: _ } => {
                        unvisited.push_back(directory.value);
                    }
                    LazyArtifact::Insert {
                        directory,
                        path: _,
                        artifact,
                    } => {
                        unvisited.push_back(directory.value);
                        if let Some(artifact) = artifact {
                            unvisited.push_back(artifact.value);
                        }
                    }
                    LazyArtifact::SetPermissions {
                        file,
                        executable: _,
                    } => {
                        unvisited.push_back(file.value);
                    }
                    LazyArtifact::Proxy(proxy) => {
                        references.blobs.push(proxy.blob);

                        let inner = proxy.inner(brioche).await?;
                        unvisited.push_back(inner);
                    }
                }
            }
        }
    }

    Ok(())
}
