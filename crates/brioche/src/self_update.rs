use std::{
    io::{IsTerminal as _, Write as _},
    path::PathBuf,
    sync::LazyLock,
};

use anyhow::Context as _;
use clap::Parser;
use futures::{TryFutureExt as _, TryStreamExt as _};
use sha2::Digest as _;
use tokio::io::{AsyncBufReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};

use crate::CURRENT_VERSION;

static BRIOCHE_RELEASES_URL: LazyLock<url::Url> = LazyLock::new(|| {
    let custom_releases_url = option_env!("BRIOCHE_CUSTOM_RELEASES_URL");
    let releases_url = custom_releases_url.unwrap_or("https://releases.brioche.dev/");
    releases_url
        .parse()
        .expect("invalid value for BRIOCHE_RELEASES_URL")
});

const RELEASE_PUBLIC_KEY_STRING: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIN62i+zbHQzRA0qSCULi9Skk8DxfYANdd73WfdyF6D48";

static RELEASE_PUBLIC_KEY: LazyLock<ssh_key::PublicKey> = LazyLock::new(|| {
    ssh_key::PublicKey::from_openssh(RELEASE_PUBLIC_KEY_STRING)
        .expect("failed to parse RELEASE_PUBLIC_KEY")
});

const RELEASE_SIGNATURE_NAMESPACE: &str = "release@brioche.dev";

#[derive(Debug, Parser)]
pub struct SelfUpdateArgs {
    /// Confirm the update without prompting
    #[arg(short = 'y', long)]
    confirm: bool,
}

