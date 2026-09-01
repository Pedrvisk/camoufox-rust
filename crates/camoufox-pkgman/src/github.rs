//! GitHub release asset discovery and downloads.

use std::time::Duration;

use camoufox_core::error::{CamoufoxError, Result};
use serde::Deserialize;

use crate::version::CamoufoxVersion;

/// Authorization headers for GitHub API requests (`GITHUB_TOKEN`).
pub fn github_authorization_headers(url: &str) -> Vec<(String, String)> {
    let Ok(token) = std::env::var("GITHUB_TOKEN") else {
        return Vec::new();
    };
    if let Some(host) = url.split('/').nth(2).map(str::to_string) {
        if host == "api.github.com" || host == "github.com" || host == "objects.githubusercontent.com" {
            return vec![("Authorization".into(), format!("Bearer {token}"))];
        }
    }
    Vec::new()
}

#[derive(Debug, Deserialize)]
pub struct Asset {
    /// Asset file name.
    pub name: String,
    /// Browser download URL.
    pub browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct Release {
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// A release asset candidate: parsed version + download URL.
pub struct ReleaseAsset {
    /// The parsed `version-release` of the asset.
    pub version: CamoufoxVersion,
    /// The browser download URL.
    pub url: String,
}

/// Fetches GitHub release data and finds a matching asset.
pub struct GitHubDownloader {
    github_repo: String,
}

impl GitHubDownloader {
    /// Creates a downloader for `owner/repo`.
    pub fn new(github_repo: impl Into<String>) -> Self {
        Self {
            github_repo: github_repo.into(),
        }
    }

    fn api_url(&self) -> String {
        format!("https://api.github.com/repos/{}/releases", self.github_repo)
    }

    /// Fetches releases and returns the first asset accepted by `check`.
    ///
    /// Skips prerelease and draft releases. Retries network failures up to
    /// `retries` times with a 5s backoff.
    pub async fn get_asset<F>(&self, check: F, retries: u32) -> Result<ReleaseAsset>
    where
        F: Fn(&Asset) -> Option<(CamoufoxVersion, String)>,
    {
        let client = reqwest::Client::builder()
            .user_agent(concat!("camoufox-rust/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| CamoufoxError::Http(e.to_string()))?;

        let mut attempts = 0;
        let releases: Vec<Release> = loop {
            let mut request = client.get(self.api_url());
            for (key, value) in github_authorization_headers(&self.api_url()) {
                request = request.header(&key, &value);
            }
            match request.send().await {
                Ok(response) if response.status().is_success() => {
                    match response.json::<Vec<Release>>().await {
                        Ok(releases) => break releases,
                        Err(e) => {
                            return Err(CamoufoxError::Json(format!(
                                "Invalid release payload from {}: {e}",
                                self.api_url()
                            )))
                        }
                    }
                }
                Ok(response) => {
                    attempts += 1;
                    if attempts >= retries {
                        return Err(CamoufoxError::Download(format!(
                            "Failed to fetch releases from {} (status {})",
                            self.api_url(),
                            response.status()
                        )));
                    }
                }
                Err(e) => {
                    attempts += 1;
                    if attempts >= retries {
                        return Err(CamoufoxError::Download(format!(
                            "Failed to fetch releases from {}: {e}",
                            self.api_url()
                        )));
                    }
                    log::warn!("retrying ({attempts}/{retries})...: {e}");
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        };

        for release in &releases {
            if release.prerelease || release.draft {
                continue;
            }
            for asset in &release.assets {
                if let Some((version, url)) = check(asset) {
                    return Ok(ReleaseAsset { version, url });
                }
            }
        }

        Err(CamoufoxError::MissingRelease(format!(
            "Could not find a release asset in {}.",
            self.github_repo
        )))
    }
}
