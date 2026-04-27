use anyhow::Context as _;

/// Returns `true` if the value is a SHA-1 or SHA-256 commit hash spelled as a
/// hexadecimal string. A commit hash is its own pin, so it should never appear
/// in the lockfile's `git_refs` section.
#[must_use]
pub fn is_commit_hash(value: &str) -> bool {
    is_sha1_hex(value) || is_sha256_hex(value)
}

fn is_sha1_hex(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Resolves a git ref to a commit hash for the given repository. Branch and
/// tag names are looked up against the remote; commit hashes are returned
/// directly without contacting the server.
pub async fn resolve_ref_to_commit(
    repository: &url::Url,
    reference: &str,
) -> anyhow::Result<String> {
    if is_sha1_hex(reference) {
        return Ok(reference.to_ascii_lowercase());
    }

    // See https://github.com/GitoxideLabs/gitoxide/issues/281.
    if is_sha256_hex(reference) {
        anyhow::bail!(
            "git ref '{reference}' looks like a SHA-256 commit hash, which is not yet supported"
        );
    }

    let repository: gix::Url = repository
        .as_str()
        .try_into()
        .with_context(|| format!("failed to parse git repository URL: {repository}"))?;
    let reference = reference.to_owned();

    // gix uses a blocking client, so run the resolver on the blocking pool
    tokio::task::spawn_blocking(move || resolve_named_ref(&repository, &reference)).await?
}

fn resolve_named_ref(repository: &gix::Url, reference: &str) -> anyhow::Result<String> {
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

    let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);

    let object_id = refs
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

    Ok(object_id.to_string())
}
