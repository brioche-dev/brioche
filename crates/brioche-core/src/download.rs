use anyhow::Context as _;
use futures::TryStreamExt as _;
use tokio_util::compat::FuturesAsyncReadCompatExt as _;

use crate::{
    reporter::job::{NewJob, UpdateJob},
    Brioche,
};

#[tracing::instrument(skip(brioche, expected_hash))]
pub async fn download(
    brioche: &Brioche,
    url: &url::Url,
    expected_hash: Option<crate::Hash>,
) -> anyhow::Result<crate::blob::BlobHash> {
    // Acquire a permit to save the blob
    let mut save_blob_permit = crate::blob::get_save_blob_permit().await?;

    // Acquire a permit to download
    tracing::debug!("acquiring download semaphore permit");
    let _permit = brioche.download_semaphore.acquire().await?;
    tracing::debug!("acquired download semaphore permit");

    tracing::debug!(%url, "starting download");

    let job_id = brioche.reporter.add_job(NewJob::Download {
        url: url.clone(),
        started_at: std::time::Instant::now(),
    });

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
                    UpdateJob::Download {
                        progress_percent: Some(progress_percent),
                        finished_at: None,
                    },
                );
            }

            Ok(())
        });

    let blob_hash = crate::blob::save_blob_from_reader(
        brioche,
        &mut save_blob_permit,
        download_stream,
        save_blob_options,
        &mut Vec::new(),
    )
    .await
    .context("failed to save blob")?;

    brioche.reporter.update_job(
        job_id,
        UpdateJob::Download {
            progress_percent: Some(100),
            finished_at: Some(std::time::Instant::now()),
        },
    );

    Ok(blob_hash)
}

pub async fn fetch_git_commit_for_ref(repository: &url::Url, ref_: &str) -> anyhow::Result<String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<anyhow::Result<_>>();

    // gix uses a blocking client, so spawn a separate thread to fetch
    std::thread::spawn({
        let repository: gix::Url = repository
            .as_str()
            .try_into()
            .with_context(|| format!("failed to parse git repository URL: {repository}"))?;
        move || {
            // Connect to the repository by URL
            let transport = gix::protocol::transport::connect(repository, Default::default());
            let mut transport = match transport {
                Ok(transport) => transport,
                Err(error) => {
                    let _ = tx.send(Err(error.into()));
                    return;
                }
            };

            // Perform a handshake to get the remote's capabilities.
            // Authentication is disabled
            let empty_auth = |_| Ok(None);
            let outcome = gix::protocol::fetch::handshake(
                &mut transport,
                empty_auth,
                vec![],
                &mut gix::progress::Discard,
            );
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);
                    let _ = tx.send(Err(error.into()));
                    return;
                }
            };

            let refs = match outcome.refs {
                Some(refs) => {
                    // The handshake will sometimes return the refs directly,
                    // depending on protocol version. If that happens, we're
                    // done
                    refs
                }
                None => {
                    // Fetch the refs
                    let refs = gix::protocol::ls_refs(
                        &mut transport,
                        &outcome.capabilities,
                        |_, _, _| Ok(gix::protocol::ls_refs::Action::Continue),
                        &mut gix::progress::Discard,
                        false,
                    );
                    match refs {
                        Ok(refs) => refs,
                        Err(error) => {
                            let _ =
                                gix::protocol::indicate_end_of_interaction(&mut transport, false);
                            let _ = tx.send(Err(error.into()));
                            return;
                        }
                    }
                }
            };

            // End the interaction with the remote
            let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);

            let _ = tx.send(Ok(refs));
        }
    });

    let remote_refs = rx.await?;
    let remote_refs = match remote_refs {
        Ok(remote_refs) => remote_refs,
        Err(error) => {
            anyhow::bail!("{error}");
        }
    };

    // Find the ref that matches the requested ref name
    let object_id = remote_refs
        .iter()
        .find_map(|remote_ref| {
            let (name, object) = match remote_ref {
                gix::protocol::handshake::Ref::Peeled {
                    full_ref_name,
                    object,
                    ..
                } => (full_ref_name, object),
                gix::protocol::handshake::Ref::Direct {
                    full_ref_name,
                    object,
                } => (full_ref_name, object),
                gix::protocol::handshake::Ref::Symbolic {
                    full_ref_name,
                    object,
                    ..
                } => (full_ref_name, object),
                gix::protocol::handshake::Ref::Unborn { .. } => {
                    return None;
                }
            };

            if let Some(tag_name) = name.strip_prefix(b"refs/tags/") {
                if tag_name == ref_.as_bytes() {
                    return Some(object);
                }
            } else if let Some(head_name) = name.strip_prefix(b"refs/heads/") {
                if head_name == ref_.as_bytes() {
                    return Some(object);
                }
            }

            None
        })
        .with_context(|| format!("git ref '{ref_}' not found in repo {repository}"))?;

    let commit = object_id.to_string();
    Ok(commit)
}
