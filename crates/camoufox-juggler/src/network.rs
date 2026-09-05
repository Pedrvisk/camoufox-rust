//! Network events and request interception.
//!
//! Typed surface over the Juggler `Network.*` domain: observe requests,
//! responses, failures and timings through [`NetworkEvents`], and intercept
//! (route) requests with [`InterceptedRequest`] — continue, fulfill or
//! abort — mirroring Playwright's Firefox route API.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::connection::{Connection, DEFAULT_COMMAND_TIMEOUT};
use crate::error::{JugglerError, Result};
use crate::protocol::Event;

/// A network request observed by the browser.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkRequest {
    /// Juggler request id.
    pub request_id: String,
    /// Request URL.
    pub url: String,
    /// HTTP method.
    pub method: String,
    /// Request headers, lowercased names.
    pub headers: HashMap<String, String>,
    /// Request resource type (`document`, `script`, `xhr`, …).
    pub resource_type: String,
    /// Request body (base64) when captured.
    pub post_data: Option<String>,
    /// The frame that issued the request, when known.
    pub frame_id: Option<String>,
    /// Navigation id when the request is a navigation.
    pub navigation_id: Option<String>,
    /// True when the request is waiting for an interception decision.
    pub is_intercepted: bool,
}

/// A network response observed by the browser.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkResponseInfo {
    /// Juggler request id.
    pub request_id: String,
    /// HTTP status code.
    pub status: u16,
    /// HTTP status text.
    pub status_text: String,
    /// Response headers, lowercased names.
    pub headers: HashMap<String, String>,
    /// Remote IP, when known.
    pub remote_ip: Option<String>,
    /// Remote port, when known.
    pub remote_port: Option<u16>,
}

/// A request that finished loading.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkRequestFinished {
    /// Juggler request id.
    pub request_id: String,
    /// Transfer size in bytes, when reported.
    pub transfer_size: Option<u64>,
    /// Encoded body size in bytes, when reported.
    pub encoded_body_size: Option<u64>,
}

/// A request that failed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkRequestFailed {
    /// Juggler request id.
    pub request_id: String,
    /// Failure error code (e.g. `NS_BINDING_ABORTED`).
    pub error_code: String,
}

/// An interception decision pending for a request.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingInterception {
    /// The request awaiting a decision.
    pub request: NetworkRequest,
}

/// The network event stream for a page session.
///
/// Obtained from [`crate::page::JugglerPage::network_events`]. Every event
/// observed after subscription is delivered in order.
pub struct NetworkEvents {
    events: UnboundedReceiver<Event>,
}

impl NetworkEvents {
    pub(crate) fn new(events: UnboundedReceiver<Event>) -> Self {
        Self { events }
    }

    /// Waits for the next network event, decoding it into a typed variant.
    ///
    /// `Ok(None)` means the session ended (connection closed).
    pub async fn next(&mut self) -> Result<Option<NetworkEvent>> {
        match self.events.recv().await {
            Some(event) => Ok(decode_event(&event)),
            None => Ok(None),
        }
    }
}

/// One decoded network event.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NetworkEvent {
    /// A request was sent (optionally awaiting interception).
    RequestWillBeSent(NetworkRequest),
    /// A response was received.
    ResponseReceived(NetworkResponseInfo),
    /// A request finished successfully.
    RequestFinished(NetworkRequestFinished),
    /// A request failed.
    RequestFailed(NetworkRequestFailed),
}

/// An intercepted request that can be continued, fulfilled or aborted.
///
/// Dropping it without a decision resumes the request (Playwright parity).
pub struct InterceptedRequest {
    connection: Arc<Connection>,
    session_id: String,
    /// The request awaiting the decision.
    pub request: NetworkRequest,
    decided: std::sync::atomic::AtomicBool,
}

