//! Unified error type covering the full exception hierarchy of the Camoufox
//! launcher, plus infrastructure errors.
//!
//! The original hierarchy (`UnknownTerritory` ⊂ `InvalidLocale` ⊂ `LocaleError`,
//! etc.) is expressed through predicate methods
//! ([`CamoufoxError::is_locale_error`], [`CamoufoxError::is_virtual_display_error`])
//! and the [`CamoufoxError::name`] accessor.

use thiserror::Error;

/// Convenience alias used across the workspace.
pub type Result<T> = std::result::Result<T, CamoufoxError>;

/// Every error raised by camoufox-rust.
#[derive(Debug, Error)]
pub enum CamoufoxError {
    /// `UnsupportedVersion` — the installed Camoufox binary is outside the supported range.
    #[error("{0}")]
    UnsupportedVersion(String),
    /// `MissingRelease` — a required GitHub release asset is missing.
    #[error("{0}")]
    MissingRelease(String),
    /// `UnsupportedArchitecture` — the host architecture is not supported.
    #[error("{0}")]
    UnsupportedArchitecture(String),
    /// `UnsupportedOS` — the host OS is not supported.
    #[error("{0}")]
    UnsupportedOs(String),
    /// `UnknownProperty` — a config key is not declared in the browser's `properties.json`.
    #[error("{0}")]
    UnknownProperty(String),
    /// `InvalidPropertyType` — a config value does not match the declared type.
    #[error("{0}")]
    InvalidPropertyType(String),
    /// `InvalidAddonPath` — an addon path is not a directory containing `manifest.json`.
    #[error("{0}")]
    InvalidAddonPath(String),
    /// `InvalidDebugPort` — invalid debug port specification.
    #[error("{0}")]
    InvalidDebugPort(String),
    /// `MissingDebugPort` — required debug port not provided.
    #[error("{0}")]
    MissingDebugPort(String),
    /// `LocaleError` — base class of the locale error family.
    #[error("{0}")]
    LocaleError(String),
    /// `InvalidIP` — an IP address is invalid.
    #[error("{0}")]
    InvalidIp(String),
    /// `InvalidProxy` — a proxy definition is invalid.
    #[error("{0}")]
    InvalidProxy(String),
    /// `UnknownIPLocation` — the geolocation of an IP could not be resolved.
    #[error("{0}")]
    UnknownIpLocation(String),
    /// `InvalidLocale` — the locale input is invalid.
    #[error("{0}")]
    InvalidLocale(String),
    /// `UnknownTerritory` — the territory is unknown.
    #[error("{0}")]
    UnknownTerritory(String),
    /// `UnknownLanguage` — the language is unknown.
    #[error("{0}")]
    UnknownLanguage(String),
    /// `NotInstalledGeoIPExtra` — parity with the Python extra; always enabled here.
    #[error("{0}")]
    NotInstalledGeoIpExtra(String),
    /// `NonFirefoxFingerprint` — a passed fingerprint is not a Firefox one.
    #[error("{0}")]
    NonFirefoxFingerprint(String),
    /// `InvalidOS` — the target OS constraint is invalid.
    #[error("{0}")]
    InvalidOs(String),
    /// `VirtualDisplayError` — base class of the virtual display error family.
    #[error("{0}")]
    VirtualDisplayError(String),
    /// `CannotFindXvfb` — Xvfb is not installed.
    #[error("{0}")]
    CannotFindXvfb(String),
    /// `CannotExecuteXvfb` — Xvfb cannot be executed.
    #[error("{0}")]
    CannotExecuteXvfb(String),
    /// `VirtualDisplayNotSupported` — virtual displays are Linux-only.
    #[error("{0}")]
    VirtualDisplayNotSupported(String),
    /// `CamoufoxNotInstalled` — the Camoufox browser is not installed.
    #[error("{0}")]
    CamoufoxNotInstalled(String),
    /// `FileNotFoundError` — a required file was not found.
    #[error("{0}")]
    FileNotFound(String),
    /// A fingerprint could not be generated.
    #[error("fingerprint generation failed: {0}")]
    Fingerprint(String),
    /// Network/HTTP failure.
    #[error("http error: {0}")]
    Http(String),
    /// Download failure after retries.
    #[error("download failed: {0}")]
    Download(String),
    /// Zip archive failure.
    #[error("zip error: {0}")]
    Zip(String),
    /// SQLite failure (WebGL database).
    #[error("sqlite error: {0}")]
    Sqlite(String),
    /// MaxMind database failure.
    #[error("maxmind error: {0}")]
    MaxMind(String),
    /// XML parsing failure (territory info).
    #[error("xml error: {0}")]
    Xml(String),
    /// JSON serialization/deserialization failure.
    #[error("json error: {0}")]
    Json(String),
    /// Generic IO failure.
    #[error("io error: {0}")]
    Io(String),
}

