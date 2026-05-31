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
    if is_commit_hash(reference) {
        return Ok(reference.to_ascii_lowercase());
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
        match ls_refs(&mut transport, &outcome.capabilities) {
            Ok(refs) => refs,
            Err(error) => {
                let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);
                return Err(error);
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

/// Runs a protocol V2 `ls-refs` command, echoing back the object format the
/// server advertised during the handshake.
fn ls_refs(
    transport: &mut impl gix::protocol::transport::client::blocking_io::Transport,
    capabilities: &gix::protocol::transport::client::Capabilities,
) -> anyhow::Result<Vec<gix::protocol::handshake::Ref>> {
    use gix::protocol::transport::client::blocking_io::TransportV2Ext as _;

    let object_format = capabilities
        .capability("object-format")
        .and_then(|capability| capability.value())
        .map(ToString::to_string);
    let mut command_capabilities: Vec<(&str, Option<String>)> = vec![("agent", None)];
    if let Some(object_format) = object_format {
        command_capabilities.push(("object-format", Some(object_format)));
    }

    let supports_unborn = capabilities
        .capability("ls-refs")
        .and_then(|capability| capability.supports("unborn"))
        .unwrap_or_default();
    let mut arguments: Vec<gix::bstr::BString> = Vec::new();
    if supports_unborn {
        arguments.push("unborn".into());
    }
    arguments.push("symrefs".into());
    arguments.push("peel".into());

    let mut response = transport.invoke(
        "ls-refs",
        command_capabilities.into_iter(),
        Some(arguments.into_iter()),
        false,
    )?;
    let refs = gix::protocol::handshake::refs::from_v2_refs(&mut response)?;
    Ok(refs)
}
