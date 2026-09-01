//! Proxy authentication WebExtension for driverless launches.
//!
//! Firefox ignores credentials embedded in `--proxy-server`
//! (`http://user:pass@host:port`) and shows an auth prompt no headless
//! process can answer. This module provisions a small WebExtension that
//! configures the proxy through the `proxy.onRequest` API and answers the
//! proxy auth challenge through `webRequest.onAuthRequired` — the same
//! approach Playwright ships for Chromium, but Firefox-native.
//!
//! When launching through the Juggler driver
//! ([`camoufox_juggler`]) this extension is unnecessary: `Browser.setBrowserProxy`
//! takes credentials natively.

use std::path::{Path, PathBuf};

use camoufox_core::error::{CamoufoxError, Result};

use crate::builder::ProxyConfig;

/// Whether the server URL embeds `user:pass@`.
pub fn server_has_credentials(server: &str) -> bool {
    let rest = server
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(server);
    rest.contains('@')
}

/// Strips `user:pass@` from a proxy URL.
pub fn strip_credentials(server: &str) -> String {
    match server.split_once("://") {
        Some((scheme, rest)) => match rest.rsplit_once('@') {
            Some((_, host)) => format!("{scheme}://{host}"),
            None => server.to_string(),
        },
        None => server.to_string(),
    }
}

/// Parsed proxy server parts.
struct ParsedServer<'a> {
    proxy_type: &'static str,
    host: String,
    port: u16,
    credentials: Option<(&'a str, &'a str)>,
}

/// Splits a proxy server URL into type/host/port and embedded credentials.
fn parse(server: &str) -> Result<ParsedServer<'_>> {
    let (scheme, rest) = server
        .split_once("://")
        .map(|(scheme, rest)| (scheme.to_ascii_lowercase(), rest))
        .unwrap_or(("http".to_string(), server));
    let proxy_type = match scheme.as_str() {
        "http" | "https" => "http",
        "socks5" | "socks5h" => "socks",
        "socks4" | "socks4a" => "socks4",
        other => {
            return Err(CamoufoxError::InvalidProxy(format!(
                "unsupported proxy scheme '{other}'"
            )))
        }
    };
    let (credentials, host_port) = match rest.rsplit_once('@') {
        Some((credentials, host_port)) => (credentials.split_once(':'), host_port),
        None => (None, rest),
    };
    let (host, port) = host_port
        .rsplit_once(':')
        .ok_or_else(|| CamoufoxError::InvalidProxy(format!("proxy '{server}' has no port")))?;
    let port: u16 = port
        .parse()
        .map_err(|_| CamoufoxError::InvalidProxy(format!("bad port in '{server}'")))?;
    Ok(ParsedServer {
        proxy_type,
        host: host.to_string(),
        port,
        credentials,
    })
}

/// FNV-1a hash for stable extension directory names.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Writes the proxy-auth extension into `<install_dir>/addons/` and returns
/// its directory (ready to be added to the addon list).
///
/// Credentials from `ProxyConfig.username`/`password` win over credentials
/// embedded in the server URL.
pub fn provision(proxy: ProxyConfig) -> Result<PathBuf> {
    let parsed = parse(&proxy.server)?;
    let (username, password) = match (proxy.username, proxy.password) {
        (Some(username), Some(password)) => (username, password),
        _ => parsed
            .credentials
            .map(|(user, pass)| (user.to_string(), pass.to_string()))
            .unwrap_or_else(|| (String::new(), String::new())),
    };
    if username.is_empty() || password.is_empty() {
        return Err(CamoufoxError::InvalidProxy(
            "proxy authentication requires both username and password".into(),
        ));
    }

    let fingerprint = format!(
        "{}|{}|{}|{}|{}",
        parsed.proxy_type, parsed.host, parsed.port, username, password
    );
    let dir = camoufox_pkgman::install_dir()
        .join("addons")
        .join(format!("proxy-auth-{:016x}", fnv1a(fingerprint.as_bytes())));

    write_extension(
        &dir,
        parsed.proxy_type,
        &parsed.host,
        parsed.port,
        &username,
        &password,
        proxy.bypass.as_deref(),
    )?;
    Ok(dir)
}

