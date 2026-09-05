//! HAR (HTTP Archive) 1.2 export from captured network events.
//!
//! Feed [`HarLog`] with the [`NetworkEvent`]s observed through
//! [`crate::page::JugglerPage::network_events`]; optionally attach response
//! bodies (fetched lazily via [`crate::page::JugglerPage::response_body`])
//! and finish with [`HarLog::to_json`] / [`HarLog::write_to`].
//!
//! The output targets the [HAR 1.2 specification](http://softhints.com/har-12-spec/)
//! subset consumed by common analyzers (Chrome DevTools, har-analyzer…).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::{json, Value};

use crate::error::{JugglerError, Result};
use crate::network::{NetworkEvent, NetworkRequest, NetworkResponseInfo};

/// Monotonic wall-clock in fractional milliseconds.
fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or_default()
}

/// ISO-8601 UTC timestamp for a unix-ms value.
fn iso8601(unix_ms: f64) -> String {
    let secs = (unix_ms / 1000.0).floor() as i64;
    let millis = (unix_ms as i64).rem_euclid(1000);
    let days = secs.div_euclid(86400);
    let time = secs.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Howard Hinnant's `civil_from_days` (days since 1970-01-01 → y/m/d).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Collects network events into a HAR 1.2 document.
///
/// ```no_run
/// # async fn demo() -> camoufox_juggler::Result<()> {
/// # let page: std::sync::Arc<camoufox_juggler::JugglerPage> = todo!();
/// let mut har = camoufox_juggler::har::HarLog::new();
/// let mut events = page.network_events();
/// while let Some(event) = events.next().await? {
///     if let camoufox_juggler::NetworkEvent::ResponseReceived(response) = &event {
///         // Optionally attach the response body.
///         if let Ok(body) = page.response_body(&response.request_id).await {
///             har.attach_body(&response.request_id, body);
///         }
///     }
///     har.record(&event);
/// }
/// har.write_to(std::path::Path::new("session.har")).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct HarLog {
    entries: BTreeMap<String, HarEntry>,
    order: Vec<String>,
    pages: Vec<Value>,
    page_timings: BTreeMap<String, (f64, f64)>,
    page_for_request: BTreeMap<String, String>,
    title: String,
}

/// Mutable per-request HAR state.
#[derive(Debug, Clone, Default)]
struct HarEntry {
    started_at_ms: f64,
    request: Option<NetworkRequest>,
    response: Option<NetworkResponseInfo>,
    finished_at_ms: Option<f64>,
    transfer_size: Option<u64>,
    encoded_body_size: Option<u64>,
    body: Option<Vec<u8>>,
    error: Option<String>,
    ws_messages: Vec<WebSocketMessage>,
}

/// One WebSocket message (HAR `_webSocketMessages` extension, as Chrome
/// DevTools writes it).
#[derive(Debug, Clone)]
struct WebSocketMessage {
    opcode: u8,
    data: String,
    timestamp: f64,
    outgoing: bool,
}

impl HarLog {
    /// Creates an empty log with the given page title.
    pub fn new() -> Self {
        Self {
            title: "camoufox-rust session".into(),
            ..Default::default()
        }
    }

    /// Sets the HAR page title.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    /// Marks a page boundary: subsequent requests are grouped under a new
    /// page (useful for multi-page sessions).
    pub fn start_page(&mut self, name: &str) {
        let id = format!("page_{}", self.pages.len() + 1);
        let started = now_ms();
        self.pages.push(json!({
            "startedDateTime": iso8601(started),
            "id": id,
            "title": name,
            "pageTimings": { "onContentLoad": -1, "onLoad": -1 },
        }));
        self.page_timings.insert(id, (started, 0.0));
    }

    /// Attributes the request to the most recent page started via
    /// [`HarLog::start_page`].
    pub fn attribute_to_last_page(&mut self, request_id: &str) {
        if let Some(page) = self.pages.last() {
            let id = page["id"].as_str().unwrap_or_default().to_string();
            self.page_for_request.insert(request_id.to_string(), id);
        }
    }

