use crate::{
    recipe::{Directory, DownloadRecipe, File},
    Brioche,
};

#[tracing::instrument(skip(brioche, download), fields(url = %download.url))]
pub async fn bake_download(brioche: &Brioche, download: DownloadRecipe) -> anyhow::Result<File> {
    let blob_hash = crate::download::download(brioche, &download.url, Some(download.hash)).await?;

    Ok(File {
        content_blob: blob_hash,
        executable: false,
        resources: Directory::default(),
    })
}
