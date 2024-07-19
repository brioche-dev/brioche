use anyhow::Context as _;
use futures::TryStreamExt as _;
use tokio_util::compat::FuturesAsyncReadCompatExt as _;

use crate::Brioche;

#[tracing::instrument(skip(brioche, expected_hash))]
pub async fn download(
    brioche: &Brioche,
    url: &url::Url,
    expected_hash: Option<crate::Hash>,
) -> anyhow::Result<crate::blob::BlobHash> {
    // Acquire a permit to save the blob
    let save_blob_permit = crate::blob::get_save_blob_permit().await?;

    // Acquire a permit to download
    tracing::debug!("acquiring download semaphore permit");
    let _permit = brioche.download_semaphore.acquire().await?;
    tracing::debug!("acquired download semaphore permit");

    tracing::debug!(%url, "starting download");

    let job_id = brioche
        .reporter
        .add_job(crate::reporter::NewJob::Download { url: url.clone() });

    let response = brioche.download_client.get(url.clone()).send().await?;
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

    let save_blob_options = crate::blob::SaveBlobOptions::new()
        .expected_hash(expected_hash)
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

    let blob_hash = crate::blob::save_blob_from_reader(
        brioche,
        save_blob_permit,
        download_stream,
        save_blob_options,
    )
    .await
    .context("failed to save blob")?;

    brioche.reporter.update_job(
        job_id,
        crate::reporter::UpdateJob::Download {
            progress_percent: Some(100),
        },
    );

    Ok(blob_hash)
}