    /// Records one network event.
    pub fn record(&mut self, event: &NetworkEvent) {
        match event {
            NetworkEvent::RequestWillBeSent(request) => {
                self.ensure_started(&request.request_id);
                if let Some((page, _)) = self.current_page() {
                    self.page_for_request
                        .insert(request.request_id.clone(), page);
                }
                self.entry_mut(&request.request_id).request = Some(request.clone());
            }
            NetworkEvent::ResponseReceived(response) => {
                self.ensure_started(&response.request_id);
                self.entry_mut(&response.request_id).response = Some(response.clone());
            }
            NetworkEvent::RequestFinished(finished) => {
                let entry = self.entry_mut(&finished.request_id);
                entry.finished_at_ms = Some(now_ms());
                entry.transfer_size = finished.transfer_size;
                entry.encoded_body_size = finished.encoded_body_size;
            }
            NetworkEvent::RequestFailed(failed) => {
                let entry = self.entry_mut(&failed.request_id);
                entry.finished_at_ms = Some(now_ms());
                entry.error = Some(failed.error_code.clone());
            }
            NetworkEvent::WebSocketCreated(info) => {
                self.ensure_started(&info.wsid);
                let mut request = NetworkRequest {
                    request_id: info.wsid.clone(),
                    url: info.request_url.clone(),
                    method: "GET".into(),
                    headers: HashMap::new(),
                    resource_type: "websocket".into(),
                    post_data: None,
                    frame_id: Some(info.frame_id.clone()),
                    navigation_id: None,
                    is_intercepted: false,
                };
                if let Some(effective) = &info.effective_url {
                    request.url = effective.clone();
                }
                self.entry_mut(&info.wsid).request = Some(request);
            }
            NetworkEvent::WebSocketOpened(info) => {
                self.ensure_started(&info.wsid);
                let entry = self.entry_mut(&info.wsid);
                if let Some(request) = entry.request.as_mut() {
                    if let Some(effective) = &info.effective_url {
                        request.url = effective.clone();
                    }
                }
                let response = NetworkResponseInfo {
                    request_id: info.wsid.clone(),
                    status: 101,
                    status_text: "Switching Protocols".into(),
                    headers: HashMap::new(),
                    remote_ip: None,
                    remote_port: None,
                };
                entry.response = Some(response);
            }
            NetworkEvent::WebSocketClosed(info) => {
                let entry = self.entry_mut(&info.wsid);
                entry.finished_at_ms = Some(now_ms());
                if entry.error.is_none() {
                    entry.error = info.error.clone();
                }
            }
            NetworkEvent::WebSocketFrameSent(frame) => {
                let entry = self.entry_mut(&frame.wsid);
                entry.ws_messages.push(WebSocketMessage {
                    opcode: frame.opcode,
                    data: frame.data.clone(),
                    timestamp: frame.timestamp,
                    outgoing: true,
                });
            }
            NetworkEvent::WebSocketFrameReceived(frame) => {
                let entry = self.entry_mut(&frame.wsid);
                entry.ws_messages.push(WebSocketMessage {
                    opcode: frame.opcode,
                    data: frame.data.clone(),
                    timestamp: frame.timestamp,
                    outgoing: false,
                });
            }
        }
    }

    /// Attaches a response body (from
    /// [`crate::page::JugglerPage::response_body`]) to an entry.
    pub fn attach_body(&mut self, request_id: &str, body: Vec<u8>) {
        self.entry_mut(request_id).body = Some(body);
    }

    /// Number of recorded entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn entry_mut(&mut self, request_id: &str) -> &mut HarEntry {
        self.entries.entry(request_id.to_string()).or_default()
    }

    /// Registers the entry's start time and order position (first event only).
    fn ensure_started(&mut self, request_id: &str) {
        let entry = self.entry_mut(request_id);
        if entry.started_at_ms == 0.0 {
            entry.started_at_ms = now_ms();
            self.order.push(request_id.to_string());
        }
    }

    fn current_page(&self) -> Option<(String, f64)> {
        self.pages
            .last()
            .and_then(|page| page["id"].as_str())
            .and_then(|id| self.page_timings.get(id).map(|(s, _)| (id.to_string(), *s)))
    }

    /// Renders the HAR document.
    pub fn to_json(&self) -> Result<Value> {
        let mut entries = Vec::with_capacity(self.order.len());
        for request_id in &self.order {
            let Some(entry) = self.entries.get(request_id) else {
                continue;
            };
            let page = self
                .page_for_request
                .get(request_id)
                .map(|page| Value::String(page.clone()))
                .unwrap_or(Value::Null);
            entries.push(entry.to_har(request_id, page));
        }
        Ok(json!({
            "log": {
                "version": "1.2",
                "creator": {
                    "name": "camoufox-rust",
                    "version": env!("CARGO_PKG_VERSION"),
                    "comment": "https://github.com/Pedrvisk/camoufox-rust",
                },
                "pages": self.pages,
                "entries": entries,
            }
        }))
    }

    /// Serializes the HAR document (compact).
    pub fn to_string(&self) -> Result<String> {
        Ok(serde_json::to_string(&self.to_json()?)?)
    }

    /// Serializes the HAR document (pretty-printed).
    pub fn to_string_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.to_json()?)?)
    }

    /// Writes the HAR document to `path` (pretty-printed).
    pub async fn write_to(&self, path: &Path) -> Result<()> {
        let content = self.to_string_pretty()?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| JugglerError::Io(format!("mkdir {}: {e}", parent.display())))?;
            }
        }
        tokio::fs::write(path, content)
            .await
            .map_err(|e| JugglerError::Io(format!("write {}: {e}", path.display())))?;
        Ok(())
    }
}

