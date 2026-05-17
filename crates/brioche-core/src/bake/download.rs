use std::sync::Arc;

use crate::{
    Brioche,
    recipe::{Directory, DownloadRecipe, File, Meta},
    reporter::job::JobContext,
};

pub async fn bake_download(
    brioche: &Brioche,
    meta: &Arc<Meta>,
    download: DownloadRecipe,
) -> anyhow::Result<File> {
    let blob_hash = crate::download::download(
        brioche,
        &download.url,
        Some(download.hash),
        JobContext::from_meta(meta),
    )
    .await?;

    Ok(File {
        content_blob: blob_hash,
        executable: false,
        resources: Directory::default(),
    })
}
