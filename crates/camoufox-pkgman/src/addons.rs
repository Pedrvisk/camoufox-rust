//! Default addon provisioning (uBlock Origin) and addon path validation.

use std::collections::HashSet;
use std::path::Path;

use camoufox_core::error::{CamoufoxError, Result};

/// Default addons that get downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefaultAddon {
    /// uBlock Origin.
    Ubo,
}

impl DefaultAddon {
    /// The download URL.
    pub fn url(self) -> &'static str {
        match self {
            Self::Ubo => {
                "https://addons.mozilla.org/firefox/downloads/latest/ublock-origin/latest.xpi"
            }
        }
    }

    /// The on-disk directory name.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Ubo => "UBO",
        }
    }
}

/// All default addons.
pub const DEFAULT_ADDONS: &[DefaultAddon] = &[DefaultAddon::Ubo];

/// Validates that every path in `paths` is a directory containing
/// `manifest.json`.
pub fn confirm_paths(paths: &[String]) -> Result<()> {
    for path in paths {
        let path = Path::new(path);
        if !path.exists() || !path.is_dir() {
            return Err(CamoufoxError::InvalidAddonPath(path.display().to_string()));
        }
        if !path.join("manifest.json").exists() {
            return Err(CamoufoxError::InvalidAddonPath(
                "manifest.json is missing. Addon path must be a path to an extracted addon.".into(),
            ));
        }
    }
    Ok(())
}

/// Appends the default addons (minus `exclude`) to `addons_list`.
pub async fn add_default_addons(
    addons_list: &mut Vec<String>,
    exclude: &[DefaultAddon],
) -> Result<()> {
    let mut addons: Vec<(DefaultAddon, &'static str)> = DEFAULT_ADDONS
        .iter()
        .filter(|addon| !exclude.contains(addon))
        .map(|addon| (*addon, addon.url()))
        .collect();
    // Deterministic order.
    addons.sort_by_key(|(addon, _)| addon.dir_name());
    let addons: Vec<(DefaultAddon, String)> = addons
        .into_iter()
        .map(|(addon, url)| (addon, url.to_string()))
        .collect();
    maybe_download_addons(addons, addons_list).await
}

/// Downloads and extracts addons, skipping ones already on disk.
///
/// A failed download removes the empty directory it created so the next retry
/// does not mistake it for a successfully downloaded addon.
pub async fn maybe_download_addons(
    addons: Vec<(DefaultAddon, String)>,
    addons_list: &mut Vec<String>,
) -> Result<()> {
    if camoufox_core::env_utils::skip_browser_download() {
        log::info!("Skipping addon download due to PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD set!");
        return Ok(());
    }

    for (addon, url) in addons {
        let addon_path = crate::paths::get_path(format!("addons/{}", addon.dir_name())).await?;

        if addon_path.exists() {
            addons_list.push(addon_path.to_string_lossy().into_owned());
            continue;
        }

        std::fs::create_dir_all(&addon_path)?;
        match download_and_extract(&url, &addon_path, addon.dir_name()).await {
            Ok(()) => {
                addons_list.push(addon_path.to_string_lossy().into_owned());
            }
            Err(e) => {
                log::error!("Failed to download and extract {}: {e}", addon.dir_name());
                let _ = std::fs::remove_dir_all(&addon_path);
            }
        }
    }
    Ok(())
}

/// Downloads an addon zip and extracts it into `extract_path`.
pub async fn download_and_extract(
    url: &str,
    extract_path: &std::path::Path,
    name: &str,
) -> Result<()> {
    let buffer =
        crate::install::webdl(url, &format!("Downloading addon ({name})"), false, None, 5).await?;
    crate::install::extract_zip(&buffer, extract_path)
}

/// Unique-ifies an addon list (dedup, order-preserving).
#[allow(dead_code)]
pub fn dedup_addons(addons: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    addons
        .iter()
        .filter(|a| seen.insert((*a).clone()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_paths_requires_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let err = confirm_paths(&[dir.path().to_string_lossy().into_owned()]).unwrap_err();
        assert_eq!(err.name(), "InvalidAddonPath");
        assert!(err.to_string().contains("manifest.json"));

        std::fs::write(dir.path().join("manifest.json"), "{}").unwrap();
        assert!(confirm_paths(&[dir.path().to_string_lossy().into_owned()]).is_ok());

        let missing = dir.path().join("nope");
        let err = confirm_paths(&[missing.to_string_lossy().into_owned()]).unwrap_err();
        assert_eq!(err.name(), "InvalidAddonPath");
    }

    #[test]
    fn dedup_preserves_order() {
        let deduped = dedup_addons(&["a".into(), "b".into(), "a".into(), "c".into()]);
        assert_eq!(deduped, vec!["a", "b", "c"]);
    }

    #[test]
    fn default_addon_urls() {
        assert!(DefaultAddon::Ubo.url().contains("ublock-origin"));
        assert_eq!(DefaultAddon::Ubo.dir_name(), "UBO");
    }
}
