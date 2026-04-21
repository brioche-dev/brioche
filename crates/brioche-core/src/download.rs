use anyhow::Context as _;
use futures::TryStreamExt as _;
use tokio_util::compat::FuturesAsyncReadCompatExt as _;

use crate::{
    Brioche,
    reporter::job::{NewJob, UpdateJob},
};

#[tracing::instrument(skip_all, fields(%url))]
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

    let download_stream = response
        .bytes_stream()
        .map_err(futures::io::Error::other)
        .into_async_read()
        .compat();
    let download_stream = std::pin::pin!(download_stream);

    let mut last_num_downloaded_bytes = 0;
    let save_blob_options = crate::blob::SaveBlobOptions::new()
        .expected_hash(expected_hash)
        .on_progress(|downloaded_bytes| {
            let downloaded_bytes: u64 = downloaded_bytes.try_into()?;
            last_num_downloaded_bytes = downloaded_bytes;

            brioche.reporter.update_job(
                job_id,
                UpdateJob::Download {
                    downloaded_bytes,
                    total_bytes: content_length,
                    finished_at: None,
                },
            );

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
            downloaded_bytes: last_num_downloaded_bytes,
            total_bytes: Some(last_num_downloaded_bytes),
            finished_at: Some(std::time::Instant::now()),
        },
    );

    Ok(blob_hash)
}

pub async fn fetch_git_commit_for_ref(
    repository: &url::Url,
    reference: &str,
) -> anyhow::Result<String> {
    let repository: gix::Url = repository
        .as_str()
        .try_into()
        .with_context(|| format!("failed to parse git repository URL: {repository}"))?;
    let reference = reference.to_owned();

    // gix uses a blocking client, so run the resolver on the blocking pool
    tokio::task::spawn_blocking(move || resolve_git_ref(&repository, &reference)).await?
}

fn resolve_git_ref(repository: &gix::Url, reference: &str) -> anyhow::Result<String> {
    let mut transport = gix::protocol::transport::client::blocking_io::connect::connect(
        repository.clone(),
        gix::protocol::transport::client::blocking_io::connect::Options::default(),
    )?;

    #[expect(clippy::result_large_err)]
    let empty_auth = |_| Ok(None);
    let outcome = match gix::protocol::handshake(
        &mut transport,
        gix::protocol::transport::Service::UploadPack,
        empty_auth,
        vec![],
        &mut gix::progress::Discard,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);
            return Err(error.into());
        }
    };

    let refs = if let Some(refs) = outcome.refs {
        refs
    } else {
        match gix::protocol::LsRefsCommand::new(None, &outcome.capabilities, ("agent", None))
            .invoke_blocking(&mut transport, &mut gix::progress::Discard, false)
        {
            Ok(refs) => refs,
            Err(error) => {
                let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);
                return Err(error.into());
            }
        }
    };

    // Prefer a name match so a branch or tag shaped like a commit hash still
    // wins over the commit-hash fallback below.
    let named_match = refs.iter().find_map(|remote_ref| {
        let (name, object) = match remote_ref {
            gix::protocol::handshake::Ref::Peeled {
                full_ref_name,
                object,
                ..
            }
            | gix::protocol::handshake::Ref::Direct {
                full_ref_name,
                object,
            }
            | gix::protocol::handshake::Ref::Symbolic {
                full_ref_name,
                object,
                ..
            } => (full_ref_name, object),
            gix::protocol::handshake::Ref::Unborn { .. } => {
                return None;
            }
        };

        if let Some(tag_name) = name.strip_prefix(b"refs/tags/") {
            if tag_name == reference.as_bytes() {
                return Some(object);
            }
        } else if let Some(head_name) = name.strip_prefix(b"refs/heads/")
            && head_name == reference.as_bytes()
        {
            return Some(object);
        }

        None
    });

    if let Some(object) = named_match {
        let object_id = object.to_string();
        let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);
        return Ok(object_id);
    }

    // No name match, so fall back to treating the reference as a full commit
    // hash and validating it via a minimal fetch.
    if let Ok(target_oid) = gix::ObjectId::from_hex(reference.as_bytes()) {
        let result = validate_commit_via_fetch(
            &mut transport,
            &outcome.capabilities,
            outcome.server_protocol_version,
            &target_oid,
        );
        let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);

        return match result {
            Ok(()) => Ok(reference.to_owned()),
            Err(error) => Err(error.context(format!(
                "git commit '{reference}' not found or unreachable in repo {repository}"
            ))),
        };
    }

    let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);
    anyhow::bail!("git ref '{reference}' not found in repo {repository}");
}

fn validate_commit_via_fetch(
    transport: &mut Box<dyn gix::protocol::transport::client::blocking_io::Transport + Send>,
    capabilities: &gix::protocol::transport::client::Capabilities,
    protocol_version: gix::protocol::transport::Protocol,
    oid: &gix::ObjectId,
) -> anyhow::Result<()> {
    let fetch_features =
        gix::protocol::Command::Fetch.default_features(protocol_version, capabilities);

    let mut arguments =
        gix::protocol::fetch::Arguments::new(protocol_version, fetch_features, false);
    arguments.want(oid);

    // Bound the response to roughly a single commit: shallow depth 1, and a
    // blob-less filter when the server supports it. Without these, the server
    // streams the full history reachable from the want.
    if arguments.can_use_deepen() {
        arguments.deepen(1);
    }
    if arguments.can_use_filter() {
        arguments.filter("blob:none");
    }

    let mut reader = arguments.send(transport, true)?;

    // Parse the response to check whether the server accepted the want.
    // An unknown OID causes a server-side error surfaced here.
    let response = gix::protocol::fetch::Response::from_line_reader(
        protocol_version,
        &mut reader,
        true,
        false,
    )?;

    // Drain any pack data so the transport is left in a clean state
    if response.has_pack() {
        std::io::copy(&mut reader, &mut std::io::sink())?;
    }

    Ok(())
}