impl HarEntry {
    fn to_har(&self, request_id: &str, page: Value) -> Value {
        let started = if self.started_at_ms == 0.0 {
            now_ms()
        } else {
            self.started_at_ms
        };
        let total_ms = self
            .finished_at_ms
            .map(|end| (end - started).max(0.0))
            .unwrap_or(-1.0);

        let request = self.request.as_ref();
        let response = self.response.as_ref();

        let mut entry = json!({
            "pageref": page,
            "startedDateTime": iso8601(started),
            "time": total_ms,
            "_requestId": request_id,
            "request": request_to_har(request, self),
            "response": response_to_har(response, self),
            "cache": {},
            "timings": {
                "blocked": -1.0,
                "dns": -1.0,
                "connect": -1.0,
                "ssl": -1.0,
                "send": 0.0,
                "wait": total_ms,
                "receive": 0.0,
                "comment": "Juggler does not expose per-phase timings",
            },
        });

        if let Some(error) = &self.error {
            entry["_error"] = Value::String(error.clone());
        }
        if !self.ws_messages.is_empty() {
            entry["_webSocketMessages"] = Value::Array(
                self.ws_messages
                    .iter()
                    .map(|message| {
                        json!({
                            "type": if message.opcode == 1 { "send" } else { "binary" },
                            "time": message.timestamp,
                            "opcode": message.opcode,
                            "data": message.data,
                            "outgoing": message.outgoing,
                        })
                    })
                    .collect(),
            );
        }
        entry
    }
}

fn request_to_har(request: Option<&NetworkRequest>, entry: &HarEntry) -> Value {
    let Some(request) = request else {
        return json!({
            "method": "",
            "url": "",
            "httpVersion": "",
            "cookies": [],
            "headers": [],
            "queryString": [],
            "headersSize": -1,
            "bodySize": -1,
        });
    };
    let headers: Vec<Value> = request
        .headers
        .iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect();
    let query: Vec<Value> = url_query(&request.url);
    let mut har = json!({
        "method": request.method,
        "url": request.url,
        "httpVersion": "HTTP/1.1",
        "cookies": [],
        "headers": headers,
        "queryString": query,
        "headersSize": -1,
        "bodySize": body_size(request, entry),
    });
    if let Some(post_data) = &request.post_data {
        use base64::Engine;
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(post_data) {
            let text = String::from_utf8_lossy(&bytes).to_string();
            har["postData"] = json!({
                "mimeType": request.headers.get("content-type").cloned().unwrap_or_default(),
                "text": text,
            });
        }
    }
    har
}

fn response_to_har(response: Option<&NetworkResponseInfo>, entry: &HarEntry) -> Value {
    let (status, status_text, headers, remote) = match response {
        Some(response) => (
            response.status,
            response.status_text.clone(),
            &response.headers,
            (response.remote_ip.clone(), response.remote_port),
        ),
        None => (0, String::new(), &HashMap::new(), (None, None)),
    };
    let har_headers: Vec<Value> = headers
        .iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect();
    let mut content = json!({
        "size": entry.encoded_body_size.unwrap_or(0),
        "mimeType": headers.get("content-type").cloned().unwrap_or_default(),
    });
    if let Some(body) = &entry.body {
        use base64::Engine;
        let text = String::from_utf8(body.clone()).ok();
        match text {
            Some(text) => {
                content["text"] = Value::String(text);
            }
            None => {
                content["encoding"] = Value::String("base64".into());
                content["text"] =
                    Value::String(base64::engine::general_purpose::STANDARD.encode(body));
            }
        }
    }
    let mut response = json!({
        "status": status,
        "statusText": status_text,
        "httpVersion": "HTTP/1.1",
        "cookies": [],
        "headers": har_headers,
        "content": content,
        "redirectURL": "",
        "headersSize": -1,
        "bodySize": entry.transfer_size.map(|v| v as i64).unwrap_or(-1),
    });
    if let (Some(ip), Some(port)) = (&remote.0, remote.1) {
        response["serverIPAddress"] = Value::String(ip.clone());
        response["_serverPort"] = json!(port);
    }
    response
}

