use std::{
    collections::HashMap,
    io::{IsTerminal, Write as _},
    os::unix::fs::PermissionsExt,
};

use anyhow::Context as _;
use clap::Parser;
use sha2::Digest as _;

#[derive(Debug, Parser)]
pub struct SelfUpdateArgs {
    /// Confirm the update without prompting
    #[arg(short = 'y', long)]
    confirm: bool,
}

const CUSTOM_UPDATE_MANIFEST_URL: Option<&str> = option_env!("BRIOCHE_CUSTOM_UPDATE_MANIFEST_URL");

const UPDATE_MANIFEST_URL: &str = "https://releases.brioche.dev/updates/update-manifest-v1.json";

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn self_update(args: SelfUpdateArgs) -> anyhow::Result<bool> {
    let client = reqwest::Client::builder()
        .user_agent(brioche_core::USER_AGENT)
        .build()?;
    let brioche_path = std::env::current_exe().context("failed to get current executable path")?;
    let brioche_path_temp = brioche_path.with_extension(format!("{}.tmp", ulid::Ulid::new()));

    let platform = brioche_core::platform::current_platform();
    let update_manifest = get_update_platform_manifest(&client, platform).await?;

    let Some(update_manifest) = update_manifest else {
        println!("No updates available for {}", CURRENT_VERSION);
        return Ok(false);
    };

    println!("Update available");
    println!("  Version: {}", update_manifest.version);
    println!("  URL: {}", update_manifest.url);
    println!("  SHA-256: {}", update_manifest.sha256);
    println!("  Platform: {platform}");
    println!("  Path: {}", brioche_path.display());

    if !cfg!(feature = "self-update") {
        println!("Self-updating is is disabled for this version of Brioche, please visit https://brioche.dev/help/manual-update for instructions on updating to the latest version");
        return Ok(false);
    }

    let should_update = if args.confirm {
        true
    } else {
        confirm_update().await?
    };

    if !should_update {
        println!("Aborting update");
        return Ok(false);
    }

    println!("Downloading update...");

    let new_update = client
        .get(&update_manifest.url)
        .query(&[("brioche", brioche_core::VERSION)])
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let actual_hash = sha2::Sha256::digest(&new_update);
    let actual_hash_string = hex::encode(actual_hash);

    if actual_hash_string.to_lowercase() != update_manifest.sha256.to_lowercase() {
        println!("Downloaded update does not match expected hash");
        println!("  Expected: {}", update_manifest.sha256);
        println!("  Actual:   {}", actual_hash_string);
        anyhow::bail!("hash mismatch");
    }

    println!("Downloaded update");

    // Write the update to a temporary file
    tokio::fs::write(&brioche_path_temp, new_update)
        .await
        .with_context(|| format!("failed to write update to {}", brioche_path_temp.display()))?;

    // Set the executable bit if it's not already set
    let mut permissions = tokio::fs::metadata(&brioche_path_temp)
        .await
        .with_context(|| format!("failed to get metadata for {}", brioche_path_temp.display()))?
        .permissions();
    const EXECUTABLE_BIT: u32 = 0o111;
    if permissions.mode() & EXECUTABLE_BIT != EXECUTABLE_BIT {
        permissions.set_mode(permissions.mode() | EXECUTABLE_BIT);
        tokio::fs::set_permissions(&brioche_path_temp, permissions)
            .await
            .with_context(|| {
                format!(
                    "failed to set executable bit on {}",
                    brioche_path_temp.display()
                )
            })?;
    }

    // Rename the temporary file to the actual file
    tokio::fs::rename(&brioche_path_temp, &brioche_path)
        .await
        .with_context(|| {
            format!(
                "failed to rename {} to {}",
                brioche_path_temp.display(),
                brioche_path.display()
            )
        })?;

    Ok(true)
}

async fn get_update_version_manifest(
    client: &reqwest::Client,
) -> anyhow::Result<Option<SelfUpdateVersionManifest>> {
    let manifest_url = match CUSTOM_UPDATE_MANIFEST_URL {
        Some(manifest_url) => {
            println!("Checking for updates from {manifest_url}");
            manifest_url
        }
        None => UPDATE_MANIFEST_URL,
    };
    // Download the manifest
    let response = client
        .get(manifest_url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .context("failed to fetch update manifest")?;
    let mut manifest: SelfUpdateManifest = response
        .error_for_status()?
        .json()
        .await
        .context("failed to get update manifest")?;

    // Get the manifest for the current version
    let Some(version) = manifest.versions.remove(CURRENT_VERSION) else {
        return Ok(None);
    };

    // Parse the manifest
    let manifest: SelfUpdateVersionManifest =
        serde_json::from_value(version).context("failed to parse update manifest")?;
    Ok(Some(manifest))
}

async fn get_update_platform_manifest(
    client: &reqwest::Client,
    platform: brioche_core::platform::Platform,
) -> anyhow::Result<Option<SelfUpdatePlatformManifest>> {
    let update_manifest = get_update_version_manifest(client).await?;
    let Some(update_manifest) = update_manifest else {
        return Ok(None);
    };

    let platform_manifest = match update_manifest {
        SelfUpdateVersionManifest::Update { platforms, message } => {
            if let Some(message) = message {
                println!("The update server returned the following message:");
                println!("  {message:?}");
            }

            let Some(platform_manifest) = platforms.get(&platform.to_string()) else {
                println!("No update returned for platform {platform}");
                return Ok(None);
            };

            platform_manifest.clone()
        }
        SelfUpdateVersionManifest::Notice { message } => {
            if let Some(message) = message {
                println!("The update server returned the following message:");
                println!("  {message:?}");
            }

            return Ok(None);
        }
    };

    Ok(Some(platform_manifest))
}

async fn confirm_update() -> anyhow::Result<bool> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        print!("Install update? [y/N] ");
        match std::io::stdout().flush() {
            Ok(_) => {}
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelfUpdateManifest {
    versions: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum SelfUpdateVersionManifest {
    #[serde(rename_all = "camelCase")]
    Update {
        message: Option<String>,
        platforms: HashMap<String, SelfUpdatePlatformManifest>,
    },
    #[serde(rename_all = "camelCase")]
    Notice { message: Option<String> },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelfUpdatePlatformManifest {
    url: String,
    sha256: String,
    version: String,
}
