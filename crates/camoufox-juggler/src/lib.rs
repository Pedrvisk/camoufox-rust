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
//!   pipe connected (FDs 3/4 on Unix, `PW_PIPE_READ`/`PW_PIPE_WRITE`
//!   inheritable handles on Windows)
//! - [`JugglerBrowser`] manages contexts, pages, cookies and the
//!   credentials-aware proxy configuration
//! - [`JugglerPage`] navigates, evaluates JS, screenshots, captures
//!   cookies/local storage, exposes network events and intercepts requests
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
//! ## Network events & interception
//!
//! ```no_run
//! # async fn demo() -> camoufox_juggler::Result<()> {
//! # let browser: camoufox_juggler::JugglerBrowser = todo!();
//! # let page: std::sync::Arc<camoufox_juggler::JugglerPage> = todo!();
//! // Observe traffic
//! let mut events = page.network_events();
//! page.set_request_interception(true).await?;
//! while let Some(event) = events.next().await? {
//!     if let camoufox_juggler::NetworkEvent::RequestWillBeSent(request) = event {
//!         if request.is_intercepted && request.url.contains("ads.example") {
//!             page.take_intercepted_request(&request).abort().await?;
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

pub mod browser;
pub mod connection;
pub mod driver;
pub mod error;
pub mod network;
pub mod page;
pub mod protocol;
pub mod transport;
pub mod verify;

pub use browser::JugglerBrowser;
pub use connection::{Connection, DEFAULT_COMMAND_TIMEOUT};
pub use driver::{core_error, into_core, launch_with_juggler};
pub use error::{JugglerError, Result};
pub use network::{
    FulfillResponse, InterceptedRequest, NetworkEvent, NetworkEvents, NetworkRequest,
    NetworkRequestFailed, NetworkRequestFinished, NetworkResponseInfo, RouteOverrides,
};
pub use page::{Dialog, JugglerPage};
pub use verify::{verify_fingerprint, SurfaceCheck, VerificationReport};
