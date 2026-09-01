//! Wire protocol types (JSON messages over the NUL-delimited pipe).
//!
//! Reference: `chrome://juggler/content/protocol/Protocol.js` inside the
//! Camoufox/Firefox omni.ja. Only the subset used by this driver is typed;
//! everything else stays a raw [`serde_json::Value`].

use serde_json::Value;

/// Root-session id (commands without a `sessionId` field).
pub const ROOT_SESSION: &str = "";

/// Sentinel id Playwright uses for fire-and-forget `Browser.close`.
pub const BROWSER_CLOSE_MESSAGE_ID: i64 = -9999;

/// Builds a request frame (without the trailing NUL).
pub fn request_frame(id: i64, session_id: Option<&str>, method: &str, params: Value) -> Value {
    let mut message = serde_json::json!({
        "id": id,
        "method": method,
        "params": params,
    });
    if let Some(session) = session_id.filter(|s| !s.is_empty()) {
        message["sessionId"] = Value::String(session.to_string());
    }
    message
}

/// An incoming protocol event (`{method, params, sessionId}`).
#[derive(Debug, Clone)]
pub struct Event {
    /// Event name, e.g. `Page.navigationCommitted`.
    pub method: String,
    /// Event payload.
    pub params: Value,
    /// Session the event belongs to (root session when absent).
    pub session_id: String,
}

/// Extracts the error payload of a response, when present.
pub fn response_error(response: &Value) -> Option<String> {
    response.get("error").map(|error| {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        match error.get("data").and_then(Value::as_str) {
            Some(data) => format!("{message}\n{data}"),
            None => message.to_string(),
        }
    })
}

/// Extracts the result payload of a response (defaults to null).
pub fn response_result(response: &Value) -> Value {
    response.get("result").cloned().unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// Typed params for the commands this driver issues
// ---------------------------------------------------------------------------

/// `Browser.enable` params.
pub fn browser_enable(attach_to_default_context: bool, user_prefs: &[(String, Value)]) -> Value {
    serde_json::json!({
        "attachToDefaultContext": attach_to_default_context,
        "userPrefs": user_prefs
            .iter()
            .map(|(name, value)| serde_json::json!({"name": name, "value": value}))
            .collect::<Vec<_>>(),
    })
}

/// `Browser.setBrowserProxy` / `Browser.setContextProxy` params.
pub fn proxy_options(
    browser_context_id: Option<&str>,
    proxy_type: &str,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
    bypass: &[String],
) -> Value {
    let mut params = serde_json::json!({
        "type": proxy_type,
        "host": host,
        "port": port,
        "bypass": bypass,
    });
    if let Some(id) = browser_context_id {
        params["browserContextId"] = Value::String(id.to_string());
    }
    if let Some(username) = username {
        params["username"] = Value::String(username.to_string());
    }
    if let Some(password) = password {
        params["password"] = Value::String(password.to_string());
    }
    params
}

/// `Page.navigate` params.
pub fn navigate(frame_id: &str, url: &str, referer: Option<&str>) -> Value {
    let mut params = serde_json::json!({"frameId": frame_id, "url": url});
    if let Some(referer) = referer {
        params["referer"] = Value::String(referer.to_string());
    }
    params
}

/// `Runtime.evaluate` params.
pub fn evaluate(execution_context_id: &str, expression: &str, return_by_value: bool) -> Value {
    serde_json::json!({
        "executionContextId": execution_context_id,
        "expression": expression,
        "returnByValue": return_by_value,
    })
}

/// `Page.setViewportSize` params.
pub fn viewport_size(width: u32, height: u32) -> Value {
    serde_json::json!({"viewportSize": {"width": width, "height": height}})
}

/// `Page.screenshot` params.
pub fn screenshot(mime_type: &str, x: f64, y: f64, width: f64, height: f64) -> Value {
    serde_json::json!({
        "mimeType": mime_type,
        "clip": {"x": x, "y": y, "width": width, "height": height},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frames() {
        let frame = request_frame(7, None, "Browser.getInfo", serde_json::json!({}));
        assert_eq!(frame["id"], 7);
        assert_eq!(frame["method"], "Browser.getInfo");
        assert!(frame.get("sessionId").is_none());

        let frame = request_frame(8, Some("sess-1"), "Page.reload", serde_json::json!({}));
        assert_eq!(frame["sessionId"], "sess-1");
    }

    #[test]
    fn response_extraction() {
        let ok = serde_json::json!({"id": 1, "result": {"version": "x"}});
        assert!(response_error(&ok).is_none());
        assert_eq!(response_result(&ok)["version"], "x");

        let err = serde_json::json!({"id": 1, "error": {"message": "boom", "data": "stack"}});
        let message = response_error(&err).unwrap();
        assert!(message.contains("boom"));
        assert!(message.contains("stack"));
    }
}
