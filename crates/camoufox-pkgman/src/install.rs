//! Browser download, extraction and installation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use camoufox_core::error::{CamoufoxError, Result};
use camoufox_core::os::{host_os, OsName};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};

use crate::github::{github_authorization_headers, GitHubDownloader};
use crate::paths::{install_dir, platform_arch};
use crate::version::{CamoufoxVersion, Constraints};

/// Downloads `url` with retries and a progress bar, streaming into `sink`.
///
/// Returns the bytes when no sink is given.
pub async fn webdl(
    url: &str,
    desc: &str,
    show_progress: bool,
    mut sink: Option<&mut (dyn Write + Send)>,
    retries: u32,
) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("camoufox-rust/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| CamoufoxError::Http(e.to_string()))?;

    let mut attempts = 0;
    let response = loop {
        let mut request = client.get(url);
        for (key, value) in github_authorization_headers(url) {
            request = request.header(&key, &value);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => break response,
            Ok(response) => {
                attempts += 1;
                if attempts >= retries {
                    return Err(CamoufoxError::Download(format!(
                        "Failed to download from {url} (status {})",
                        response.status()
                    )));
                }
            }
            Err(e) => {
                attempts += 1;
                if attempts >= retries {
                    return Err(CamoufoxError::Download(format!(
                        "Failed to download from {url}: {e}"
                    )));
                }
                log::warn!("download retrying ({attempts}/{retries})...: {e}");
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    };

    let total_size = response.content_length().unwrap_or(0);
    let progress = if show_progress && total_size > 0 {
        let bar = ProgressBar::new(total_size);
        bar.set_style(
            ProgressStyle::with_template(
                "{msg} [{bar:40}] {percent}% | ETA: {eta_precise} | {bytes}/{total_bytes}",
            )
            .unwrap()
            .progress_chars("=>-"),
        );
        bar.set_message(desc.to_string());
        Some(bar)
    } else {
        None
    };

    let mut buffer: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| CamoufoxError::Download(format!("stream error: {e}")))?;
        buffer.extend_from_slice(&chunk);
        if let Some(sink) = sink.as_deref_mut() {
            sink.write_all(&chunk)
                .map_err(|e| CamoufoxError::Io(e.to_string()))?;
        }
        if let Some(bar) = &progress {
            bar.inc(chunk.len() as u64);
        }
    }
    if let Some(bar) = &progress {
        bar.finish();
    }
    Ok(buffer)
}

/// Extracts a zip archive into `dest`.
pub fn extract_zip(zip_file: &[u8], dest: &Path) -> Result<()> {
    let cursor = std::io::Cursor::new(zip_file);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| CamoufoxError::Zip(e.to_string()))?;
    archive
        .extract(dest)
        .map_err(|e| CamoufoxError::Zip(e.to_string()))?;
    Ok(())
}

/// Downloads and installs the Camoufox browser from GitHub releases.
pub struct CamoufoxFetcher;

impl Default for CamoufoxFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl CamoufoxFetcher {
    /// The upstream browser repository.
    pub const REPO: &'static str = "daijro/camoufox";

    /// Creates a fetcher for the current platform.
    pub fn new() -> Self {
        Self
    }

    /// Finds the latest supported release asset for the current platform.
    pub async fn latest_asset(&self) -> Result<(CamoufoxVersion, String)> {
        let os = host_os();
        let arch = platform_arch()?;
        let pattern = regex::Regex::new(&format!(
            "camoufox-(.+)-(.+)-{}\\.{}\\.zip",
            os.as_str(),
            arch
        ))
        .map_err(|e| CamoufoxError::Io(e.to_string()))?;

        let downloader = GitHubDownloader::new(Self::REPO);
        let asset = downloader
            .get_asset(
                |asset| {
                    let captures = pattern.captures(&asset.name)?;
                    let version = CamoufoxVersion::new(
                        captures.get(2)?.as_str().to_string(),
                        Some(captures.get(1)?.as_str().to_string()),
                    );
                    if !version.is_supported() {
                        return None;
                    }
                    Some((version, asset.browser_download_url.clone()))
                },
                5,
            )
            .await
            .map_err(|e| match e {
                CamoufoxError::MissingRelease(msg) => CamoufoxError::MissingRelease(format!(
                    "No matching release found for {os} {arch} in the supported range: ({}). \
                     Please update the library. ({msg})",
                    Constraints::as_range()
                )),
                other => other,
            })?;
        Ok((asset.version, asset.url))
    }

    /// Removes the install directory when present.
    pub fn cleanup() -> Result<bool> {
        let dir = install_dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| {
                CamoufoxError::Io(format!("could not remove {}: {e}", dir.display()))
            })?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Downloads and installs the browser, writing `version.json`.
    ///
    /// The download is staged in a temp directory that is always torn down,
    /// even on failure.
    pub async fn install(&self) -> Result<()> {
        let (version, url) = self.latest_asset().await?;
        Self::cleanup().ok();

        let staging = tempfile::Builder::new()
            .prefix("camoufox-")
            .tempdir()
            .map_err(|e| CamoufoxError::Io(e.to_string()))?;
        let zip_path = staging.path().join("camoufox.zip");

        let install_dir = install_dir();
        std::fs::create_dir_all(&install_dir)?;

        let result: Result<()> = async {
            let mut file =
                std::fs::File::create(&zip_path).map_err(|e| CamoufoxError::Io(e.to_string()))?;
            webdl(&url, "Downloading Camoufox...", true, Some(&mut file), 5).await?;
            drop(file);

            let bytes = std::fs::read(&zip_path)?;
            extract_zip(&bytes, &install_dir)?;

            let version_json = serde_json::json!({
                "version": version.version,
                "release": version.release,
            });
            std::fs::write(
                install_dir.join("version.json"),
                serde_json::to_string_pretty(&version_json)?,
            )?;

            if host_os() != OsName::Win {
                make_executable(&install_dir)?;
            }
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                log::info!("Camoufox successfully installed.");
                Ok(())
            }
            Err(e) => {
                let _ = Self::cleanup();
                Err(e)
            }
        }
    }
}

/// `chmod -R 755` equivalent (POSIX only).
#[cfg(unix)]
fn make_executable(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry.map_err(|e| CamoufoxError::Io(e.to_string()))?;
        let path: PathBuf = entry.path().to_path_buf();
        let mut perms = std::fs::metadata(&path)
            .map_err(|e| CamoufoxError::Io(e.to_string()))?
            .permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(&path, perms).map_err(|e| CamoufoxError::Io(e.to_string()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Installs the browser and returns the install directory.
pub async fn install() -> Result<PathBuf> {
    CamoufoxFetcher::new().install().await?;
    Ok(install_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_pattern_matches() {
        let pattern = regex::Regex::new("camoufox-(.+)-(.+)-lin\\.x86_64\\.zip").unwrap();
        let captures = pattern
            .captures("camoufox-132.0-0.9.9-lin.x86_64.zip")
            .unwrap();
        assert_eq!(captures.get(1).unwrap().as_str(), "132.0");
        assert_eq!(captures.get(2).unwrap().as_str(), "0.9.9");
        assert!(pattern
            .captures("camoufox-132.0-0.9.9-win.x86_64.zip")
            .is_none());
    }

    #[test]
    fn extract_zip_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();

        let mut buffer = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        writer.start_file("hello.txt", options).unwrap();
        writer.write_all(b"hello").unwrap();
        writer.finish().unwrap();

        extract_zip(buffer.get_ref(), &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("hello.txt")).unwrap(),
            "hello"
        );
    }
}
