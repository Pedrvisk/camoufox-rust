//! Camoufox version model and supported-range constraints.
//!
//! Release assets are named `camoufox-<version>-<release>-<os>.<arch>.zip`,
//! where `<version>` is the Firefox version and `<release>` is the Camoufox
//! release tag (e.g. `alpha.1`, `1.2.3`). The supported range is
//! `>= alpha.1, < 1`.

use serde::{Deserialize, Serialize};
use std::path::Path;

use camoufox_core::error::{CamoufoxError, Result};

/// Supported Camoufox version range (inclusive min, exclusive max).
pub struct Constraints;

impl Constraints {
    /// Minimum supported release.
    pub const MIN_VERSION: &'static str = "alpha.1";
    /// Maximum supported release (exclusive).
    pub const MAX_VERSION: &'static str = "1";

    /// The range rendered as `>=min, <max`.
    pub fn as_range() -> String {
        format!(">={}, <{}", Self::MIN_VERSION, Self::MAX_VERSION)
    }
}

/// A parsed `version-release` pair with comparison support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CamoufoxVersion {
    /// Camoufox release tag (e.g. `alpha.1`).
    pub release: String,
    /// Firefox version (e.g. `132.0`).
    pub version: Option<String>,
    #[serde(skip)]
    sorted_rel: Vec<i64>,
}

impl CamoufoxVersion {
    /// Builds a version from `release` and optional Firefox `version`.
    pub fn new(release: impl Into<String>, version: Option<String>) -> Self {
        let release = release.into();
        let sorted_rel = Self::sort_key(&release);
        Self {
            release,
            version,
            sorted_rel,
        }
    }

    /// Sort key for a release string: numeric parts parse to numbers, any
    /// non-numeric part contributes `char_code - 1024` (so `alpha.1` sorts
    /// below `0.0.1`). Padded to five components.
    fn sort_key(release: &str) -> Vec<i64> {
        let mut parts: Vec<i64> = Vec::new();
        for part in release.split('.') {
            let value = match part.parse::<i64>() {
                Ok(n) => n,
                Err(_) => i64::from(part.chars().next().unwrap_or('a') as u32) - 1024,
            };
            parts.push(value);
        }
        while parts.len() < 5 {
            parts.push(0);
        }
        parts
    }

    /// `version-release` rendering.
    pub fn full_string(&self) -> String {
        match &self.version {
            Some(version) => format!("{version}-{}", self.release),
            None => self.release.clone(),
        }
    }

    /// Whether this version lies within the supported range
    /// (`>= min, < max`, min inclusive).
    pub fn is_supported(&self) -> bool {
        let min = Self::new(Constraints::MIN_VERSION, None);
        let max = Self::new(Constraints::MAX_VERSION, None);
        (min.less_than(self) || min.sorted_rel == self.sorted_rel)
            && self.less_than(&max)
    }

    /// Component-wise comparison: strictly less than.
    pub fn less_than(&self, other: &Self) -> bool {
        for (a, b) in self.sorted_rel.iter().zip(other.sorted_rel.iter()) {
            if a < b {
                return true;
            }
            if a > b {
                return false;
            }
        }
        false
    }

    /// Reads `version.json` from an install root.
    pub fn from_path(path: &Path) -> Result<Self> {
        let version_path = path.join("version.json");
        if !version_path.exists() {
            return Err(CamoufoxError::FileNotFound(format!(
                "Version information not found at {}. Please run `camoufox fetch` to install.",
                version_path.display()
            )));
        }
        #[derive(Deserialize)]
        struct Raw {
            release: String,
            #[serde(default)]
            version: Option<String>,
        }
        let raw: Raw = serde_json::from_str(
            &std::fs::read_to_string(&version_path).map_err(|e| {
                CamoufoxError::Io(format!(
                    "Could not read {}: {e}",
                    version_path.display()
                ))
            })?,
        )
        .map_err(|e| {
            CamoufoxError::Json(format!(
                "Invalid version.json at {}: {e}",
                version_path.display()
            ))
        })?;
        Ok(Self::new(raw.release, raw.version))
    }

    /// Whether the version at the given install root is supported.
    pub fn is_supported_path(path: &Path) -> Result<bool> {
        Ok(Self::from_path(path)?.is_supported())
    }
}

/// Reads the installed version string (`installedVerStr`).
pub fn installed_ver_str() -> Result<String> {
    Ok(CamoufoxVersion::from_path(&crate::paths::install_dir())?.full_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(release: &str, version: Option<&str>) -> CamoufoxVersion {
        CamoufoxVersion::new(release, version.map(str::to_string))
    }

    #[test]
    fn sort_key_handles_alpha_and_numeric() {
        // alpha parts sort below zero.
        assert!(v("alpha.1", None).less_than(&v("0.0.1", None)));
        assert!(v("0.2", None).less_than(&v("0.10", None)));
        assert!(!v("0.10", None).less_than(&v("0.2", None)));
        assert!(!v("1", None).less_than(&v("1", None)));
    }

    #[test]
    fn supported_range() {
        assert!(v("alpha.1", None).is_supported());
        assert!(v("alpha.2", None).is_supported());
        assert!(v("0.9.9", Some("132.0")).is_supported());
        assert!(!v("1", None).is_supported());
        assert!(!v("1.0.1", None).is_supported());
        assert!(!v("alpha.0", None).is_supported(), "below alpha.1");
        assert_eq!(Constraints::as_range(), ">=alpha.1, <1");
    }

    #[test]
    fn full_string() {
        assert_eq!(v("0.9.9", Some("132.0")).full_string(), "132.0-0.9.9");
        assert_eq!(v("alpha.1", None).full_string(), "alpha.1");
    }

    #[test]
    fn from_path_reads_version_json() {
        let dir = tempfile::tempdir().unwrap();
        let err = CamoufoxVersion::from_path(dir.path()).unwrap_err();
        assert_eq!(err.name(), "FileNotFoundError");
        assert!(err.to_string().contains("camoufox fetch"));

        std::fs::write(
            dir.path().join("version.json"),
            r#"{"release": "0.9.9", "version": "132.0"}"#,
        )
        .unwrap();
        let version = CamoufoxVersion::from_path(dir.path()).unwrap();
        assert_eq!(version.full_string(), "132.0-0.9.9");
        assert!(version.is_supported());
    }
}
