//! Browser session: enable, contexts, pages, proxy, cookies, shutdown.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

use camoufox::builder::PreparedLaunch;
use camoufox::builder::ProxyConfig;
use camoufox_core::error::Result as CoreResult;

use crate::connection::{Connection, DEFAULT_COMMAND_TIMEOUT};
use crate::error::{JugglerError, Result};
use crate::page::JugglerPage;
use crate::protocol;

/// A running Camoufox driven through the native Juggler pipe.
pub struct JugglerBrowser {
    /// The browser process.
    pub child: tokio::process::Child,
    /// Everything a launch resolved (fingerprint, env, prefs…).
    pub prepared: PreparedLaunch,
    /// The virtual display backing the process, when one was started.
    pub virtual_display: Option<camoufox_virtdisplay::VirtualDisplay>,
    connection: Arc<Connection>,
    root_events: tokio::sync::Mutex<UnboundedReceiver<crate::protocol::Event>>,
    /// `true` after `Browser.close` was sent.
    closing: std::sync::atomic::AtomicBool,
    /// `true` when pages are created in the default (persistent) context.
    persistent: bool,
}

impl JugglerBrowser {
    /// Wraps a live connection (called by the driver).
    pub(crate) fn new(
        child: tokio::process::Child,
        prepared: PreparedLaunch,
        virtual_display: Option<camoufox_virtdisplay::VirtualDisplay>,
        connection: Arc<Connection>,
        persistent: bool,
    ) -> Self {
        let root_events = connection.subscribe(protocol::ROOT_SESSION);
        Self {
            child,
            prepared,
            virtual_display,
            connection,
            root_events: tokio::sync::Mutex::new(root_events),
            closing: std::sync::atomic::AtomicBool::new(false),
            persistent,
        }
    }

    /// The underlying connection (advanced use).
    pub fn connection(&self) -> Arc<Connection> {
        self.connection.clone()
    }

