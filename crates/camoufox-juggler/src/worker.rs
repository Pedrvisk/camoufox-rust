//! Worker lifecycle and messaging.
//!
//! Firefox reports dedicated workers (web workers) through
//! `Page.workerCreated` / `Page.workerDestroyed` /
//! `Page.dispatchMessageFromWorker`. Workers are torn down when their
//! frame commits a new navigation (Playwright parity).
//!
//! [`crate::page::JugglerPage::send_message_to_worker`] tunnels a message
//! to the worker through `Page.sendMessageToWorker` — the same channel
//! Playwright uses to drive a full Juggler session inside the worker, so
//! payloads are conventionally JSON strings.

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

/// A dedicated worker (web worker) belonging to a page frame.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerInfo {
    /// Worker id.
    pub worker_id: String,
    /// The frame that spawned the worker.
    pub frame_id: String,
    /// Worker script URL.
    pub url: String,
}

/// One worker lifecycle event.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WorkerEvent {
    /// A worker was created.
    Created(WorkerInfo),
    /// A worker was destroyed (or its frame navigated away).
    Destroyed {
        /// Worker id.
        worker_id: String,
    },
    /// A message arrived from a worker.
    Message {
        /// Worker id.
        worker_id: String,
        /// Message payload (by convention a JSON string).
        message: String,
    },
}

/// Decodes a `Page.workerCreated` payload.
pub(crate) fn decode_worker_created(params: &Value) -> Option<WorkerInfo> {
    Some(WorkerInfo {
        worker_id: params.get("workerId")?.as_str()?.to_string(),
        frame_id: params
            .get("frameId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        url: params
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// Decodes a `workerId` field.
pub(crate) fn decode_worker_id(params: &Value) -> Option<String> {
    Some(params.get("workerId")?.as_str()?.to_string())
}

/// Decodes a `Page.dispatchMessageFromWorker` payload.
pub(crate) fn decode_worker_message(params: &Value) -> Option<(String, String)> {
    let worker_id = params.get("workerId")?.as_str()?.to_string();
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some((worker_id, message))
}

/// The worker event stream for a page session.
///
/// Obtained from [`crate::page::JugglerPage::worker_events`].
pub struct WorkerEvents {
    events: UnboundedReceiver<WorkerEvent>,
}

impl WorkerEvents {
    pub(crate) fn new(events: UnboundedReceiver<WorkerEvent>) -> Self {
        Self { events }
    }

    /// Waits for the next worker event.
    ///
    /// `None` means the session ended.
    pub async fn next(&mut self) -> Option<WorkerEvent> {
        self.events.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_worker_created() {
        let worker = decode_worker_created(&json!({
            "workerId": "w1",
            "frameId": "f1",
            "url": "https://example.com/worker.js",
        }))
        .unwrap();
        assert_eq!(worker.worker_id, "w1");
        assert_eq!(worker.frame_id, "f1");
        assert_eq!(worker.url, "https://example.com/worker.js");
    }

    #[test]
    fn decodes_worker_ids_and_messages() {
        assert_eq!(
            decode_worker_id(&json!({"workerId": "w1"})).as_deref(),
            Some("w1")
        );
        assert!(decode_worker_id(&json!({})).is_none());

        let (worker_id, message) =
            decode_worker_message(&json!({"workerId": "w1", "message": "{\"id\":1}"})).unwrap();
        assert_eq!(worker_id, "w1");
        assert_eq!(message, "{\"id\":1}");
    }
}
