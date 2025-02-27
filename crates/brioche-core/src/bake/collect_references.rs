use crate::{
    Brioche,
    recipe::{Artifact, Directory},
};

pub async fn bake_collect_references(
    brioche: &Brioche,
    directory: Directory,
) -> anyhow::Result<Directory> {
    let mut resources_dir = Directory::default();

    let mut updated_directory =
        collect_directory_references(brioche, directory, &mut resources_dir).await?;

    if !resources_dir.is_empty() {
        let mut root_with_resources_dir = Directory::default();
        root_with_resources_dir
            .insert(
                brioche,
                b"brioche-resources.d",
                Some(Artifact::Directory(resources_dir)),
            )
            .await?;

        updated_directory
            .merge(&root_with_resources_dir, brioche)
            .await?;
    }

    Ok(updated_directory)
}

async fn collect_references(
    brioche: &Brioche,
    artifact: Artifact,
    resources_dir: &mut Directory,
) -> anyhow::Result<Artifact> {
    match artifact {
        Artifact::File(mut file) => {
            let file_resources = std::mem::take(&mut file.resources);
            let file_resources =
                collect_directory_references(brioche, file_resources, resources_dir).await?;
            resources_dir.merge(&file_resources, brioche).await?;

            Ok(Artifact::File(file))
        }
        Artifact::Symlink { .. } => Ok(artifact),
        Artifact::Directory(directory) => {
            let new_directory =
                collect_directory_references(brioche, directory, resources_dir).await?;
            Ok(Artifact::Directory(new_directory))
        }
    }
}

async fn collect_directory_references(
    brioche: &Brioche,
    directory: Directory,
    resources_dir: &mut Directory,
) -> anyhow::Result<Directory> {
    let mut new_directory = Directory::default();

    for (name, artifact) in directory.entries(brioche).await? {
        let new_artifact = Box::pin(collect_references(brioche, artifact, resources_dir)).await?;
        new_directory
            .insert(brioche, &name, Some(new_artifact))
            .await?;
    }

    Ok(new_directory)
}
