use anyhow::Context as _;
use futures::TryStreamExt as _;
use tokio_util::compat::FuturesAsyncReadCompatExt as _;

use crate::brioche::{
    artifact::{Directory, DownloadArtifact, File},
    Brioche,
};

#[tracing::instrument(skip(brioche, download), fields(url = %download.url))]
pub async fn resolve_download(
    brioche: &Brioche,
    download: DownloadArtifact,
) -> anyhow::Result<File> {
    tracing::debug!("acquiring download semaphore permit");
    let _permit = brioche.download_semaphore.acquire().await;
    tracing::debug!("acquired download semaphore permit");

    tracing::debug!(url = %download.url, "starting download");

    let job_id = brioche.reporter.add_job(crate::reporter::NewJob::Download {
        url: download.url.clone(),
    });

    let response = reqwest::get(download.url.clone()).await?;
    let response = response.error_for_status()?;

    let content_length = response.content_length().or_else(|| {
        let content_length = response.headers().get(reqwest::header::CONTENT_LENGTH)?;
        let content_length = content_length.to_str().ok()?.parse().ok()?;
        if content_length == 0 {
            None
        } else {
            Some(content_length)
        }
    });

    let mut download_stream = response
        .bytes_stream()
        .map_err(|e| futures::io::Error::new(futures::io::ErrorKind::Other, e))
        .into_async_read()
        .compat();
    let download_stream = std::pin::pin!(download_stream);

    let save_blob_options = crate::brioche::blob::SaveBlobOptions::new()
        .expected_hash(Some(download.hash))
        .on_progress(|bytes_read| {
            if let Some(content_length) = content_length {
                let progress_percent = (bytes_read as f64 / content_length as f64) * 100.0;
                let progress_percent = progress_percent.round().min(99.0) as u8;
                brioche.reporter.update_job(
                    job_id,
                    crate::reporter::UpdateJob::Download {
                        progress_percent: Some(progress_percent),
                    },
                );
            }

            Ok(())
        });

    let blob_id = crate::brioche::blob::save_blob(brioche, download_stream, save_blob_options)
        .await
        .context("failed to save blob")?;

    brioche.reporter.update_job(
        job_id,
        crate::reporter::UpdateJob::Download {
            progress_percent: Some(100),
        },
    );

    Ok(File {
        data: blob_id,
        executable: false,
        resources: Directory::default(),
    })
}