fn body_size(request: &NetworkRequest, entry: &HarEntry) -> i64 {
    if let Some(encoded) = entry.encoded_body_size {
        return encoded as i64;
    }
    request
        .post_data
        .as_deref()
        .and_then(|data| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .ok()
                .map(|bytes| bytes.len() as i64)
        })
        .unwrap_or(-1)
}

/// Parses a URL's query string into HAR `queryString` entries.
fn url_query(url: &str) -> Vec<Value> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) => json!({
                "name": decode_query_component(name),
                "value": decode_query_component(value),
            }),
            None => json!({
                "name": decode_query_component(pair),
                "value": "",
            }),
        })
        .collect()
}

/// Minimal percent-decoding for query components.
fn decode_query_component(component: &str) -> String {
    let bytes = component.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() + 1 && i + 2 <= bytes.len() - 1 + 1 => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|pair| std::str::from_utf8(pair).ok())
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok());
                match hex {
                    Some(byte) => {
                        decoded.push(byte);
                        i += 3;
                    }
                    None => {
                        decoded.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                decoded.push(b' ');
                i += 1;
            }
            byte => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{
        NetworkRequestFailed, NetworkRequestFinished, WebSocketFrame, WebSocketInfo,
    };

    fn request(id: &str, url: &str) -> NetworkRequest {
        NetworkRequest {
            request_id: id.into(),
            url: url.into(),
            method: "GET".into(),
            headers: HashMap::new(),
            resource_type: "document".into(),
            post_data: None,
            frame_id: None,
            navigation_id: None,
            is_intercepted: false,
        }
    }

    #[test]
    fn builds_a_minimal_har() {
        let mut har = HarLog::new();
        har.record(&NetworkEvent::RequestWillBeSent(request(
            "r1",
            "https://example.com/?a=1&b=two",
        )));
        har.record(&NetworkEvent::ResponseReceived(NetworkResponseInfo {
            request_id: "r1".into(),
            status: 200,
            status_text: "OK".into(),
            headers: HashMap::from([("content-type".into(), "text/html".into())]),
            remote_ip: Some("93.184.216.34".into()),
            remote_port: Some(443),
        }));
        har.record(&NetworkEvent::RequestFinished(NetworkRequestFinished {
            request_id: "r1".into(),
            transfer_size: Some(1250),
            encoded_body_size: Some(620),
        }));
        har.attach_body("r1", b"<html>hello</html>".to_vec());

        let doc = har.to_json().unwrap();
        let log = &doc["log"];
        assert_eq!(log["version"], "1.2");
        assert_eq!(log["creator"]["name"], "camoufox-rust");
        let entries = log["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry["request"]["method"], "GET");
        assert_eq!(entry["request"]["url"], "https://example.com/?a=1&b=two");
        assert_eq!(entry["response"]["status"], 200);
        assert_eq!(entry["response"]["content"]["text"], "<html>hello</html>");
        assert_eq!(entry["response"]["content"]["size"], 620);
        assert_eq!(entry["response"]["serverIPAddress"], "93.184.216.34");
        let query = entry["request"]["queryString"].as_array().unwrap();
        assert_eq!(query[0]["name"], "a");
        assert_eq!(query[0]["value"], "1");
        assert_eq!(query[1]["value"], "two");
        // startedDateTime parses as ISO-8601-ish.
        let started = entry["startedDateTime"].as_str().unwrap();
        assert!(started.starts_with("20") && started.ends_with('Z') && started.contains('T'));
    }

    #[test]
    fn records_failures_and_binary_bodies() {
        let mut har = HarLog::new();
        har.record(&NetworkEvent::RequestWillBeSent(request("r2", "https://x")));
        har.record(&NetworkEvent::RequestFailed(NetworkRequestFailed {
            request_id: "r2".into(),
            error_code: "NS_BINDING_ABORTED".into(),
        }));
        har.record(&NetworkEvent::RequestWillBeSent(request("r3", "https://y")));
        har.record(&NetworkEvent::ResponseReceived(NetworkResponseInfo {
            request_id: "r3".into(),
            status: 200,
            status_text: "OK".into(),
            headers: HashMap::new(),
            remote_ip: None,
            remote_port: None,
        }));
        har.attach_body("r3", vec![0u8, 159, 146, 150]);

        let doc = har.to_json().unwrap();
        let entries = doc["log"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["_error"], "NS_BINDING_ABORTED");
        // Binary bodies are base64-encoded.
        assert_eq!(entries[1]["response"]["content"]["encoding"], "base64");
    }

    #[test]
    fn records_websocket_entries() {
        let mut har = HarLog::new();
        har.record(&NetworkEvent::WebSocketCreated(WebSocketInfo {
            wsid: "ws-1".into(),
            request_url: "wss://example.com/live".into(),
            frame_id: "f1".into(),
            effective_url: None,
            error: None,
        }));
        har.record(&NetworkEvent::WebSocketOpened(WebSocketInfo {
            wsid: "ws-1".into(),
            request_url: "wss://example.com/live".into(),
            frame_id: "f1".into(),
            effective_url: Some("wss://example.com/live".into()),
            error: None,
        }));
        har.record(&NetworkEvent::WebSocketFrameSent(WebSocketFrame {
            wsid: "ws-1".into(),
            opcode: 1,
            data: "ping".into(),
            timestamp: 12.5,
            direction: "sent",
        }));
        har.record(&NetworkEvent::WebSocketFrameReceived(WebSocketFrame {
            wsid: "ws-1".into(),
            opcode: 1,
            data: "pong".into(),
            timestamp: 13.0,
            direction: "received",
        }));
        har.record(&NetworkEvent::WebSocketClosed(WebSocketInfo {
            wsid: "ws-1".into(),
            request_url: "wss://example.com/live".into(),
            frame_id: "f1".into(),
            effective_url: None,
            error: Some("1005".into()),
        }));

        let doc = har.to_json().unwrap();
        let entries = doc["log"]["entries"].as_array().unwrap();
        let entry = &entries[0];
        assert_eq!(entry["request"]["url"], "wss://example.com/live");
        assert_eq!(entry["response"]["status"], 101);
        let messages = entry["_webSocketMessages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["data"], "ping");
        assert_eq!(messages[0]["outgoing"], true);
        assert_eq!(messages[1]["outgoing"], false);
    }

    #[test]
    fn groups_entries_by_page() {
        let mut har = HarLog::new();
        har.start_page("first");
        har.record(&NetworkEvent::RequestWillBeSent(request("r1", "https://a")));
        har.start_page("second");
        har.record(&NetworkEvent::RequestWillBeSent(request("r2", "https://b")));

        let doc = har.to_json().unwrap();
        assert_eq!(doc["log"]["pages"].as_array().unwrap().len(), 2);
        let entries = doc["log"]["entries"].as_array().unwrap();
        assert_eq!(entries[0]["_requestId"], "r1");
        assert_eq!(entries[0]["pageref"], "page_1");
        assert_eq!(entries[1]["_requestId"], "r2");
        assert_eq!(entries[1]["pageref"], "page_2");
    }

    #[test]
    fn decodes_query_components() {
        assert_eq!(decode_query_component("a%20b"), "a b");
        assert_eq!(decode_query_component("a+b"), "a b");
        assert_eq!(decode_query_component("plain"), "plain");
        assert_eq!(decode_query_component("bad%zz"), "bad%zz");
    }

    #[test]
    fn iso8601_renders_known_dates() {
        assert_eq!(iso8601(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(87_122_099_000.0), "1972-10-05T08:34:59.000Z");
        // Leap-year day: 2024-02-29T00:00:00Z.
        assert_eq!(iso8601(1_709_164_800_000.0), "2024-02-29T00:00:00.000Z");
        // 2026-01-30T04:29:21Z (used by the CLI's date tests)
        assert!(iso8601(1_769_766_161_000.0).starts_with("2026-01-30T"));
    }
}