impl InterceptedRequest {
    pub(crate) fn new(
        connection: Arc<Connection>,
        session_id: String,
        request: NetworkRequest,
    ) -> Self {
        Self {
            connection,
            session_id,
            request,
            decided: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn request_id(&self) -> &str {
        &self.request.request_id
    }

    /// Continues the request, optionally overriding method, headers or body.
    pub async fn continue_request(&self, overrides: RouteOverrides) -> Result<()> {
        if self.decided.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Err(JugglerError::Protocol(
                "interception already decided".into(),
            ));
        }
        let mut params = serde_json::json!({"requestId": self.request_id()});
        if let Some(url) = &overrides.url {
            params["url"] = Value::String(url.clone());
        }
        if let Some(method) = &overrides.method {
            params["method"] = Value::String(method.clone());
        }
        if !overrides.headers.is_empty() {
            params["headers"] = header_array(&overrides.headers);
        }
        if let Some(post_data) = &overrides.post_data {
            use base64::Engine;
            params["postData"] = Value::String(
                base64::engine::general_purpose::STANDARD.encode(post_data),
            );
        }
        self.connection
            .send_command(
                Some(&self.session_id),
                "Network.resumeInterceptedRequest",
                params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Fulfills the request with a synthetic response.
    pub async fn fulfill(&self, response: FulfillResponse) -> Result<()> {
        if self.decided.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Err(JugglerError::Protocol(
                "interception already decided".into(),
            ));
        }
        use base64::Engine;
        let base64body = base64::engine::general_purpose::STANDARD.encode(&response.body);
        let mut params = serde_json::json!({
            "requestId": self.request_id(),
            "status": response.status,
            "statusText": status_text(response.status),
            "headers": header_array(&response.headers),
            "base64body": base64body,
        });
        if let Some(content_type) = &response.content_type {
            params["contentType"] = Value::String(content_type.clone());
        }
        self.connection
            .send_command(
                Some(&self.session_id),
                "Network.fulfillInterceptedRequest",
                params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Aborts the request.
    pub async fn abort(&self) -> Result<()> {
        self.abort_with("NS_ERROR_FAILURE").await
    }

    /// Aborts the request with a specific Firefox error code.
    pub async fn abort_with(&self, error_code: &str) -> Result<()> {
        if self.decided.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Err(JugglerError::Protocol(
                "interception already decided".into(),
            ));
        }
        self.connection
            .send_command(
                Some(&self.session_id),
                "Network.abortInterceptedRequest",
                serde_json::json!({"requestId": self.request_id(), "errorCode": error_code}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }
}

/// Overrides for [`InterceptedRequest::continue_request`].
#[derive(Debug, Clone, Default)]
pub struct RouteOverrides {
    /// Replacement URL.
    pub url: Option<String>,
    /// Replacement HTTP method.
    pub method: Option<String>,
    /// Replacement headers (replaces the whole header list).
    pub headers: Vec<(String, String)>,
    /// Replacement body (raw bytes; sent base64-encoded).
    pub post_data: Option<Vec<u8>>,
}

/// A synthetic response for [`InterceptedRequest::fulfill`].
#[derive(Debug, Clone)]
pub struct FulfillResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body (raw bytes).
    pub body: Vec<u8>,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// `Content-Type` shortcut (added to headers when set).
    pub content_type: Option<String>,
}

impl FulfillResponse {
    /// Builds a text response with the given status, Content-Type and body.
    pub fn text(status: u16, content_type: &str, body: &str) -> Self {
        Self {
            status,
            body: body.as_bytes().to_vec(),
            headers: Vec::new(),
            content_type: Some(content_type.to_string()),
        }
    }
}

/// Enables or disables request interception for a session.
pub(crate) async fn set_request_interception(
    connection: &Connection,
    session_id: &str,
    enabled: bool,
) -> Result<()> {
    connection
        .send_command(
            Some(session_id),
            "Network.setRequestInterception",
            serde_json::json!({"enabled": enabled}),
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await?;
    // Playwright disables the cache while intercepting (a cached response
    // bypasses interception entirely).
    connection
        .send_command(
            Some(session_id),
            "Page.setCacheDisabled",
            serde_json::json!({"cacheDisabled": enabled}),
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await?;
    Ok(())
}

/// Fetches a response body for a finished request.
pub(crate) async fn get_response_body(
    connection: &Connection,
    session_id: &str,
    request_id: &str,
) -> Result<Vec<u8>> {
    let result = connection
        .send_command(
            Some(session_id),
            "Network.getResponseBody",
            serde_json::json!({"requestId": request_id}),
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await?;
    if result.get("evicted").and_then(Value::as_bool).unwrap_or(false) {
        return Err(JugglerError::Protocol(
            "response body was evicted from the cache".into(),
        ));
    }
    use base64::Engine;
    let body = result
        .get("base64body")
        .and_then(Value::as_str)
        .unwrap_or_default();
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|e| JugglerError::Protocol(format!("response body base64: {e}")))
}

pub(crate) fn decode_event(event: &Event) -> Option<NetworkEvent> {
    match event.method.as_str() {
        "Network.requestWillBeSent" => {
            Some(NetworkEvent::RequestWillBeSent(decode_request(&event.params)))
        }
        "Network.responseReceived" => {
            Some(NetworkEvent::ResponseReceived(decode_response(&event.params)))
        }
        "Network.requestFinished" => {
            Some(NetworkEvent::RequestFinished(NetworkRequestFinished {
                request_id: str_field(&event.params, "requestId"),
                transfer_size: event
                    .params
                    .get("transferSize")
                    .and_then(Value::as_u64),
                encoded_body_size: event
                    .params
                    .get("encodedBodySize")
                    .and_then(Value::as_u64),
            }))
        }
        "Network.requestFailed" => Some(NetworkEvent::RequestFailed(NetworkRequestFailed {
            request_id: str_field(&event.params, "requestId"),
            error_code: str_field(&event.params, "errorCode"),
        })),
        _ => None,
    }
}

pub(crate) fn decode_request(params: &Value) -> NetworkRequest {
    let headers = event_headers(params.get("headers"));
    NetworkRequest {
        request_id: str_field(params, "requestId"),
        url: str_field(params, "url"),
        method: str_field(params, "method"),
        resource_type: resource_type(&str_field(params, "cause")),
        headers,
        post_data: params
            .get("postData")
            .and_then(Value::as_str)
            .map(str::to_string),
        frame_id: params
            .get("frameId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        navigation_id: params
            .get("navigationId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        is_intercepted: params
            .get("isIntercepted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

fn decode_response(params: &Value) -> NetworkResponseInfo {
    NetworkResponseInfo {
        request_id: str_field(params, "requestId"),
        status: params
            .get("status")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u16,
        status_text: str_field(params, "statusText"),
        headers: event_headers(params.get("headers")),
        remote_ip: params
            .get("remoteIPAddress")
            .and_then(Value::as_str)
            .map(str::to_string),
        remote_port: params.get("remotePort").and_then(Value::as_u64).map(|p| p as u16),
    }
}

/// Juggler headers are arrays of `{name, value}` pairs.
fn event_headers(value: Option<&Value>) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    if let Some(array) = value.and_then(Value::as_array) {
        for header in array {
            let name = str_field(header, "name").to_ascii_lowercase();
            let value = str_field(header, "value");
            if !name.is_empty() {
                headers.insert(name, value);
            }
        }
    }
    headers
}

fn header_array(headers: &[(String, String)]) -> Value {
    serde_json::json!(headers
        .iter()
        .map(|(name, value)| serde_json::json!({"name": name, "value": value}))
        .collect::<Vec<_>>())
}

fn str_field(params: &Value, key: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Firefox load cause → resource type (Playwright's mapping).
fn resource_type(cause: &str) -> String {
    match cause {
        "TYPE_SCRIPT" => "script",
        "TYPE_IMAGE" | "TYPE_IMAGESET" => "image",
        "TYPE_STYLESHEET" => "stylesheet",
        "TYPE_DOCUMENT" | "TYPE_REFRESH" => "document",
        "TYPE_SUBDOCUMENT" => "subdocument",
        "TYPE_XMLHTTPREQUEST" => "xhr",
        "TYPE_FETCH" => "fetch",
        "TYPE_FONT" => "font",
        "TYPE_MEDIA" => "media",
        "TYPE_WEBSOCKET" => "websocket",
        "TYPE_CSP_REPORT" => "cspreport",
        "TYPE_BEACON" => "beacon",
        "TYPE_WEB_MANIFEST" => "manifest",
        "TYPE_INTERNAL_EVENTSOURCE" => "eventsource",
        _ => "other",
    }
    .to_string()
}

/// Minimal status text for common codes (avoids an http crate dependency).
fn status_text(status: u16) -> String {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_request_events() {
        let params = json!({
            "requestId": "req-1",
            "url": "https://example.com/x",
            "method": "GET",
            "cause": "TYPE_DOCUMENT",
            "isIntercepted": true,
            "headers": [
                {"name": "Accept", "value": "text/html"},
                {"name": "User-Agent", "value": "UA"}
            ],
            "frameId": "frame-1",
        });
        let request = decode_request(&params);
        assert_eq!(request.request_id, "req-1");
        assert_eq!(request.resource_type, "document");
        assert!(request.is_intercepted);
        assert_eq!(request.headers.get("accept").map(String::as_str), Some("text/html"));
        assert_eq!(request.frame_id.as_deref(), Some("frame-1"));
        assert!(request.navigation_id.is_none());
    }

    #[test]
    fn decodes_response_events() {
        let params = json!({
            "requestId": "req-1",
            "status": 200,
            "statusText": "OK",
            "headers": [{"name": "Content-Type", "value": "text/html"}],
            "remoteIPAddress": "93.184.216.34",
            "remotePort": 443,
        });
        let response = decode_response(&params);
        assert_eq!(response.status, 200);
        assert_eq!(response.remote_port, Some(443));
        assert_eq!(
            response.headers.get("content-type").map(String::as_str),
            Some("text/html")
        );
    }

    #[test]
    fn maps_resource_types() {
        assert_eq!(resource_type("TYPE_XMLHTTPREQUEST"), "xhr");
        assert_eq!(resource_type("TYPE_INTERNAL_EVENTSOURCE"), "eventsource");
        assert_eq!(resource_type("TYPE_UNKNOWN"), "other");
    }
}
