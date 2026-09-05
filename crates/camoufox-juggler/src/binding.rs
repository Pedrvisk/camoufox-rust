//! Page bindings: Rust-callable functions exposed into the page.
//!
//! [`crate::page::JugglerPage::add_binding`] installs `window.<name>` in
//! every execution context (`Page.addBinding`). When the page calls it,
//! a `Page.bindingCalled` event fires with the **first argument** as
//! payload — subscribe through
//! [`crate::page::JugglerPage::binding_calls`].

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

/// One binding invocation (`Page.bindingCalled`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct BindingCall {
    /// Binding name (the `window.<name>` that was called).
    pub name: String,
    /// The first argument the page passed (JSON value).
    pub payload: Value,
    /// Execution context that invoked the binding.
    pub execution_context_id: String,
}

/// `Page.addBinding` params.
///
/// `script` is evaluated in every new execution context after the native
/// `window.<name>` function is installed — use it for wrappers (e.g. a
/// promise-based API around the one-shot native callback). Empty for the
/// bare function.
pub(crate) fn add_binding(world_name: Option<&str>, name: &str, script: &str) -> Value {
    let mut params = serde_json::json!({"name": name, "script": script});
    if let Some(world_name) = world_name {
        params["worldName"] = Value::String(world_name.to_string());
    }
    params
}

/// Decodes a `Page.bindingCalled` payload.
pub(crate) fn decode_binding_call(params: &Value) -> Option<BindingCall> {
    Some(BindingCall {
        name: params.get("name")?.as_str()?.to_string(),
        payload: params.get("payload").cloned().unwrap_or(Value::Null),
        execution_context_id: params
            .get("executionContextId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// The binding invocation stream for a page session.
///
/// Obtained from [`crate::page::JugglerPage::binding_calls`].
pub struct BindingCalls {
    calls: UnboundedReceiver<BindingCall>,
}

impl BindingCalls {
    pub(crate) fn new(calls: UnboundedReceiver<BindingCall>) -> Self {
        Self { calls }
    }

    /// Waits for the next binding invocation.
    ///
    /// `None` means the session ended.
    pub async fn next(&mut self) -> Option<BindingCall> {
        self.calls.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_add_binding_params() {
        let params = add_binding(None, "hello", "");
        assert_eq!(params["name"], "hello");
        assert_eq!(params["script"], "");
        assert!(params.get("worldName").is_none());

        let params = add_binding(Some("utility"), "hello", "wrapper()");
        assert_eq!(params["worldName"], "utility");
        assert_eq!(params["script"], "wrapper()");
    }

    #[test]
    fn decodes_binding_calls() {
        let call = decode_binding_call(&json!({
            "executionContextId": "ctx-1",
            "name": "hello",
            "payload": {"greeting": "hi"},
        }))
        .unwrap();
        assert_eq!(call.name, "hello");
        assert_eq!(call.payload["greeting"], "hi");
        assert_eq!(call.execution_context_id, "ctx-1");

        // Missing payload defaults to null; missing name is rejected.
        let call = decode_binding_call(&json!({"name": "ping"})).unwrap();
        assert!(call.payload.is_null());
        assert!(decode_binding_call(&json!({"payload": 1})).is_none());
    }
}
