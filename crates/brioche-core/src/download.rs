use std::{path::Path, sync::atomic::AtomicBool};

use anyhow::Context as _;
use futures::TryStreamExt as _;
use tokio_util::compat::FuturesAsyncReadCompatExt as _;

use crate::{
    Brioche,
    reporter::job::{NewJob, UpdateJob},
};

fn parse_git_repository_url(repository: &url::Url) -> anyhow::Result<gix::Url> {
    repository
        .as_str()
        .try_into()
        .with_context(|| format!("failed to parse git repository URL: {repository}"))
}

fn parse_git_commit_id(commit: &str) -> anyhow::Result<gix::hash::ObjectId> {
    gix::hash::ObjectId::from_hex(commit.as_bytes())
        .with_context(|| format!("invalid git commit hash {commit}"))
}

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
    let (tx, rx) = tokio::sync::oneshot::channel::<anyhow::Result<_>>();

    // gix uses a blocking client, so spawn a separate thread to fetch
    std::thread::spawn({
        let repository = parse_git_repository_url(repository)?;
        move || {
            // For proper authentication, we need to create a config
            let config = gix::config::File::from_globals().unwrap_or_default();

            // Get credential helpers from config
            let (mut cascade, _action, _prompt_options) = match gix::config::credential_helpers(
                repository.clone(),
                &config,
                false,
                |_| true,
                gix::open::permissions::Environment::all(),
                false,
            ) {
                Ok(result) => result,
                Err(error) => {
                    let _ = tx.send(Err(error.into()));
                    return;
                }
            };

            // Authenticate function that uses the cascade
            let auth = move |action: gix::credentials::helper::Action| {
                cascade.invoke(action, gix::prompt::Options::default())
            };

            // Connect to the repository by URL
            let transport = gix::protocol::transport::client::blocking_io::connect::connect(
                repository,
                gix::protocol::transport::client::blocking_io::connect::Options::default(),
            );
            let mut transport = match transport {
                Ok(transport) => transport,
                Err(error) => {
                    let _ = tx.send(Err(error.into()));
                    return;
                }
            };

            // Perform a handshake with authentication support
            let outcome = gix::protocol::handshake(
                &mut transport,
                gix::protocol::transport::Service::UploadPack,
                auth,
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

            let refs = if let Some(refs) = outcome.refs {
                // The handshake will sometimes return the refs directly,
                // depending on protocol version. If that happens, we're
                // done
                refs
            } else {
                // Fetch the refs
                let refs =
                    gix::protocol::LsRefsCommand::new(None, &outcome.capabilities, ("agent", None))
                        .invoke_blocking(&mut transport, &mut gix::progress::Discard, false);
                match refs {
                    Ok(refs) => refs,
                    Err(error) => {
                        let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);
                        let _ = tx.send(Err(error.into()));
                        return;
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
        })
        .with_context(|| format!("git ref '{reference}' not found in repo {repository}"))?;

    let commit = object_id.to_string();
    Ok(commit)
}

pub async fn git_checkout(
    repository: &url::Url,
    reference: &str,
    commit: &str,
    output_dir: &Path,
) -> anyhow::Result<()> {
    #[derive(Clone)]
    struct RepoObjectFinder {
        repo: gix::Repository,
    }

    impl gix::objs::Find for RepoObjectFinder {
        fn try_find<'a>(
            &self,
            id: &gix::hash::oid,
            buffer: &'a mut Vec<u8>,
        ) -> Result<Option<gix::objs::Data<'a>>, gix::objs::find::Error> {
            let Some(object) = self.repo.try_find_object(id).map_err(|error| error.0)? else {
                return Ok(None);
            };

            buffer.clear();
            buffer.extend_from_slice(&object.data);

            Ok(Some(gix::objs::Data::new(object.kind, buffer.as_slice())))
        }
    }

    let repository = repository.clone();
    let reference = reference.to_string();
    let commit = commit.to_string();
    let output_dir = output_dir.to_path_buf();

    tokio::task::spawn_blocking(move || {
        if output_dir.exists() {
            return Ok(());
        }

        let parent_dir = output_dir
            .parent()
            .with_context(|| format!("checkout path {} had no parent", output_dir.display()))?;
        std::fs::create_dir_all(parent_dir)
            .with_context(|| format!("failed to create {}", parent_dir.display()))?;

        let temp_dir = parent_dir.join(format!("checkout-tmp-{}", ulid::Ulid::new()));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir).with_context(|| {
                format!(
                    "failed to remove temporary directory {}",
                    temp_dir.display()
                )
            })?;
        }
        std::fs::create_dir_all(&temp_dir)
            .with_context(|| format!("failed to create {}", temp_dir.display()))?;

        let repository = parse_git_repository_url(&repository)?;
        let mut prepare_fetch = gix::clone::PrepareFetch::new(
            repository,
            &temp_dir,
            gix::create::Kind::WithWorktree,
            gix::create::Options::default(),
            gix::open::Options::default(),
        )?
        .with_ref_name(Some(reference.as_str()))
        .map_err(anyhow::Error::from)?;

        let should_interrupt = AtomicBool::new(false);
        let (repo, _) = prepare_fetch.fetch_only(gix::progress::Discard, &should_interrupt)?;

        let commit_id = parse_git_commit_id(&commit)?;
        let commit = repo.find_commit(commit_id).with_context(|| {
            format!("commit {commit_id} not found after fetching ref {reference}")
        })?;
        let tree_id = commit.tree_id()?.detach();
        let mut index = repo.index_from_tree(&tree_id)?;

        let mut checkout_options =
            repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
        checkout_options.destination_is_initially_empty = true;

        let finder = RepoObjectFinder { repo: repo.clone() };
        let files_progress = gix::progress::Discard;
        let bytes_progress = gix::progress::Discard;
        gix::worktree::state::checkout(
            &mut index,
            repo.workdir()
                .with_context(|| format!("repository {} has no workdir", temp_dir.display()))?,
            finder,
            &files_progress,
            &bytes_progress,
            &should_interrupt,
            checkout_options,
        )?;
        index.write(Default::default())?;

        let dot_git_dir = temp_dir.join(".git");
        if dot_git_dir.exists() {
            std::fs::remove_dir_all(&dot_git_dir)
                .with_context(|| format!("failed to remove {}", dot_git_dir.display()))?;
        }

        match std::fs::rename(&temp_dir, &output_dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::remove_dir_all(&temp_dir).with_context(|| {
                    format!("failed to remove {} after rename race", temp_dir.display())
                })?;
                Ok(())
            }
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to move git checkout from {} to {}",
                    temp_dir.display(),
                    output_dir.display()
                )
            }),
        }
    })
    .await??;

    Ok(())
}