fn write_extension(
    dir: &Path,
    proxy_type: &str,
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    bypass: Option<&str>,
) -> Result<()> {
    std::fs::create_dir_all(dir)?;

    let bypass_list: Vec<String> = bypass
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(|entry| format!("\"{}\"", entry.replace('"', "")))
                .collect()
        })
        .unwrap_or_default();

    let config = serde_json::json!({
        "type": proxy_type,
        "host": host,
        "port": port,
        "username": username,
        "password": password,
        "bypass": bypass_list,
    });

    let manifest = serde_json::json!({
        "manifest_version": 2,
        "name": "camoufox proxy auth",
        "version": "1.0",
        "description": "Proxy configuration and authentication for driverless camoufox launches",
        "permissions": ["proxy", "webRequest", "webRequestBlocking", "<all_urls>"],
        "background": {"scripts": ["background.js"]},
        "browser_specific_settings": {
            "gecko": {"id": "proxyauth@camoufox-rust"}
        }
    });

    // The config is injected as a JSON literal; escape `</script>`-style
    // sequences are irrelevant here, but keep it single-quoted safe by
    // embedding it as a JSON string and parsing at runtime.
    let background = format!(
        r#"// Generated by camoufox-rust (proxyauth). Do not edit.
const CONFIG = JSON.parse({config_json});

function shouldBypass(url) {{
  try {{
    const host = new URL(url).hostname;
    return CONFIG.bypass.some(suffix => host === suffix || host.endsWith("." + suffix));
  }} catch (e) {{
    return false;
  }}
}}

browser.proxy.onRequest.addListener(request => {{
  if (shouldBypass(request.url)) {{ return {{type: "direct"}}; }}
  return {{
    type: CONFIG.type,
    host: CONFIG.host,
    port: CONFIG.port,
  }};
}}, {{urls: ["<all_urls>"]}});

browser.webRequest.onAuthRequired.addListener((details, callback) => {{
  if (!details.isProxy) {{
    if (callback) {{ callback({{}}); }}
    return;
  }}
  const response = {{
    authCredentials: {{
      username: CONFIG.username,
      password: CONFIG.password,
    }},
  }};
  if (callback) {{ callback(response); }} else {{ return response; }}
}}, {{urls: ["<all_urls>"]}}, ["blocking"]);
"#,
        config_json = serde_json::to_string(&config)?
    );

    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    std::fs::write(dir.join("background.js"), background)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_strips_credentials() {
        assert!(server_has_credentials("http://u:p@h:1"));
        assert!(!server_has_credentials("http://h:1"));
        assert_eq!(strip_credentials("http://u:p@h:1"), "http://h:1");
        assert_eq!(strip_credentials("http://h:1"), "http://h:1");
    }

    #[test]
    fn parses_servers() {
        let parsed = parse("socks5://h:1080").unwrap();
        assert_eq!(parsed.proxy_type, "socks");
        assert_eq!(parsed.host, "h");
        assert_eq!(parsed.port, 1080);
        assert!(parsed.credentials.is_none());

        let parsed = parse("http://u:p@h:3128").unwrap();
        assert_eq!(parsed.proxy_type, "http");
        assert_eq!(parsed.host, "h");
        assert_eq!(parsed.port, 3128);
        assert_eq!(parsed.credentials, Some(("u", "p")));

        assert!(parse("http://noport").is_err());
        assert!(parse("ftp://h:1").is_err());
    }

    #[test]
    fn extension_is_written() {
        let dir = tempfile::tempdir().unwrap();
        write_extension(
            dir.path(),
            "http",
            "proxy.example.com",
            8080,
            "user",
            "pass",
            Some("localhost, 127.0.0.1"),
        )
        .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["manifest_version"], 2);
        assert_eq!(
            manifest["browser_specific_settings"]["gecko"]["id"],
            "proxyauth@camoufox-rust"
        );
        let background = std::fs::read_to_string(dir.path().join("background.js")).unwrap();
        assert!(background.contains("proxy.example.com"));
        assert!(background.contains("onAuthRequired"));
        assert!(background.contains("localhost"));
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv1a(b"abc"), fnv1a(b"abc"));
        assert_ne!(fnv1a(b"abc"), fnv1a(b"abd"));
    }
}
