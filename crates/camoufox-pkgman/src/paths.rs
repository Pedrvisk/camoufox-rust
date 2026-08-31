//! Install-directory layout and launch paths.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use camoufox_core::error::{CamoufoxError, Result};
use camoufox_core::os::{host_os, OsName};

/// Env var that relocates the install directory.
pub const INSTALL_DIR_ENV: &str = "CAMOUFOX_INSTALL_DIR";

static OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Overrides the install directory (test seam for
/// [`INSTALL_DIR_ENV`]).
pub fn set_install_dir(dir: Option<PathBuf>) {
    *OVERRIDE.lock().unwrap() = dir;
}

/// Per-user cache directory for an app (win/mac/lin layouts).
fn user_cache_dir(app_name: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    match host_os() {
        OsName::Win => home
            .join("AppData")
            .join("Local")
            .join(app_name)
            .join(app_name)
            .join("Cache"),
        OsName::Mac => home.join("Library").join("Caches").join(app_name),
        OsName::Lin => home.join(".cache").join(app_name),
    }
}

/// The directory the Camoufox browser is downloaded to and launched from.
///
/// Defaults to the per-user cache directory; set `CAMOUFOX_INSTALL_DIR` (or
/// [`set_install_dir`]) to relocate it — e.g. into a container image layer
/// when the home directory is ephemeral or persisted separately.
pub fn install_dir() -> PathBuf {
    if let Some(dir) = OVERRIDE.lock().unwrap().as_ref() {
        return dir.clone();
    }
    match std::env::var(INSTALL_DIR_ENV) {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value.trim()).canonicalize().unwrap_or_else(|_| PathBuf::from(value.trim())),
        _ => user_cache_dir("camoufox"),
    }
}

/// The bundled data directory (embedded assets that ship with this crate).
pub fn local_data_dir() -> PathBuf {
    // data files live in camoufox-core (embedded); this returns the crate-local
    // share dir used for additive runtime data.
    std::env::current_dir()
        .map(|p| p.join("data-files"))
        .unwrap_or_else(|_| PathBuf::from("data-files"))
}

/// OS → supported architectures matrix (from the release assets).
pub fn os_arch_matrix(os: OsName) -> &'static [&'static str] {
    match os {
        OsName::Win => &["x86_64", "i686"],
        OsName::Mac => &["x86_64", "arm64"],
        OsName::Lin => &["x86_64", "arm64", "i686"],
    }
}

/// The launch executable file name per OS.
fn launch_file(os: OsName) -> &'static str {
    match os {
        OsName::Win => "camoufox.exe",
        OsName::Mac => "../MacOS/camoufox",
        OsName::Lin => "camoufox-bin",
    }
}

/// Host architecture in release-asset naming.
pub fn platform_arch() -> Result<&'static str> {
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "i686"
    } else {
        return Err(CamoufoxError::UnsupportedArchitecture(format!(
            "Architecture {} is not supported",
            std::env::consts::ARCH
        )));
    };
    if !os_arch_matrix(host_os()).contains(&arch) {
        return Err(CamoufoxError::UnsupportedArchitecture(format!(
            "Architecture {arch} is not supported for {}",
            host_os()
        )));
    }
    Ok(arch)
}

/// Ensures the browser is installed (downloading it when missing or outdated)
/// and returns the install directory.
pub async fn camoufox_path() -> Result<PathBuf> {
    let dir = install_dir();
    let installed = dir.is_dir()
        && std::fs::read_dir(&dir)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);

    if installed {
        if crate::version::CamoufoxVersion::is_supported_path(&dir)? {
            return Ok(dir);
        }
        if camoufox_core::env_utils::skip_browser_download() {
            return Err(CamoufoxError::UnsupportedVersion(
                "Camoufox executable is outdated.".into(),
            ));
        }
    } else if camoufox_core::env_utils::skip_browser_download() {
        return Err(CamoufoxError::Io(format!(
            "Camoufox executable not found at {}",
            dir.display()
        )));
    }

    crate::install::CamoufoxFetcher::new().install().await?;
    Ok(install_dir())
}

/// Resolves a file inside the install directory (macOS bundles under
/// `Camoufox.app/Contents/Resources`).
pub async fn get_path(file: impl AsRef<Path>) -> Result<PathBuf> {
    let root = camoufox_path().await?;
    let file = file.as_ref();
    if host_os() == OsName::Mac {
        Ok(root
            .join("Camoufox.app")
            .join("Contents")
            .join("Resources")
            .join(file))
    } else {
        Ok(root.join(file))
    }
}

/// The browser executable path, verifying it exists.
pub async fn launch_path() -> Result<PathBuf> {
    let path = get_path(launch_file(host_os())).await?;
    if !path.exists() {
        return Err(CamoufoxError::CamoufoxNotInstalled(format!(
            "Camoufox is not installed at {}. Please run `camoufox fetch` to install.",
            install_dir().display()
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_cache_dir_layout() {
        let dir = user_cache_dir("camoufox");
        let rendered = dir.to_string_lossy();
        assert!(rendered.contains("camoufox"));
        assert!(dir.is_absolute() || rendered.starts_with('.'));
    }

    #[test]
    fn override_wins_over_env() {
        let tmp = tempfile::tempdir().unwrap();
        set_install_dir(Some(tmp.path().to_path_buf()));
        assert_eq!(install_dir(), tmp.path());
        set_install_dir(None);
    }

    #[test]
    fn platform_arch_is_in_matrix() {
        let arch = platform_arch().unwrap();
        assert!(os_arch_matrix(host_os()).contains(&arch));
    }
}