#[expect(clippy::print_stdout)]
pub async fn self_update(args: SelfUpdateArgs) -> anyhow::Result<bool> {
    let current_version: semver::Version = CURRENT_VERSION
        .parse()
        .context("failed to parse current Brioche version as semver")?;
    let platform = self_update_platform();
    let client = reqwest::Client::builder()
        .user_agent(brioche_core::USER_AGENT)
        .build()?;

    // Fetch the latest version number from the server
    let latest_version = get_latest_version(&client).await?;

    if latest_version <= current_version {
        println!("Brioche v{CURRENT_VERSION} is already the latest version!");
        return Ok(false);
    }

    println!("A new version of Brioche is available: v{current_version} -> v{latest_version}");

    // Get the update manifest (to figure out if we can self-update)
    let update_manifest = get_version_self_update_manifest(&client, &latest_version).await;
    let update_manifest = match update_manifest {
        Ok(update_manifest) => update_manifest,
        Err(error) => {
            println!("Error fetching self-update manifest: {error:#}");
            println!(
                "A newer version of Brioche is available: v{current_version} -> v{latest_version}"
            );
            println!(
                "Failed to retrieve self-update manifest, please visit https://brioche.dev/help/manual-update for instructions on updating to the latest version"
            );
            return Ok(false);
        }
    };

    // Ensure that the update manifest allows us to self-update
    if !update_manifest
        .self_update_allowed
        .matches(&current_version)
    {
        println!(
            "This version of Brioche cannot be updated to v{latest_version}, please visit https://brioche.dev/help/manual-update for instructions on updating to the latest version"
        );
        return Ok(false);
    }

    // Check if self-updates are enabled
    if !cfg!(feature = "self-update") {
        println!(
            "Your current version of Brioche does not support self-updating, please visit https://brioche.dev/help/manual-update for instructions on updating to the latest version"
        );
        return Ok(false);
    }

    // Figure out where the current version is installed
    let installation_info = get_installation_info();
    let installation_info = match installation_info {
        Ok(installation_info) => installation_info,
        Err(error) => {
            println!("Error getting installation info: {error:#}");
            println!(
                "Failed to determine how the current Brioche installation is set up, please visit https://brioche.dev/help/manual-update for instructions on updating to the latest version"
            );
            return Ok(false);
        }
    };

    // Get the path where the new version should go
    let latest_version_dir_name = format!("v{latest_version}");
    let latest_version_dir = installation_info
        .install_root
        .join(&latest_version_dir_name);

    let download_basename = format!("brioche-{platform}");
    let download_filename = format!("v{latest_version}/{download_basename}.tar.xz");
    let download_url = BRIOCHE_RELEASES_URL.join(&download_filename)?;

    let signature_filename = format!("{download_filename}.sig");
    let signature_url = BRIOCHE_RELEASES_URL.join(&signature_filename)?;

    println!("Update info:");
    println!(
        "  Current Brioche installation: {}",
        installation_info.current_exe.display()
    );
    println!("  Download URL: {download_url}");
    println!(
        "  New Brioche installation dir: {}",
        latest_version_dir.display()
    );
    println!(
        "  Current version symlink: {}",
        installation_info.current_version_symlink.display()
    );
    println!();
    println!("Signature info:");
    println!("  Signature URL: {signature_url}");
    println!("  Public key: {RELEASE_PUBLIC_KEY_STRING}");
    println!("  Signature namespace: {RELEASE_SIGNATURE_NAMESPACE}");
    println!();

    if tokio::fs::try_exists(&latest_version_dir).await? {
        println!("Oh! The directory for the latest version already exists here:");
        println!("  {}", latest_version_dir.display());
        println!();
        println!("...but the current version you're running is here:");
        println!("  {}", installation_info.current_exe.display());
        println!();
        println!("If you continue with the update, the directory for the latest version will be");
        println!("deleted and re-downloaded.");
        println!();
    }

    // Prompt the user to confirm the update
    let should_update = if args.confirm {
        true
    } else {
        confirm_update().await?
    };

    if !should_update {
        println!("Cancelled update");
        return Ok(false);
    }

    println!("Downloading update...");

    // Fetch the signature for the update
    let signature = client
        .get(signature_url.clone())
        .send()
        .map_err(anyhow::Error::from)
        .and_then(async |response| {
            let signature = response.error_for_status()?.bytes().await?;
            Ok(signature)
        })
        .await
        .with_context(|| format!("failed to fetch signature from {signature_url}"))?;
    let signature = ssh_key::SshSig::from_pem(signature)
        .with_context(|| format!("failed to parse signature from {signature_url}"))?;

    // Request the update file (this is done before the file is opened so we
    // don't create an empty file if the request fails)
    let download_response = client
        .get(download_url.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("failed to download update from {download_url}"))?;
    let download_body = download_response
        .bytes_stream()
        .map_err(tokio::io::Error::other);
    let mut download_body = tokio_util::io::StreamReader::new(download_body);

    // Open a temporary file to save the response
    let id = ulid::Ulid::new();
    let download_path = installation_info
        .install_root
        .join(format!("v{latest_version}-{download_basename}-{id}.tar.xz"));
    let mut download_file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&download_path)
        .await?;

    // Unlink the file. On Unix, the file is only deleted after we close it,
    // so this essentially handles cleanup for us.
    tokio::fs::remove_file(&download_path)
        .await
        .context("failed to unlink download file")?;

    // Download the update file to the temporary file
    tokio::io::copy(&mut download_body, &mut download_file)
        .await
        .with_context(|| format!("failed while downloading update from {download_url}"))?;
    download_file.flush().await?;

    println!("Verifying update...");

    // Verify the signature against the downloaded file
    download_file
        .rewind()
        .await
        .context("failed to rewind downloaded file")?;
    let mut download_file = tokio::io::BufReader::new(download_file);
    verify_signature(
        &RELEASE_PUBLIC_KEY,
        RELEASE_SIGNATURE_NAMESPACE,
        &mut download_file,
        &signature,
    )
    .await
    .context("failed to validate signature")?;

    println!("Installing update...");

    // Rewind the file, wrap with a xz decoding stream, and pick a path
    // to unpack it
    download_file
        .rewind()
        .await
        .context("failed to rewind downloaded file")?;
    let download_archive = async_compression::tokio::bufread::XzDecoder::new(download_file);
    let new_version_temp_path = installation_info
        .install_root
        .join(format!("temp-{id}-v{latest_version}"));

    // Unpack the tar archive to the temp path
    let new_version_temp_path = tokio::task::spawn_blocking(move || {
        let download_archive = tokio_util::io::SyncIoBridge::new(download_archive);
        let mut download_archive = tar::Archive::new(download_archive);
        download_archive
            .unpack(&new_version_temp_path)
            .with_context(|| {
                format!(
                    "failed to unpack download to {}",
                    new_version_temp_path.display()
                )
            })?;
        anyhow::Ok(new_version_temp_path)
    })
    .await??;

    // If the path for the latest version exists already, move it out of the
    // way first
    let old_version_temp_path = installation_info
        .install_root
        .join(format!("old-{id}-v{latest_version}"));
    let _ = tokio::fs::rename(&latest_version_dir, &old_version_temp_path).await;

    // Move the unpacked path to its final place. The tarfile will be wrapped
    // within an extra directory, so we don't just move the top-level path.
    tokio::fs::rename(
        &new_version_temp_path.join(&download_basename),
        &latest_version_dir,
    )
    .await
    .with_context(|| {
        format!(
            "failed to move new version dir {download_basename:?} from unpacked archive to {}",
            latest_version_dir.display()
        )
    })?;

    // Replace the new symlink. We do this by creating a temp symlink then
    // renaming it, which atomically replaces the old symlink.
    let temp_symlink_path = installation_info
        .install_root
        .join(format!("temp-{id}-current"));
    tokio::fs::symlink(&latest_version_dir_name, &temp_symlink_path).await?;
    tokio::fs::rename(
        &temp_symlink_path,
        &installation_info.current_version_symlink,
    )
    .await
    .with_context(|| {
        format!(
            "failed to update 'current' symlink: {}",
            installation_info.current_version_symlink.display()
        )
    })?;

    // Try to remove some leftover temp dirs
    let _ = tokio::fs::remove_dir_all(&old_version_temp_path).await;
    let _ = tokio::fs::remove_dir_all(&new_version_temp_path).await;

    // Run post-install steps
    let post_install_result = tokio::process::Command::new(latest_version_dir.join("bin/brioche"))
        .arg("self-post-install")
        .env("BRIOCHE_SELF_POST_INSTALL_SOURCE", "self-update")
        .env(
            "BRIOCHE_SELF_POST_INSTALL_PREVIOUS_VERSION",
            CURRENT_VERSION,
        )
        .status()
        .await;

    let post_install_succeeded = match post_install_result {
        Ok(status) => status.success(),
        Err(error) => {
            println!("Error running post-install command:");
            println!("{error:#}");
            println!();
            false
        }
    };

    if !post_install_succeeded {
        println!("Warning: post-install command failed! The new version was installed,");
        println!("but may not be set up correctly. The last version of Brioche is still");
        println!("available at the following path:");
        println!();
        println!("  {}", installation_info.current_exe.display());
        println!();
        println!("Here are some suggestions:");
        println!();
        println!("- Validate to see if the new installation is working correctly");
        println!("- Check for open issues, or file a new one with details about your system:");
        println!("    https://github.com/brioche-dev/brioche/issues");
        println!("- Try manually updating: https://brioche.dev/help/manual-update");
    }

    Ok(true)
}

