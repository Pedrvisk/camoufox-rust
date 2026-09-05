//! Browser session: enable, contexts, pages, proxy, cookies, shutdown.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use camoufox::builder::PreparedLaunch;
use camoufox::builder::ProxyConfig;
use camoufox_core::error::Result as CoreResult;

use crate::connection::{Connection, DEFAULT_COMMAND_TIMEOUT};
use crate::download::{self, DownloadBehavior, DownloadEvent, DownloadEvents};
use crate::error::{JugglerError, Result};
use crate::page::JugglerPage;
use crate::protocol;

type DownloadSubscribers =
    Arc<std::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<DownloadEvent>>>>;

/// A running Camoufox driven through the native Juggler pipe.
pub struct JugglerBrowser {
    /// The browser process.
    pub child: crate::process::BrowserProcess,
    /// Everything a launch resolved (fingerprint, env, prefs…).
    pub prepared: PreparedLaunch,
    /// The virtual display backing the process, when one was started.
    pub virtual_display: Option<camoufox_virtdisplay::VirtualDisplay>,
    connection: Arc<Connection>,
    /// `Browser.attachedToTarget` events, fed by the root-event pump.
    attached_rx: tokio::sync::Mutex<UnboundedReceiver<crate::protocol::Event>>,
    /// `true` after `Browser.close` was sent.
    closing: std::sync::atomic::AtomicBool,
    /// `true` when pages are created in the default (persistent) context.
    persistent: bool,
    /// Broadcast senders for `download_events()` subscribers.
    download_broadcast: DownloadSubscribers,
}

impl JugglerBrowser {
    /// Wraps a live connection (called by the driver).
    pub(crate) fn new(
        child: crate::process::BrowserProcess,
        prepared: PreparedLaunch,
        virtual_display: Option<camoufox_virtdisplay::VirtualDisplay>,
        connection: Arc<Connection>,
        persistent: bool,
    ) -> Self {
        let mut root_events = connection.subscribe(protocol::ROOT_SESSION);
        let (attached_tx, attached_rx) = mpsc::unbounded_channel();
        let download_broadcast: DownloadSubscribers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let pump_broadcast = download_broadcast.clone();
        // Root-event pump: fans out attachedToTarget to `new_page` and
        // download events to `download_events` subscribers.
        tokio::spawn(async move {
            while let Some(event) = root_events.recv().await {
                match event.method.as_str() {
                    "Browser.attachedToTarget" => {
                        let _ = attached_tx.send(event);
                    }
                    "Browser.downloadCreated" => {
                        if let Some(created) = download::decode_download_created(&event.params) {
                            broadcast_download(&pump_broadcast, DownloadEvent::Created(created));
                        }
                    }
                    "Browser.downloadFinished" => {
                        if let Some(finished) = download::decode_download_finished(&event.params) {
                            broadcast_download(&pump_broadcast, DownloadEvent::Finished(finished));
                        }
                    }
                    _ => {}
                }
            }
        });
        Self {
            child,
            prepared,
            virtual_display,
            connection,
            attached_rx: tokio::sync::Mutex::new(attached_rx),
            closing: std::sync::atomic::AtomicBool::new(false),
            persistent,
            download_broadcast,
        }
    }

    /// Subscribes to download events (created/finished) browser-wide.
    pub fn download_events(&self) -> DownloadEvents {
        let (tx, rx) = mpsc::unbounded_channel();
        self.download_broadcast.lock().unwrap().push(tx);
        DownloadEvents::new(rx)
    }

    /// Configures what the browser does with downloads.
    ///
    /// Call *before* triggering a download: `SaveToDisk(dir)` writes files
    /// into `dir`; `Cancel` drops them. `None` resets to the browser's
    /// default behavior.
    pub async fn set_download_options(&self, behavior: Option<&DownloadBehavior>) -> Result<()> {
        self.connection
            .send_command(
                None,
                "Browser.setDownloadOptions",
                download::download_options(None, behavior),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Cancels a download by uuid, or every in-flight download when the
    /// uuid is omitted.
    pub async fn cancel_download(&self, uuid: Option<&str>) -> Result<()> {
        self.connection
            .send_command(
                None,
                "Browser.cancelDownload",
                download::cancel_download(uuid),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Toggles the touch capability of a browser context
    /// (`Browser.setTouchOverride`).
    ///
    /// `Some(true)` makes content in the context behave as a touch device
    /// (`pointer: coarse`, touch events in the DOM) — pair with
    /// [`crate::input`]'s `tap`/`touch_event` for full mobile emulation.
    /// `browser_context_id: None` targets the default context; a `None`
    /// `has_touch` clears the override.
    pub async fn set_touch_override(
        &self,
        browser_context_id: Option<&str>,
        has_touch: Option<bool>,
    ) -> Result<()> {
        self.connection
            .send_command(
                None,
                "Browser.setTouchOverride",
                crate::emulation::touch_override(browser_context_id, has_touch),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Grants permissions to an origin (`Browser.grantPermissions`).
    ///
    /// `origin` is `'*'` (every origin) or a URL prefix (e.g.
    /// `https://example.com`); pages whose URL starts with it receive the
    /// permissions. `browser_context_id: None` targets the default
    /// context.
    pub async fn grant_permissions(
        &self,
        browser_context_id: Option<&str>,
        origin: &str,
        permissions: &[crate::permission::Permission],
    ) -> Result<()> {
        self.connection
            .send_command(
                None,
                "Browser.grantPermissions",
                crate::permission::grant(browser_context_id, origin, permissions),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Resets all granted permissions (`Browser.resetPermissions`).
    pub async fn reset_permissions(&self, browser_context_id: Option<&str>) -> Result<()> {
        self.connection
            .send_command(
                None,
                "Browser.resetPermissions",
                crate::permission::reset(browser_context_id),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Sets extra HTTP headers for a whole browser context
    /// (`Browser.setExtraHTTPHeaders`).
    ///
    /// Replaces the previous context-level headers; an empty list clears
    /// them. Page-level headers come from
    /// [`crate::page::JugglerPage::set_extra_http_headers`].
    pub async fn set_extra_http_headers(
        &self,
        browser_context_id: Option<&str>,
        headers: &[(String, String)],
    ) -> Result<()> {
        let mut params = serde_json::json!({"headers": headers});
        if let Some(id) = browser_context_id {
            params["browserContextId"] = Value::String(id.to_string());
        }
        self.connection
            .send_command(
                None,
                "Browser.setExtraHTTPHeaders",
                params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Clears the browser's cache (`Browser.clearCache`).
    pub async fn clear_cache(&self) -> Result<()> {
        self.connection
            .send_command(
                None,
                "Browser.clearCache",
                serde_json::json!({}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Enables/disables the cache for a browser context
    /// (`Browser.setCacheDisabled`).
    pub async fn set_cache_disabled(
        &self,
        browser_context_id: Option<&str>,
        disabled: bool,
    ) -> Result<()> {
        let mut params = serde_json::json!({"cacheDisabled": disabled});
        if let Some(id) = browser_context_id {
            params["browserContextId"] = Value::String(id.to_string());
        }
        self.connection
            .send_command(
                None,
                "Browser.setCacheDisabled",
                params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
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
            let mut events = self.attached_rx.lock().await;
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

/// Fans a download event out to subscribers, dropping closed ones.
fn broadcast_download(subscribers: &DownloadSubscribers, event: DownloadEvent) {
    let mut senders = subscribers.lock().unwrap();
    senders.retain(|sender| {
        let _ = sender.send(event.clone());
        !sender.is_closed()
    });
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
