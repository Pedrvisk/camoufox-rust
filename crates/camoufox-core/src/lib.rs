//! # camoufox-core
//!
//! Pure domain layer for browser anti-fingerprint launching with Camoufox.
//!
//! This crate contains no IO: no network, no filesystem access at runtime
//! (static data files are embedded at compile time). It hosts:
//!
//! - [`error`]: the unified error type covering the whole exception hierarchy
//! - [`os`]: host/target OS types
//! - [`env_utils`]: env-var boolean helpers (`PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD`…)
//! - [`mappings`]: BrowserForge → Camoufox config mapping, font lists, leak warnings
//! - [`fingerprint`]: fingerprint generation (via `veilus-fingerprint`) and conversion
//! - [`config`]: the `ConfigMap` with validation, seeding and `CAMOU_CONFIG_*` chunking
//! - [`locale`]: statistical locale selection from embedded CLDR territory data
//! - [`persona`]: the persisted-identity data model (personas + session snapshots)
//! - [`rotation`]: persona rotation policies (per-domain, time-based, usage-based)

pub mod config;
pub mod env_utils;
pub mod error;
pub mod fingerprint;
pub mod locale;
pub mod mappings;
pub mod os;
pub mod persona;
pub mod profile_snapshot;
pub mod rotation;

pub use config::{
    get_env_vars, is_domain_set, load_properties, merge_into, set_into, spoofs_window_dimensions,
    validate_config, ConfigMap, PropertyDef, SEED_PROPERTIES,
};
pub use error::{CamoufoxError, Result};
pub use fingerprint::{
    check_custom_fingerprint, determine_ua_os, from_browserforge_convert as from_browserforge,
    generate_fingerprint, FingerprintRequest, ScreenConstraints,
};
pub use locale::{
    get_geolocation_config, handle_locale, handle_locales, normalize_locale, Geolocation, Locale,
};
pub use os::{host_os, os_name_from_str, supported_os_from_str, OsName, SupportedOs, SUPPORTED_OS};
pub use persona::{
    PersonaCookie, PersonaLocalStorage, PersonaRecord, PersonaSummary, SessionSnapshot,
};
pub use profile_snapshot::{restore_profile, snapshot_profile, ProfileFile};
pub use rotation::{RotationContext, RotationDecision, RotationPolicy, RotationState};