/// Get the platform name used for self-updating. This will either be the
/// current platform (e.g. `x86_64-linux`) or a variant controlled by
/// the `$BRIOCHE_SELF_UPDATE_PLATFORM` env var (e.g. `x86_64-linux-gnu`).
const fn self_update_platform() -> SelfUpdatePlatform {
    let platform_name = option_env!("BRIOCHE_SELF_UPDATE_PLATFORM");
    if let Some(platform_name) = platform_name {
        SelfUpdatePlatform::PlatformName(platform_name)
    } else {
        let current_platform = brioche_core::platform::current_platform();
        SelfUpdatePlatform::Platform(current_platform)
    }
}

#[derive(Debug, Clone, Copy)]
enum SelfUpdatePlatform {
    Platform(brioche_core::platform::Platform),
    PlatformName(&'static str),
}

impl std::fmt::Display for SelfUpdatePlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Platform(platform) => write!(f, "{platform}"),
            Self::PlatformName(name) => write!(f, "{name}"),
        }
    }
}

/// Get details about the current Brioche installation on the filesystem for
/// use while self-updating.
///
/// For self-updating, the current installation is expected to follow a
/// structure like this:
///
/// - `$INSTALL_ROOT/v$VERSION/bin/brioche`: Current binary
/// - `$INSTALL_ROOT/v$VERSION`: Unpacked release archive
/// - `$INSTALL_ROOT/current` -> `v$VERSION`: Current version symlink (optional)
///
/// ...where `$INSTALL_ROOT` is a directory named `brioche`-- most commonly
/// `$HOME/.local/libexec/brioche`.
fn get_installation_info() -> anyhow::Result<InstallationInfo> {
    let current_exe = std::env::current_exe().context("failed to get current executable path")?;
    let current_exe_bin_dir = current_exe
        .parent()
        .context("failed to get current executable dir")?;

    let current_exe_bin_dir_name = current_exe_bin_dir
        .file_name()
        .context("failed to get current executable dir name")?;
    anyhow::ensure!(
        current_exe_bin_dir_name.to_str() == Some("bin"),
        "expected current executable path {} to be under 'bin/' directory",
        current_exe.display()
    );

    let current_version_dir = current_exe_bin_dir
        .parent()
        .context("failed to get current version dir")?;
    let install_root = current_version_dir
        .parent()
        .context("failed to get install root")?;

    let install_root_name = install_root
        .file_name()
        .context("failed to get install root dir name")?;
    anyhow::ensure!(
        install_root_name.to_str() == Some("brioche"),
        "expected install root for {} to be a directory named 'brioche/', but got {}",
        current_exe.display(),
        install_root_name.display()
    );

    let current_version_symlink = install_root.join("current");

    Ok(InstallationInfo {
        install_root: install_root.to_path_buf(),
        current_exe,
        current_version_symlink,
    })
}

