//! Public IP resolution through optional proxies.

use std::net::IpAddr;
use std::time::Duration;

use camoufox_core::error::{CamoufoxError, Result};

/// The endpoints tried in order to discover the public IP.
pub const PUBLIC_IP_URLS: &[&str] = &[
    "https://api.ipify.org",
    "https://checkip.amazonaws.com",
    "https://ipinfo.io/ip",
    "https://icanhazip.com",
    "https://ifconfig.co/ip",
    "https://ipecho.net/plain",
];

/// Whether the input parses as an IPv4 address.
pub fn valid_ipv4(ip: &str) -> bool {
    ip.parse::<std::net::Ipv4Addr>().is_ok()
}

/// Whether the input parses as an IPv6 address.
pub fn valid_ipv6(ip: &str) -> bool {
    ip.parse::<std::net::Ipv6Addr>().is_ok()
}

/// Validates an IP address (v4 or v6).
pub fn validate_ip(ip: &str) -> Result<()> {
    if ip.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    Err(CamoufoxError::InvalidIp(format!(
        "Invalid IP address: {ip}"
    )))
}

/// Builds a reqwest client that routes through `proxy` when given.
fn client_for(proxy: Option<&str>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(5));
    if let Some(proxy) = proxy {
        let proxy = reqwest::Proxy::all(proxy).map_err(|e| {
            CamoufoxError::InvalidProxy(format!("Invalid proxy server: {proxy} ({e})"))
        })?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|e| CamoufoxError::Http(e.to_string()))
}

/// Resolves the current public IP, optionally through a proxy.
///
/// Tries each endpoint in [`PUBLIC_IP_URLS`] in order and returns the first
/// valid response.
pub async fn public_ip(proxy: Option<&str>) -> Result<String> {
    let client = client_for(proxy)?;
    let mut errors: Vec<String> = Vec::new();

    for url in PUBLIC_IP_URLS {
        match client.get(*url).send().await {
            Ok(response) if response.status().is_success() => {
                let ip = response
                    .text()
                    .await
                    .map_err(|e| CamoufoxError::Http(e.to_string()))?
                    .trim()
                    .to_string();
                match validate_ip(&ip) {
                    Ok(()) => return Ok(ip),
                    Err(e) => errors.push(format!("{url}: {e}")),
                }
            }
            Ok(response) => errors.push(format!("{url}: status {}", response.status())),
            Err(e) => {
                if std::env::var("CAMOUFOX_DEBUG").is_ok() {
                    log::warn!("Failed to fetch public proxy IP from {url}, retrying with another URL...: {e}");
                }
                errors.push(format!("{url}: {e}"));
            }
        }
    }

    Err(CamoufoxError::InvalidIp(format!(
        "Failed to get a public proxy IP address from any API endpoint. {}",
        errors.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_validation() {
        assert!(valid_ipv4("123.45.67.89"));
        assert!(valid_ipv4("0.0.0.0"));
        assert!(valid_ipv4("255.255.255.255"));
        assert!(!valid_ipv4("256.1.1.1"));
        assert!(!valid_ipv4("abc"));
        assert!(!valid_ipv4(""));

        assert!(valid_ipv6("::1"));
        assert!(valid_ipv6("2001:db8::8a2e:370:7334"));
        assert!(!valid_ipv6("2001:db8:::"));

        assert!(validate_ip("1.2.3.4").is_ok());
        let err = validate_ip("not-an-ip").unwrap_err();
        assert_eq!(err.name(), "InvalidIP");
    }
}
