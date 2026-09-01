//! OS types used across the workspace.
//!
//! [`OsName`] is the compact three-value OS identifier used by the Camoufox
//! release assets and config (`win`/`mac`/`lin`), while [`SupportedOs`] is the
//! user-facing fingerprint constraint (`windows`/`macos`/`linux`).

use crate::error::{CamoufoxError, Result};

/// Compact OS identifier (`win` | `mac` | `lin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OsName {
    /// Windows.
    Win,
    /// macOS.
    Mac,
    /// Linux.
    Lin,
}

impl OsName {
    /// The string used in release asset names and lookup columns.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Win => "win",
            Self::Mac => "mac",
            Self::Lin => "lin",
        }
    }
}

impl std::fmt::Display for OsName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parses `"win" | "mac" | "lin"`.
pub fn os_name_from_str(value: &str) -> Option<OsName> {
    match value {
        "win" => Some(OsName::Win),
        "mac" => Some(OsName::Mac),
        "lin" => Some(OsName::Lin),
        _ => None,
    }
}

/// The host OS (`process.platform` equivalent).
///
/// Unsupported platforms fail to compile in practice.
pub fn host_os() -> OsName {
    if cfg!(windows) {
        OsName::Win
    } else if cfg!(target_os = "macos") {
        OsName::Mac
    } else {
        OsName::Lin
    }
}

/// User-facing fingerprint OS constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupportedOs {
    /// Windows.
    Windows,
    /// macOS.
    Macos,
    /// Linux.
    Linux,
}

/// Default fingerprint OS choices: `["windows", "macos", "linux"]`.
pub const SUPPORTED_OS: &[SupportedOs] =
    &[SupportedOs::Windows, SupportedOs::Macos, SupportedOs::Linux];

impl SupportedOs {
    /// The string accepted in [`crate::LaunchOptions`]-style inputs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
        }
    }

    /// The compact [`OsName`] counterpart.
    pub fn to_os_name(self) -> OsName {
        match self {
            Self::Windows => OsName::Win,
            Self::Macos => OsName::Mac,
            Self::Linux => OsName::Lin,
        }
    }

    /// The `veilus_fingerprint::OsFamily` counterpart.
    pub fn to_family(self) -> veilus_fingerprint::OsFamily {
        use veilus_fingerprint::OsFamily;
        match self {
            Self::Windows => OsFamily::Windows,
            Self::Macos => OsFamily::MacOs,
            Self::Linux => OsFamily::Linux,
        }
    }
}

/// Parses `"windows" | "macos" | "linux"`.
pub fn supported_os_from_str(value: &str) -> Option<SupportedOs> {
    match value {
        "windows" => Some(SupportedOs::Windows),
        "macos" => Some(SupportedOs::Macos),
        "linux" => Some(SupportedOs::Linux),
        _ => None,
    }
}

/// Validates the OS constraint list: every entry must be a supported OS.
pub fn validate_os(os: &[SupportedOs]) -> Result<Option<Vec<SupportedOs>>> {
    if os.is_empty() {
        return Ok(None);
    }
    for entry in os {
        if !SUPPORTED_OS.contains(entry) {
            return Err(CamoufoxError::InvalidOs(format!(
                "Camoufox does not support the OS: '{}'",
                entry.as_str()
            )));
        }
    }
    Ok(Some(os.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_os_strings() {
        assert_eq!(SupportedOs::Windows.as_str(), "windows");
        assert_eq!(SupportedOs::Macos.as_str(), "macos");
        assert_eq!(SupportedOs::Linux.as_str(), "linux");
        assert_eq!(supported_os_from_str("macos"), Some(SupportedOs::Macos));
        assert_eq!(supported_os_from_str("android"), None);
    }

    #[test]
    fn validate_os_rejects_unknown() {
        // All entries of SUPPORTED_OS validate.
        assert!(validate_os(SUPPORTED_OS).unwrap().is_some());
    }

    #[test]
    fn host_os_maps() {
        let host = host_os();
        assert!(matches!(host, OsName::Win | OsName::Mac | OsName::Lin));
    }
}
