//! Context permissions: geolocation, notifications, network access.
//!
//! Firefox's Juggler supports a fixed permission set (see `TargetRegistry`:
//! `geo`, `desktop-notification`, `local-network`, `loopback-network`).
//! Grant them per origin through
//! [`crate::browser::JugglerBrowser::grant_permissions`]; permissions
//! apply to matching pages and are stored until
//! [`crate::browser::JugglerBrowser::reset_permissions`].

use serde_json::Value;

/// A permission Firefox's Juggler can grant to an origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Permission {
    /// Geolocation access (`geo`).
    Geolocation,
    /// Desktop notifications (`desktop-notification`).
    DesktopNotification,
    /// Local network access (`local-network`).
    LocalNetwork,
    /// Loopback network access (`loopback-network`).
    LoopbackNetwork,
}

impl Permission {
    /// The Firefox permission name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Permission::Geolocation => "geo",
            Permission::DesktopNotification => "desktop-notification",
            Permission::LocalNetwork => "local-network",
            Permission::LoopbackNetwork => "loopback-network",
        }
    }
}

/// `Browser.grantPermissions` params.
///
/// `origin` is either `'*'` (every origin) or a URL prefix (e.g.
/// `https://example.com`); pages whose URL starts with it get the
/// permissions.
pub(crate) fn grant(
    browser_context_id: Option<&str>,
    origin: &str,
    permissions: &[Permission],
) -> Value {
    let mut params = serde_json::json!({
        "origin": origin,
        "permissions": permissions.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
    });
    if let Some(id) = browser_context_id {
        params["browserContextId"] = Value::String(id.to_string());
    }
    params
}

/// `Browser.resetPermissions` params.
pub(crate) fn reset(browser_context_id: Option<&str>) -> Value {
    match browser_context_id {
        Some(id) => serde_json::json!({"browserContextId": id}),
        None => serde_json::json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_permission_names() {
        assert_eq!(Permission::Geolocation.as_str(), "geo");
        assert_eq!(
            Permission::DesktopNotification.as_str(),
            "desktop-notification"
        );
        assert_eq!(Permission::LocalNetwork.as_str(), "local-network");
        assert_eq!(Permission::LoopbackNetwork.as_str(), "loopback-network");
    }

    #[test]
    fn builds_grant_params() {
        let params = grant(
            Some("ctx-1"),
            "https://example.com",
            &[Permission::Geolocation, Permission::DesktopNotification],
        );
        assert_eq!(params["browserContextId"], "ctx-1");
        assert_eq!(params["origin"], "https://example.com");
        assert_eq!(
            params["permissions"],
            serde_json::json!(["geo", "desktop-notification"])
        );

        let params = grant(None, "*", &[]);
        assert!(params.get("browserContextId").is_none());
        assert_eq!(params["permissions"], serde_json::json!([]));
    }

    #[test]
    fn builds_reset_params() {
        let params = reset(Some("ctx-1"));
        assert_eq!(params["browserContextId"], "ctx-1");
        let params = reset(None);
        assert!(params.as_object().unwrap().is_empty());
    }
}
