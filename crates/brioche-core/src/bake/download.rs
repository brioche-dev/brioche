use crate::{
    Brioche,
    recipe::{Directory, DownloadRecipe, File},
    reporter::job::JobContext,
};

pub async fn bake_download(brioche: &Brioche, download: DownloadRecipe) -> anyhow::Result<File> {
    let blob_hash = crate::download::download(
        brioche,
        &download.url,
        Some(download.hash),
        JobContext::default(),
    )
    .await?;

    Ok(File {
        content_blob: blob_hash,
        executable: false,
        resources: Directory::default(),
    })
}
