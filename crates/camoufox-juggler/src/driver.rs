//! Launching Camoufox with the native Juggler driver.

use std::sync::Arc;
use std::time::Duration;

use camoufox::builder::{prepare, HeadlessMode, LaunchOptions};
use camoufox_core::error::{CamoufoxError, Result as CoreResult};

use crate::browser::JugglerBrowser;
use crate::connection::Connection;
use crate::error::{JugglerError, Result};
use crate::transport::spawn_with_juggler_pipe;
#[cfg(unix)]
use crate::transport::wait_ready;

/// How long to wait for the pipe to become ready after spawn.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Prepares the launch, spawns the browser with the Juggler pipe and
/// initializes the protocol session.
///
/// This closes the automation loop without any Playwright dependency:
/// fingerprint/env/prefs resolution comes from [`camoufox::prepare`] and the
/// browser is driven through its native pipe.
///
/// The proxy is configured through `Browser.setBrowserProxy` (credentials
/// included — Juggler answers the auth prompts natively, so no
/// proxy-auth extension is needed on this path).
pub async fn launch_with_juggler(options: &LaunchOptions) -> Result<JugglerBrowser> {
    // HeadlessMode::Virtual → headful + Xvfb (Linux only).
    let (options, virtual_display) = camoufox::launch::resolve_headless(options).await?;
    let headless = options.headless == HeadlessMode::On;

    let prepared = prepare(&options).await?;

    // Profile: persistent (session keep-alive) or fresh temp directory.
    let profile_dir = camoufox::launch::resolve_profile_dir(options.persistent_profile.as_deref())?;
    camoufox::launch::materialize_user_js(&profile_dir, &prepared.firefox_user_prefs)?;

    let transport =
        spawn_with_juggler_pipe(&prepared, &profile_dir, headless, &options.args).await?;
    let ready = transport.ready.clone();
    #[cfg(unix)]
    let mut child = transport.child;
    #[cfg(not(unix))]
    let child = transport.child;
    let connection = Arc::new(Connection::new(transport.write, transport.read));
    #[cfg(unix)]
    wait_ready(&mut child, &ready, READY_TIMEOUT).await?;
    #[cfg(not(unix))]
    let _ = (ready, READY_TIMEOUT);

    let persistent = options.persistent_profile.is_some();
    let connection2 = connection.clone();

    // Browser.enable with the resolved user prefs (bool/number/string only —
    // the protocol rejects other types; user.js already covers the rest).
    let user_prefs: Vec<(String, serde_json::Value)> = prepared
        .firefox_user_prefs
        .iter()
        .filter(|(_, value)| value.is_boolean() || value.is_number() || value.is_string())
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    connection2
        .send_command(
            None,
            "Browser.enable",
            crate::protocol::browser_enable(persistent, &user_prefs),
            Duration::from_secs(30),
        )
        .await?;

    let browser = JugglerBrowser::new(child, prepared, virtual_display, connection, persistent);

    // Proxy with credentials: native Juggler proxy configuration.
    if let Some(proxy) = options.proxy.as_ref() {
        if proxy.username.is_some()
            || proxy.password.is_some()
            || embedded_credentials(&proxy.server)
        {
            browser.set_browser_proxy(&normalize_proxy(proxy)).await?;
        } else if !proxy.server.trim().is_empty() {
            browser
                .set_browser_proxy(&merge_embedded(proxy.clone()))
                .await?;
        }
    }

    let (version, user_agent) = browser.info().await?;
    log::info!("juggler: connected (version {version}, userAgent {user_agent})");

    Ok(browser)
}

fn embedded_credentials(server: &str) -> bool {
    let rest = server
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(server);
    rest.contains('@')
}

/// Lifts `user:pass@` embedded in the server URL into the credential fields.
fn merge_embedded(mut proxy: camoufox::builder::ProxyConfig) -> camoufox::builder::ProxyConfig {
    let rest = proxy
        .server
        .clone()
        .split_once("://")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_else(|| proxy.server.clone());
    if let Some((credentials, host)) = rest.rsplit_once('@') {
        if let Some((user, pass)) = credentials.split_once(':') {
            proxy.username = Some(user.to_string());
            proxy.password = Some(pass.to_string());
        }
        let scheme = proxy
            .server
            .split_once("://")
            .map(|(scheme, _)| format!("{scheme}://"))
            .unwrap_or_default();
        proxy.server = format!("{scheme}{host}");
    }
    proxy
}

/// Empty-credential proxies pass through untouched.
fn normalize_proxy(proxy: &camoufox::builder::ProxyConfig) -> camoufox::builder::ProxyConfig {
    if embedded_credentials(&proxy.server) {
        merge_embedded(proxy.clone())
    } else {
        proxy.clone()
    }
}

/// `Result` adapter for callers using [`CamoufoxError`].
pub fn core_error(error: JugglerError) -> CamoufoxError {
    match error {
        JugglerError::Camoufox(error) => error,
        other => CamoufoxError::Juggler(other.to_string()),
    }
}

/// `Result` adapter for juggler callers using [`CoreResult`].
pub fn into_core<T>(result: Result<T>) -> CoreResult<T> {
    result.map_err(core_error)
}
