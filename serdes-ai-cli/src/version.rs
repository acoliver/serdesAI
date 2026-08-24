//! Version checking against remote

use anyhow::Result;
use serde::Deserialize;
use tracing::{debug, info};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const VERSION_CHECK_URL: &str = "https://api.github.com/repos/yourorg/serdes-ai/releases/latest";

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    body: String,
}

/// Check for updates
pub async fn check_for_updates() -> Result<VersionInfo> {
    debug!("Checking for updates...");

    let client = reqwest::Client::builder()
        .user_agent("serdes-ai-cli")
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let response = client.get(VERSION_CHECK_URL).send().await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let release: GitHubRelease = resp.json().await?;
            let latest_version = release.tag_name.trim_start_matches('v').to_string();
            let current = parse_version(CURRENT_VERSION);
            let latest = parse_version(&latest_version);

            let update_available = latest > current;

            info!(
                "Version check: current={}, latest={}, update_available={}",
                CURRENT_VERSION, latest_version, update_available
            );

            Ok(VersionInfo {
                current_version: CURRENT_VERSION.to_string(),
                latest_version: Some(latest_version),
                update_available,
                release_notes: Some(release.body),
                download_url: Some(release.html_url),
            })
        }
        _ => {
            // Silently fail - don't bother user if check fails
            Ok(VersionInfo {
                current_version: CURRENT_VERSION.to_string(),
                latest_version: None,
                update_available: false,
                release_notes: None,
                download_url: None,
            })
        }
    }
}

/// Print version info
pub fn print_version_info(info: &VersionInfo) {
    if info.update_available {
        println!("\n📦 Update available!");
        println!("   Current: {}", info.current_version);
        println!(
            "   Latest:  {}",
            info.latest_version.as_deref().unwrap_or("unknown")
        );
        if let Some(url) = &info.download_url {
            println!("   Download: {}", url);
        }
        println!();
    }
}

fn parse_version(version: &str) -> (u32, u32, u32) {
    let parts: Vec<u32> = version
        .split('.')
        .take(3)
        .map(|s| s.parse().unwrap_or(0))
        .collect();

    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub release_notes: Option<String>,
    pub download_url: Option<String>,
}