    /// `Browser.getInfo` → `(version, userAgent)`.
    pub async fn info(&self) -> Result<(String, String)> {
        let result = self
            .connection
            .send_command(
                None,
                "Browser.getInfo",
                serde_json::json!({}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        let version = result
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let user_agent = result
            .get("userAgent")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok((version, user_agent))
    }

    /// Configures the browser-level proxy (credentials supported natively —
    /// no extension needed: Juggler answers the proxy auth prompts itself).
    pub async fn set_browser_proxy(&self, proxy: &ProxyConfig) -> Result<()> {
        let parsed = parse_proxy(proxy)?;
        self.connection
            .send_command(
                None,
                "Browser.setBrowserProxy",
                protocol::proxy_options(
                    None,
                    &parsed.proxy_type,
                    &parsed.host,
                    parsed.port,
                    parsed.username.as_deref(),
                    parsed.password.as_deref(),
                    &bypass_list(proxy),
                ),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Creates a fresh page.
    ///
    /// In persistent launches the page lives in the **default** browser
    /// context, so its cookies/local storage land in the profile directory
    /// and survive restarts. Otherwise a disposable context
    /// (`removeOnDetach`) wraps the page.
    pub async fn new_page(&self) -> Result<Arc<JugglerPage>> {
        let browser_context_id = if self.persistent {
            // Default context: omit browserContextId entirely.
            None
        } else {
            let context = self
                .connection
                .send_command(
                    None,
                    "Browser.createBrowserContext",
                    serde_json::json!({"removeOnDetach": true}),
                    DEFAULT_COMMAND_TIMEOUT,
                )
                .await?;
            context
                .get("browserContextId")
                .and_then(Value::as_str)
                .map(str::to_string)
        };

        let mut new_page_params = serde_json::json!({});
        if let Some(id) = &browser_context_id {
            new_page_params["browserContextId"] = Value::String(id.clone());
        }
        let new_page = self
            .connection
            .send_command(
                None,
                "Browser.newPage",
                new_page_params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        let target_id = new_page
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or_else(|| JugglerError::Protocol("newPage returned no targetId".into()))?
            .to_string();

        // Wait for the attachedToTarget event carrying our target's session.
        let session_id = {
            let mut events = self.root_events.lock().await;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
                    if event.method == "Browser.attachedToTarget" {
                        let matches = event
                            .params
                            .pointer("/targetInfo/targetId")
                            .and_then(Value::as_str)
                            == Some(&target_id);
                        if matches {
                            break event
                                .params
                                .get("sessionId")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .ok_or_else(|| {
                                    JugglerError::Protocol(
                                        "attachedToTarget without sessionId".into(),
                                    )
                                })?;
                        }
                    }
                } else {
                    return Err(JugglerError::Timeout(format!(
                        "waiting for attachedToTarget of target {target_id}"
                    )));
                }
            }
        };

        let page_events = self.connection.subscribe(&session_id);
        Ok(JugglerPage::new(
            self.connection.clone(),
            session_id,
            target_id,
            browser_context_id,
            page_events,
        ))
    }

    /// Gracefully closes the browser and reaps the process.
    pub async fn close(&mut self) -> Result<()> {
        if self.closing.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Ok(());
        }
        // Fire-and-forget: the browser tears the pipe down before answering.
        self.connection
            .send_notification("Browser.close", serde_json::json!({}))
            .await?;
        match tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(JugglerError::Io(format!("wait: {e}"))),
            Err(_) => {
                let _ = self.child.kill().await;
            }
        }
        if let Some(display) = self.virtual_display.as_mut() {
            display.kill();
            let _ = display.wait().await;
        }
        self.connection.mark_closed();
        Ok(())
    }

    /// Kills the browser without waiting for a graceful close.
    pub async fn kill(&mut self) -> CoreResult<()> {
        self.child
            .kill()
            .await
            .map_err(|e| camoufox_core::error::CamoufoxError::Io(e.to_string()))?;
        if let Some(display) = self.virtual_display.as_mut() {
            display.kill();
            let _ = display.wait().await;
        }
        self.connection.mark_closed();
        Ok(())
    }

    /// The browser process id.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }
}

/// Parsed proxy parts for the Juggler protocol.
struct ParsedProxy {
    proxy_type: String,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
}

fn parse_proxy(proxy: &ProxyConfig) -> Result<ParsedProxy> {
    let url = proxy.server.trim();
    let (scheme, rest) = match url.split_once("://") {
        Some(parts) => parts,
        None => ("http", url),
    };
    let proxy_type = match scheme.to_ascii_lowercase().as_str() {
        "http" => "http",
        "https" => "https",
        "socks5" | "socks5h" => "socks",
        "socks4" | "socks4a" => "socks4",
        other => {
            return Err(JugglerError::Protocol(format!(
                "unsupported proxy scheme '{other}'"
            )))
        }
    }
    .to_string();
    // Strip credentials embedded in the server URL (user:pass@host:port).
    let rest = match rest.rsplit_once('@') {
        Some((_, host_part)) => host_part,
        None => rest,
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse::<u16>().map_err(|_| {
                JugglerError::Protocol(format!("invalid proxy port in '{}'", proxy.server))
            })?,
        ),
        None => {
            return Err(JugglerError::Protocol(format!(
                "proxy server '{}' has no port",
                proxy.server
            )))
        }
    };
    Ok(ParsedProxy {
        proxy_type,
        host,
        port,
        username: proxy.username.clone(),
        password: proxy.password.clone(),
    })
}

fn bypass_list(proxy: &ProxyConfig) -> Vec<String> {
    proxy
        .bypass
        .as_deref()
        .map(|bypass| {
            bypass
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proxy_servers() {
        let parsed = parse_proxy(&ProxyConfig {
            server: "http://proxy.example.com:8080".into(),
            username: Some("user".into()),
            password: Some("pass".into()),
            bypass: Some("localhost,127.0.0.1".into()),
        })
        .unwrap();
        assert_eq!(parsed.proxy_type, "http");
        assert_eq!(parsed.host, "proxy.example.com");
        assert_eq!(parsed.port, 8080);
        assert_eq!(parsed.username.as_deref(), Some("user"));
        assert_eq!(
            bypass_list(&ProxyConfig {
                server: String::new(),
                username: None,
                password: None,
                bypass: Some("localhost, 127.0.0.1,".into()),
            }),
            vec!["localhost".to_string(), "127.0.0.1".to_string()]
        );

        let parsed = parse_proxy(&ProxyConfig {
            server: "socks5://127.0.0.1:1080".into(),
            username: None,
            password: None,
            bypass: None,
        })
        .unwrap();
        assert_eq!(parsed.proxy_type, "socks");

        assert!(parse_proxy(&ProxyConfig {
            server: "http://noport.example.com".into(),
            username: None,
            password: None,
            bypass: None,
        })
        .is_err());
    }
}
