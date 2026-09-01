//! # camoufox-juggler
//!
//! Native Rust client for Firefox's Juggler automation protocol.
//!
//! Camoufox embeds the Juggler protocol (the Firefox automation layer
//! Playwright uses). This crate closes the automation loop with zero
//! Playwright dependency:
//!
//! - [`launch_with_juggler`] prepares the launch (fingerprint, env, prefs)
//!   through the `camoufox` facade and spawns the browser with the Juggler
//!   pipe wired to FDs 3/4
//! - [`JugglerBrowser`] manages contexts, pages, cookies and the
//!   credentials-aware proxy configuration
//! - [`JugglerPage`] navigates, evaluates JS, screenshots and captures
//!   cookies/local storage
//! - [`verify_fingerprint`] asserts the running browser's spoofed surfaces
//!   match the generated fingerprint
//!
//! ## Example
//!
//! ```no_run
//! # async fn demo() -> camoufox_juggler::Result<()> {
//! use camoufox::builder::{HeadlessMode, LaunchOptions};
//! use camoufox_core::os::SupportedOs;
//!
//! let options = LaunchOptions {
//!     os: vec![SupportedOs::Linux],
//!     headless: HeadlessMode::On,
//!     ..Default::default()
//! };
//! let mut browser = camoufox_juggler::launch_with_juggler(&options).await?;
//! let page = browser.new_page().await?;
//! page.goto("https://example.com").await?;
//! let title = page.evaluate("document.title").await?;
//! println!("title: {title:?}");
//! browser.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! The pipe transport is Unix-only for now (Windows returns
//! `JugglerError::UnsupportedOs`).

pub mod browser;
pub mod connection;
pub mod driver;
pub mod error;
pub mod page;
pub mod protocol;
pub mod transport;
pub mod verify;

pub use browser::JugglerBrowser;
pub use connection::{Connection, DEFAULT_COMMAND_TIMEOUT};
pub use driver::{core_error, into_core, launch_with_juggler};
pub use error::{JugglerError, Result};
pub use page::{Dialog, JugglerPage};
pub use verify::{verify_fingerprint, SurfaceCheck, VerificationReport};
