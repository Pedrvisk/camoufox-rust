//! Connection: request/response correlation and event fan-out.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, oneshot};

use crate::error::{JugglerError, Result};
use crate::protocol::{self, Event, ROOT_SESSION};

/// Maximum buffered events for sessions without a subscriber yet.
const BACKLOG_CAP: usize = 4096;

/// Default per-command timeout.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

struct ConnectionInner {
    writer: tokio::sync::Mutex<tokio::fs::File>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, oneshot::Sender<Value>>>,
    subscribers: Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Event>>>,
    backlog: Mutex<VecDeque<Event>>,
    closed: AtomicBool,
}

/// A live Juggler connection over the pipe transport.
pub struct Connection {
    inner: Arc<ConnectionInner>,
}

impl Connection {
    /// Starts the reader loop over the transport's pipe ends.
    pub fn new(write: tokio::fs::File, read: tokio::fs::File) -> Self {
        let inner = Arc::new(ConnectionInner {
            writer: tokio::sync::Mutex::new(write),
            next_id: AtomicI64::new(0),
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(HashMap::new()),
            backlog: Mutex::new(VecDeque::new()),
            closed: AtomicBool::new(false),
        });
        let reader_inner = inner.clone();
        tokio::spawn(async move {
            read_loop(read, reader_inner).await;
        });
        Self { inner }
    }

    /// Whether the connection is closed.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// Subscribes to a session's events (root session for browser events).
    ///
    /// Events that arrived before subscribing are replayed in order.
    pub fn subscribe(&self, session_id: &str) -> mpsc::UnboundedReceiver<Event> {
        let (tx, rx) = mpsc::unbounded_channel();
        {
            let mut subscribers = self.inner.subscribers.lock().unwrap();
            subscribers.insert(session_id.to_string(), tx.clone());
            // Replay backlog for this session, preserving order.
            let mut backlog = self.inner.backlog.lock().unwrap();
            let mut remaining = VecDeque::new();
            while let Some(event) = backlog.pop_front() {
                if event.session_id == session_id {
                    let _ = tx.send(event);
                } else {
                    remaining.push_back(event);
                }
            }
            *backlog = remaining;
        }
        rx
    }

    /// Sends a command and awaits its result.
    pub async fn send_command(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        if self.is_closed() {
            return Err(JugglerError::Disconnected);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let message = protocol::request_frame(id, session_id, method, params);
        if let Err(e) = write_frame(&self.inner, &message).await {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => {
                if let Some(error) = protocol::response_error(&response) {
                    Err(JugglerError::Protocol(format!("{method}: {error}")))
                } else {
                    Ok(protocol::response_result(&response))
                }
            }
            Ok(Err(_)) => Err(JugglerError::Disconnected),
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                Err(JugglerError::Timeout(format!("{method} ({timeout:?})")))
            }
        }
    }

    /// Sends a command without waiting for (or registering) its response.
    ///
    /// Used for `Browser.close`, whose response is dropped by the browser
    /// while tearing the pipe down (Playwright's `kBrowserCloseMessageId`).
    pub async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        if self.is_closed() {
            return Ok(());
        }
        let id = protocol::BROWSER_CLOSE_MESSAGE_ID;
        let message = protocol::request_frame(id, None, method, params);
        // Best effort: ignore write errors after teardown starts.
        let _ = write_frame(&self.inner, &message).await;
        Ok(())
    }

    /// Fails all pending commands (shutdown path).
    pub fn mark_closed(&self) {
        self.inner.closed.store(true, Ordering::Release);
    }
}

async fn write_frame(inner: &ConnectionInner, message: &Value) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut frame = serde_json::to_string(message)?.into_bytes();
    frame.push(b'\0');
    let mut writer = inner.writer.lock().await;
    writer
        .write_all(&frame)
        .await
        .map_err(|e| JugglerError::Io(format!("pipe write: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| JugglerError::Io(format!("pipe flush: {e}")))?;
    Ok(())
}

async fn read_loop(read: tokio::fs::File, inner: Arc<ConnectionInner>) {
    let mut reader = read;
    let mut buffer: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 8 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                while let Some(position) = buffer.iter().position(|&b| b == 0) {
                    let frame: Vec<u8> = buffer.drain(..=position).collect();
                    if frame.len() <= 1 {
                        continue;
                    }
                    match serde_json::from_slice::<Value>(&frame[..frame.len() - 1]) {
                        Ok(message) => dispatch(inner.clone(), message),
                        Err(e) => log::warn!("juggler: unparseable frame: {e}"),
                    }
                }
                if buffer.len() > 8 * 1024 * 1024 {
                    log::error!("juggler: oversized frame without NUL, dropping");
                    buffer.clear();
                }
            }
            Err(e) => {
                log::debug!("juggler: pipe read error: {e}");
                break;
            }
        }
    }
    inner.closed.store(true, Ordering::Release);
    let pending: Vec<oneshot::Sender<Value>> = {
        let mut map = inner.pending.lock().unwrap();
        map.drain().map(|(_, sender)| sender).collect()
    };
    for sender in pending {
        let _ = sender.send(serde_json::json!({"error": {"message": "connection closed"}}));
    }
}

fn dispatch(inner: Arc<ConnectionInner>, message: Value) {
    // Response: has "id" and no "method".
    if message.get("method").is_none() {
        if let Some(id) = message.get("id").and_then(Value::as_i64) {
            if let Some(sender) = inner.pending.lock().unwrap().remove(&id) {
                let _ = sender.send(message);
            }
            return;
        }
        log::warn!("juggler: malformed message without id/method");
        return;
    }

    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = message
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or(ROOT_SESSION)
        .to_string();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let event = Event {
        method,
        params,
        session_id: session_id.clone(),
    };

    let route = {
        let subscribers = inner.subscribers.lock().unwrap();
        subscribers.get(&session_id).cloned()
    };
    match route {
        Some(sender) => {
            let _ = sender.send(event);
        }
        None => {
            let mut backlog = inner.backlog.lock().unwrap();
            if backlog.len() >= BACKLOG_CAP {
                log::warn!("juggler: event backlog full, dropping oldest events");
                backlog.pop_front();
            }
            backlog.push_back(event);
        }
    }
}
