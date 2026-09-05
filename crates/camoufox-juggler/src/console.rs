//! Console message capture: `Runtime.console` events with typed levels.
//!
//! [`crate::page::JugglerPage::console_messages`] streams
//! [`ConsoleMessage`]s — level, rendered arguments and source location.
//! Juggler reports `warn` for some browser-internal messages; those are
//! normalized to `warning` (Playwright parity).

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

/// Console message level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleLevel {
    /// `console.log`
    Log,
    /// `console.info`
    Info,
    /// `console.warn` (Juggler's `warn`)
    Warning,
    /// `console.error`
    Error,
    /// `console.debug`
    Debug,
    /// `console.trace`
    Trace,
    /// `console.dir`
    Dir,
    /// `console.table`
    Table,
    /// `console.assert` failures
    Assert,
    /// Group start (`console.group`)
    StartGroup,
    /// Group start (`console.groupCollapsed`)
    StartGroupCollapsed,
    /// Group end (`console.groupEnd`)
    EndGroup,
    /// `console.timeEnd` output
    TimeEnd,
    /// `console.count` output
    Count,
}

impl ConsoleLevel {
    /// Maps a Juggler console type to a level.
    pub fn from_type(type_name: &str) -> Option<ConsoleLevel> {
        match type_name {
            "log" | "verbose" => Some(ConsoleLevel::Log),
            "info" => Some(ConsoleLevel::Info),
            "warn" | "warning" => Some(ConsoleLevel::Warning),
            "error" => Some(ConsoleLevel::Error),
            "debug" => Some(ConsoleLevel::Debug),
            "trace" => Some(ConsoleLevel::Trace),
            "dir" | "dirxml" => Some(ConsoleLevel::Dir),
            "table" => Some(ConsoleLevel::Table),
            "assert" => Some(ConsoleLevel::Assert),
            "startGroup" => Some(ConsoleLevel::StartGroup),
            "startGroupCollapsed" => Some(ConsoleLevel::StartGroupCollapsed),
            "endGroup" => Some(ConsoleLevel::EndGroup),
            "timeEnd" => Some(ConsoleLevel::TimeEnd),
            "count" => Some(ConsoleLevel::Count),
            _ => None,
        }
    }
}

/// One console message.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsoleMessage {
    /// Message level.
    pub level: ConsoleLevel,
    /// The raw console API type (e.g. `warn`, `timeEnd`).
    pub type_name: String,
    /// Rendered arguments (primitives by value; objects as placeholders).
    pub args: Vec<String>,
    /// Source script URL (empty for eval'd code).
    pub url: String,
    /// Source line number (0-based).
    pub line: u64,
    /// Source column number (0-based).
    pub column: u64,
    /// Execution context that logged the message.
    pub execution_context_id: String,
}

impl ConsoleMessage {
    /// The arguments joined with spaces, like the devtools render.
    pub fn text(&self) -> String {
        self.args.join(" ")
    }
}

/// Renders one remote object argument to a string.
///
/// Primitives use their `value`; unserializable values (`NaN`,
/// `Infinity`, `-0`) use `unserializableValue`; objects/functions render
/// as a `[object …]` placeholder (full previews would need extra
/// protocol round-trips).
fn render_arg(remote: &Value) -> String {
    if let Some(value) = remote.get("value") {
        return match value {
            Value::String(text) => text.clone(),
            Value::Null => "null".into(),
            other => other.to_string(),
        };
    }
    if let Some(unserializable) = remote.get("unserializableValue").and_then(Value::as_str) {
        return unserializable.to_string();
    }
    let type_name = remote
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match remote.get("subtype").and_then(Value::as_str) {
        Some("null") => "null".into(),
        Some(subtype) => format!("[{type_name} {subtype}]"),
        None => format!("[{type_name}]"),
    }
}

/// Decodes a `Runtime.console` payload.
pub(crate) fn decode_console(params: &Value) -> Option<ConsoleMessage> {
    let type_name = params.get("type")?.as_str()?;
    // Unknown console types (clear, profile, …) are dropped.
    let level = ConsoleLevel::from_type(type_name)?;
    let args = params
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().map(render_arg).collect())
        .unwrap_or_default();
    let location = params.get("location");
    Some(ConsoleMessage {
        level,
        type_name: type_name.to_string(),
        args,
        url: location
            .and_then(|location| location.get("url"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        line: location
            .and_then(|location| location.get("lineNumber"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        column: location
            .and_then(|location| location.get("columnNumber"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        execution_context_id: params
            .get("executionContextId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// The console message stream for a page session.
///
/// Obtained from [`crate::page::JugglerPage::console_messages`].
pub struct ConsoleEvents {
    messages: UnboundedReceiver<ConsoleMessage>,
}

impl ConsoleEvents {
    pub(crate) fn new(messages: UnboundedReceiver<ConsoleMessage>) -> Self {
        Self { messages }
    }

    /// Waits for the next console message.
    ///
    /// `None` means the session ended.
    pub async fn next(&mut self) -> Option<ConsoleMessage> {
        self.messages.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_console_types() {
        assert_eq!(ConsoleLevel::from_type("log"), Some(ConsoleLevel::Log));
        assert_eq!(ConsoleLevel::from_type("warn"), Some(ConsoleLevel::Warning));
        assert_eq!(
            ConsoleLevel::from_type("warning"),
            Some(ConsoleLevel::Warning)
        );
        assert_eq!(
            ConsoleLevel::from_type("timeEnd"),
            Some(ConsoleLevel::TimeEnd)
        );
        assert_eq!(ConsoleLevel::from_type("clear"), None);
    }

    #[test]
    fn decodes_console_messages() {
        let message = decode_console(&json!({
            "type": "warn",
            "executionContextId": "ctx-1",
            "args": [
                {"type": "string", "value": "careful:"},
                {"type": "number", "value": 42},
                {"type": "object", "subtype": "node", "objectId": "obj-1"},
                {"type": "number", "unserializableValue": "NaN"},
            ],
            "location": {"url": "https://example.com/app.js", "lineNumber": 7, "columnNumber": 3},
        }))
        .unwrap();
        assert_eq!(message.level, ConsoleLevel::Warning);
        assert_eq!(message.text(), "careful: 42 [object node] NaN");
        assert_eq!(message.url, "https://example.com/app.js");
        assert_eq!(message.line, 7);
        assert_eq!(message.column, 3);

        let error = decode_console(&json!({
            "type": "error",
            "executionContextId": "ctx-1",
            "args": [{"type": "string", "value": "boom"}],
            "location": {"url": "", "lineNumber": 0, "columnNumber": 0},
        }))
        .unwrap();
        assert_eq!(error.level, ConsoleLevel::Error);
        assert_eq!(error.text(), "boom");
    }

    #[test]
    fn renders_null_and_missing_args() {
        assert_eq!(
            render_arg(&json!({"type": "object", "subtype": "null"})),
            "null"
        );
        assert_eq!(render_arg(&json!({})), "[unknown]");
        let message = decode_console(&json!({
            "type": "log",
            "executionContextId": "ctx-1",
            "args": [],
        }))
        .unwrap();
        assert!(message.args.is_empty());
    }
}