struct InstallationInfo {
    install_root: PathBuf,
    current_exe: PathBuf,
    current_version_symlink: PathBuf,
}

/// Fetch the latest (stable) Brioche version number from the release server.
async fn get_latest_version(client: &reqwest::Client) -> anyhow::Result<semver::Version> {
    let latest_version_url = BRIOCHE_RELEASES_URL.join("channels/stable/latest-version.txt")?;
    let latest_version_string = client
        .get(latest_version_url.clone())
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .and_then(async |res| Ok(res.error_for_status()?.text().await?))
        .await
        .with_context(|| format!("failed to get latest version from {latest_version_url}"))?;
    let latest_version = latest_version_string
        .trim()
        .strip_prefix('v')
        .with_context(|| {
            format!("invalid version returned from {latest_version_url}: {latest_version_string:?}")
        })?;
    let latest_version: semver::Version = latest_version.parse().with_context(|| {
        format!(
            "invalid semver version returned from {latest_version_url}: {latest_version_string:?}"
        )
    })?;

    Ok(latest_version)
}

/// Get the self-update manifest for a specific version. This is a JSON file
/// from the release server with instructions related to self-updates.
async fn get_version_self_update_manifest(
    client: &reqwest::Client,
    version: &semver::Version,
) -> anyhow::Result<SelfUpdateManifest> {
    let manifest_url = BRIOCHE_RELEASES_URL
        .join(&format!("v{version}/"))?
        .join("update.json")?;

    let response = client
        .get(manifest_url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;
    let manifest: SelfUpdateManifest = response.error_for_status()?.json().await?;

    Ok(manifest)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelfUpdateManifest {
    self_update_allowed: semver::VersionReq,
}

/// Verify a message from a reader against an OpenSSH signature using a
/// public key and a namespace.
///
/// This function is like [`ssh_key::PublicKey::verify`], but gets the message
/// from a reader so that the message doesn't need to be buffered entirely
/// in memory.
async fn verify_signature(
    public_key: &ssh_key::PublicKey,
    namespace: &str,
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    signature: &ssh_key::SshSig,
) -> anyhow::Result<()> {
    let mut hasher = SignatureHasher::new_with_algorithm(signature.hash_alg())?;

    loop {
        let data = reader.fill_buf().await?;
        if data.is_empty() {
            break;
        }

        hasher.update(data);
        let consumed = data.len();

        reader.consume(consumed);
    }

    let hash = hasher.finalize();
    public_key.verify_prehash(namespace, &hash[..], signature)?;
    Ok(())
}

enum SignatureHasher {
    Sha256(sha2::Sha256),
    Sha512(sha2::Sha512),
}

impl SignatureHasher {
    fn new_with_algorithm(algorithm: ssh_key::HashAlg) -> anyhow::Result<Self> {
        match algorithm {
            ssh_key::HashAlg::Sha256 => Ok(Self::Sha256(sha2::Sha256::default())),
            ssh_key::HashAlg::Sha512 => Ok(Self::Sha512(sha2::Sha512::default())),
            _ => {
                anyhow::bail!("unsupported SSH key algorithm: {algorithm}");
            }
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(data),
            Self::Sha512(hasher) => hasher.update(data),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            Self::Sha256(hasher) => hasher.finalize().to_vec(),
            Self::Sha512(hasher) => hasher.finalize().to_vec(),
        }
    }
}

/// Prompt the user via stdin to install the self-update.
#[expect(clippy::print_stdout)]
async fn confirm_update() -> anyhow::Result<bool> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        print!("Install update? [y/N] ");
        match std::io::stdout().flush() {
            Ok(()) => {}
            Err(error) => {
                let _ = tx.send(Err(error));
                return;
            }
        }

        let stdin = std::io::stdin();

        if !stdin.is_terminal() {
            println!();
            println!("Pass `--confirm` to install update non-interactively");
            let _ = tx.send(Ok(false));
            return;
        }

        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(_) => {}
            Err(error) => {
                let _ = tx.send(Err(error));
                return;
            }
        }

        let input = line.trim().to_lowercase();
        let confirmed = matches!(input.as_str(), "y" | "yes");
        let _ = tx.send(Ok(confirmed));
    });

    let response = rx.await??;
    Ok(response)
}
