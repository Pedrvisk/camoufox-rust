//! # camoufox
//!
//! Facade crate for launching the Camoufox anti-detect browser.
//!
//! The main entry points are:
//!
//! - [`LaunchOptions`]: the full launch configuration
//! - [`prepare`]: resolves everything (fingerprint, fonts, geoip, webgl, env
//!   vars, executable path) into a [`PreparedLaunch`]
//! - [`launch`]: starts the browser process directly with the prepared
//!   environment
//!
//! The [`PreparedLaunch`] is serializable and integration-friendly: hand its
//! env vars, user prefs and executable path to any Playwright driver to launch
//! through the standard automation stack.

pub mod builder;
pub mod launch;

pub use builder::{prepare, LaunchOptions, PreparedLaunch};
pub use launch::launch;