impl From<std::io::Error> for CamoufoxError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for CamoufoxError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value.to_string())
    }
}

impl CamoufoxError {
    /// Stable error name, used by tests and diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Self::UnsupportedVersion(_) => "UnsupportedVersion",
            Self::MissingRelease(_) => "MissingRelease",
            Self::UnsupportedArchitecture(_) => "UnsupportedArchitecture",
            Self::UnsupportedOs(_) => "UnsupportedOS",
            Self::UnknownProperty(_) => "UnknownProperty",
            Self::InvalidPropertyType(_) => "InvalidPropertyType",
            Self::InvalidAddonPath(_) => "InvalidAddonPath",
            Self::InvalidDebugPort(_) => "InvalidDebugPort",
            Self::MissingDebugPort(_) => "MissingDebugPort",
            Self::LocaleError(_) => "LocaleError",
            Self::InvalidIp(_) => "InvalidIP",
            Self::InvalidProxy(_) => "InvalidProxy",
            Self::UnknownIpLocation(_) => "UnknownIPLocation",
            Self::InvalidLocale(_) => "InvalidLocale",
            Self::UnknownTerritory(_) => "UnknownTerritory",
            Self::UnknownLanguage(_) => "UnknownLanguage",
            Self::NotInstalledGeoIpExtra(_) => "NotInstalledGeoIPExtra",
            Self::NonFirefoxFingerprint(_) => "NonFirefoxFingerprint",
            Self::InvalidOs(_) => "InvalidOS",
            Self::VirtualDisplayError(_) => "VirtualDisplayError",
            Self::CannotFindXvfb(_) => "CannotFindXvfb",
            Self::CannotExecuteXvfb(_) => "CannotExecuteXvfb",
            Self::VirtualDisplayNotSupported(_) => "VirtualDisplayNotSupported",
            Self::CamoufoxNotInstalled(_) => "CamoufoxNotInstalled",
            Self::FileNotFound(_) => "FileNotFoundError",
            Self::Fingerprint(_) => "FingerprintError",
            Self::Http(_) => "HttpError",
            Self::Download(_) => "DownloadError",
            Self::Zip(_) => "ZipError",
            Self::Sqlite(_) => "SqliteError",
            Self::MaxMind(_) => "MaxMindError",
            Self::Xml(_) => "XmlError",
            Self::Json(_) => "JsonError",
            Self::Io(_) => "IoError",
        }
    }

    /// Whether this error belongs to the `LocaleError` family
    /// (`LocaleError`, `InvalidLocale`, `UnknownTerritory`, `UnknownLanguage`,
    /// `UnknownIPLocation`).
    pub fn is_locale_error(&self) -> bool {
        matches!(
            self,
            Self::LocaleError(_)
                | Self::InvalidLocale(_)
                | Self::UnknownTerritory(_)
                | Self::UnknownLanguage(_)
                | Self::UnknownIpLocation(_)
        )
    }

    /// Whether this error belongs to the `VirtualDisplayError` family.
    pub fn is_virtual_display_error(&self) -> bool {
        matches!(
            self,
            Self::VirtualDisplayError(_)
                | Self::CannotFindXvfb(_)
                | Self::CannotExecuteXvfb(_)
                | Self::VirtualDisplayNotSupported(_)
        )
    }

    // -- Constructors with the default messages -----------------------------------

    pub fn unsupported_version() -> Self {
        Self::UnsupportedVersion("The Camoufox executable is outdated.".into())
    }

    pub fn missing_release() -> Self {
        Self::MissingRelease("A required GitHub release asset is missing.".into())
    }

    pub fn unsupported_architecture() -> Self {
        Self::UnsupportedArchitecture("The architecture is not supported.".into())
    }

    pub fn unsupported_os() -> Self {
        Self::UnsupportedOs("The OS is not supported.".into())
    }

    pub fn unknown_property() -> Self {
        Self::UnknownProperty("The property is unknown.".into())
    }

    pub fn invalid_property_type() -> Self {
        Self::InvalidPropertyType("The property type is invalid.".into())
    }

    pub fn invalid_addon_path() -> Self {
        Self::InvalidAddonPath("The addon path is invalid.".into())
    }

    pub fn invalid_debug_port() -> Self {
        Self::InvalidDebugPort("The debug port is invalid.".into())
    }

    pub fn missing_debug_port() -> Self {
        Self::MissingDebugPort("The debug port is missing.".into())
    }

    pub fn invalid_ip() -> Self {
        Self::InvalidIp("An IP address is invalid.".into())
    }

    pub fn invalid_proxy() -> Self {
        Self::InvalidProxy("A proxy is invalid.".into())
    }

    pub fn unknown_ip_location() -> Self {
        Self::UnknownIpLocation("The location of an IP is unknown.".into())
    }

    /// `UnknownTerritory` with a custom message.
    pub fn unknown_territory_msg(msg: impl Into<String>) -> Self {
        Self::UnknownTerritory(msg.into())
    }

    /// `UnknownLanguage` with a custom message.
    pub fn unknown_language_msg(msg: impl Into<String>) -> Self {
        Self::UnknownLanguage(msg.into())
    }

    pub fn not_installed_geoip_extra() -> Self {
        Self::NotInstalledGeoIpExtra("The geoip2 module is not installed.".into())
    }

    pub fn non_firefox_fingerprint() -> Self {
        Self::NonFirefoxFingerprint("A passed Browserforge fingerprint is invalid.".into())
    }

    pub fn invalid_os() -> Self {
        Self::InvalidOs("The target OS is invalid.".into())
    }

    pub fn cannot_find_xvfb() -> Self {
        Self::CannotFindXvfb("Xvfb cannot be found.".into())
    }

    pub fn cannot_execute_xvfb() -> Self {
        Self::CannotExecuteXvfb("Xvfb cannot be executed.".into())
    }

    pub fn virtual_display_not_supported() -> Self {
        Self::VirtualDisplayNotSupported(
            "The user tried to use a virtual display on a non-Linux OS.".into(),
        )
    }

    pub fn camoufox_not_installed() -> Self {
        Self::CamoufoxNotInstalled("Camoufox is not installed.".into())
    }

    pub fn file_not_found() -> Self {
        Self::FileNotFound("File couldn't be found.".into())
    }

    /// Builds the invalid-locale error for a bad user input.
    pub fn invalid_locale_input(locale: &str) -> Self {
        Self::InvalidLocale(format!(
            "Invalid locale: '{locale}'. Must be either a region, language, language-region, \
             or language-script-region."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_messages_are_non_empty() {
        assert!(!CamoufoxError::unsupported_version().to_string().is_empty());
        assert!(!CamoufoxError::missing_release().to_string().is_empty());
        assert!(!CamoufoxError::unsupported_os().to_string().is_empty());
        assert!(!CamoufoxError::invalid_os().to_string().is_empty());
        assert!(!CamoufoxError::camoufox_not_installed()
            .to_string()
            .is_empty());
    }

    #[test]
    fn custom_messages_win() {
        let msg = "custom error message";
        assert_eq!(
            CamoufoxError::UnsupportedVersion(msg.into()).to_string(),
            msg
        );
        assert_eq!(CamoufoxError::MissingRelease(msg.into()).to_string(), msg);
        assert_eq!(CamoufoxError::InvalidOs(msg.into()).to_string(), msg);
    }

    #[test]
    fn error_names_match_js() {
        assert_eq!(
            CamoufoxError::unsupported_version().name(),
            "UnsupportedVersion"
        );
        assert_eq!(
            CamoufoxError::invalid_locale_input("x").name(),
            "InvalidLocale"
        );
        assert_eq!(
            CamoufoxError::unknown_territory_msg("x").name(),
            "UnknownTerritory"
        );
        assert_eq!(
            CamoufoxError::virtual_display_not_supported().name(),
            "VirtualDisplayNotSupported"
        );
        assert_eq!(
            CamoufoxError::camoufox_not_installed().name(),
            "CamoufoxNotInstalled"
        );
    }

    #[test]
    fn locale_hierarchy() {
        assert!(CamoufoxError::invalid_locale_input("xyz").is_locale_error());
        assert!(CamoufoxError::unknown_territory_msg("xyz").is_locale_error());
        assert!(CamoufoxError::unknown_language_msg("xyz").is_locale_error());
        assert!(CamoufoxError::unknown_ip_location().is_locale_error());
        assert!(!CamoufoxError::unsupported_version().is_locale_error());
    }

    #[test]
    fn virtual_display_hierarchy() {
        assert!(CamoufoxError::cannot_find_xvfb().is_virtual_display_error());
        assert!(CamoufoxError::cannot_execute_xvfb().is_virtual_display_error());
        assert!(CamoufoxError::virtual_display_not_supported().is_virtual_display_error());
        assert!(!CamoufoxError::unsupported_os().is_virtual_display_error());
    }

    #[test]
    fn invalid_locale_input_message() {
        let err = CamoufoxError::invalid_locale_input("xyz");
        assert!(err.to_string().contains("xyz"));
        assert!(err.to_string().contains("Invalid locale"));
    }
}
