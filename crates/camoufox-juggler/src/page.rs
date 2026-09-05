//! Page sessions: navigation, evaluation, screenshots, dialogs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::connection::{Connection, DEFAULT_COMMAND_TIMEOUT};
use crate::error::{JugglerError, Result};
use crate::protocol;

/// Shared page state, updated by the event pump.
#[derive(Default)]
struct PageState {
    /// frameId → parent frameId (None for the main frame).
    frames: Mutex<HashMap<String, Option<String>>>,
    /// Latest known URL per frame.
    urls: Mutex<HashMap<String, String>>,
    /// Execution contexts: (id, frameId) pairs, newest last.
    contexts: Mutex<Vec<(String, String)>>,
    /// Monotonic sequence of handled events.
    seq: std::sync::atomic::AtomicU64,
    /// (seq, frameId, name) of observed lifecycle events.
    lifecycle: Mutex<Vec<(u64, String, String)>>,
    /// (seq, frameId, navigationId) of committed navigations.
    commits: Mutex<Vec<(u64, String, String)>>,
    /// Open dialogs.
    dialogs: Mutex<HashMap<String, Dialog>>,
    /// Whether the target was detached.
    detached: AtomicBool,
    /// Broadcast senders for `network_events()` subscribers.
    network_broadcast: Mutex<Vec<tokio::sync::mpsc::UnboundedSender<crate::protocol::Event>>>,
    /// Whether request interception is enabled.
    interception: AtomicBool,
    /// Pending intercepted file choosers.
    file_choosers: Mutex<Vec<PendingFileChooser>>,
    /// Active workers (workerId → info).
    workers: Mutex<HashMap<String, crate::worker::WorkerInfo>>,
    /// Broadcast senders for screencast frames.
    screencast_broadcast:
        Mutex<Vec<tokio::sync::mpsc::UnboundedSender<crate::screencast::ScreencastFrame>>>,
    /// Broadcast senders for worker events.
    worker_broadcast: Mutex<Vec<tokio::sync::mpsc::UnboundedSender<crate::worker::WorkerEvent>>>,
}

/// Data captured from a `Page.fileChooserOpened` event.
#[derive(Debug, Clone)]
struct PendingFileChooser {
    object_id: String,
    execution_context_id: String,
    frame_id: String,
}

/// An intercepted file chooser: the `<input type=file>` element that
/// triggered it, ready to receive files through [`FileChooser::set_files`].
pub struct FileChooser {
    /// Remote object id of the input element.
    pub object_id: String,
    /// Execution context the element lives in.
    pub execution_context_id: String,
    /// Frame the element lives in.
    pub frame_id: String,
    connection: Arc<Connection>,
    session_id: String,
}

impl FileChooser {
    /// Assigns local files to the input element.
    ///
    /// Paths must be absolute and readable by the browser process.
    pub async fn set_files(&self, files: &[impl AsRef<std::path::Path>]) -> Result<()> {
        let files: Vec<String> = files
            .iter()
            .map(|path| path.as_ref().to_string_lossy().into_owned())
            .collect();
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.setFileInputFiles",
                serde_json::json!({
                    "frameId": self.frame_id,
                    "objectId": self.object_id,
                    "files": files,
                }),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }
}

/// Extracts (executionContextId, objectId) from a file chooser event.
fn decode_file_chooser(params: &Value) -> Option<(String, String)> {
    let execution_context_id = params
        .get("executionContextId")
        .and_then(Value::as_str)?
        .to_string();
    let object_id = params
        .pointer("/element/objectId")
        .and_then(Value::as_str)?
        .to_string();
    Some((execution_context_id, object_id))
}

/// An open JS dialog (alert/confirm/prompt/beforeunload).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Dialog {
    /// Dialog id.
    pub id: String,
    /// Dialog type.
    pub kind: String,
    /// Dialog message.
    pub message: String,
    /// Default prompt value, when any.
    pub default_value: Option<String>,
}

/// A Juggler page target.
pub struct JugglerPage {
    pub(crate) connection: Arc<Connection>,
    /// Juggler session id for this target.
    pub session_id: String,
    /// Target id.
    pub target_id: String,
    /// Owning browser context id.
    pub browser_context_id: Option<String>,
    state: Arc<PageState>,
    events: tokio::sync::Mutex<UnboundedReceiver<crate::protocol::Event>>,
}

impl JugglerPage {
    /// Creates a page handle over an attached target session.
    pub fn new(
        connection: Arc<Connection>,
        session_id: String,
        target_id: String,
        browser_context_id: Option<String>,
        events: UnboundedReceiver<crate::protocol::Event>,
    ) -> Arc<Self> {
        let state = Arc::new(PageState::default());
        let page = Arc::new(Self {
            connection,
            session_id,
            target_id,
            browser_context_id,
            state: state.clone(),
            events: tokio::sync::Mutex::new(events),
        });
        // Pump buffered events so early navigation/context events are
        // reflected in the state before the first command.
        let page2 = page.clone();
        tokio::spawn(async move {
            page2.pump(Duration::from_millis(200)).await;
        });
        page
    }

    /// The page's current main-frame URL.
    pub fn url(&self) -> Option<String> {
        let main = self.main_frame_id()?;
        self.state.urls.lock().unwrap().get(&main).cloned()
    }

    /// Whether the target was detached from the browser.
    pub fn is_detached(&self) -> bool {
        self.state.detached.load(Ordering::Acquire)
    }

    /// The main frame id, when known.
    pub fn main_frame_id(&self) -> Option<String> {
        let frames = self.state.frames.lock().unwrap();
        frames
            .iter()
            .find(|(_, parent)| parent.is_none())
            .map(|(id, _)| id.clone())
    }

    fn handle_event(&self, event: crate::protocol::Event) {
        // Network and WebSocket events are forwarded to subscribers, not
        // page state.
        if event.method.starts_with("Network.") || event.method.starts_with("Page.webSocket") {
            let mut senders = self.state.network_broadcast.lock().unwrap();
            senders.retain(|sender| {
                let _ = sender.send(event.clone());
                !sender.is_closed()
            });
            return;
        }
        let seq = self.state.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let params = &event.params;
        match event.method.as_str() {
            "Page.frameAttached" => {
                let frame_id = str_field(params, "frameId");
                let parent = str_field_opt(params, "parentFrameId");
                if let Some(frame_id) = frame_id {
                    self.state.frames.lock().unwrap().insert(frame_id, parent);
                }
            }
            "Page.frameDetached" => {
                if let Some(frame_id) = str_field(params, "frameId") {
                    self.state.frames.lock().unwrap().remove(&frame_id);
                    self.state.urls.lock().unwrap().remove(&frame_id);
                }
            }
            "Page.navigationCommitted" => {
                let frame_id = str_field(params, "frameId").unwrap_or_default();
                let url = str_field(params, "url").unwrap_or_default();
                let navigation_id = str_field_opt(params, "navigationId").unwrap_or_default();
                // Unknown frame committing → main frame (Playwright's
                // FrameTree heuristic).
                {
                    let mut frames = self.state.frames.lock().unwrap();
                    frames.entry(frame_id.clone()).or_insert(None);
                }
                self.state
                    .urls
                    .lock()
                    .unwrap()
                    .insert(frame_id.clone(), url);
                // Workers of the navigating frame are torn down
                // (Playwright parity).
                let destroyed: Vec<String> = {
                    let mut workers = self.state.workers.lock().unwrap();
                    let ids: Vec<String> = workers
                        .iter()
                        .filter(|(_, worker)| worker.frame_id == frame_id)
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in &ids {
                        workers.remove(id);
                    }
                    ids
                };
                for worker_id in destroyed {
                    self.broadcast_worker_event(crate::worker::WorkerEvent::Destroyed {
                        worker_id,
                    });
                }
                self.state
                    .commits
                    .lock()
                    .unwrap()
                    .push((seq, frame_id, navigation_id));
            }
            "Page.sameDocumentNavigation" => {
                let frame_id = str_field(params, "frameId").unwrap_or_default();
                let url = str_field(params, "url").unwrap_or_default();
                self.state.urls.lock().unwrap().insert(frame_id, url);
            }
            "Page.eventFired" => {
                let frame_id = str_field(params, "frameId").unwrap_or_default();
                let name = str_field(params, "name").unwrap_or_default();
                self.state
                    .lifecycle
                    .lock()
                    .unwrap()
                    .push((seq, frame_id, name));
            }
            "Runtime.executionContextCreated" => {
                let context_id = str_field(params, "executionContextId");
                let frame_id = params
                    .pointer("/auxData/frameId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(context_id) = context_id {
                    let mut contexts = self.state.contexts.lock().unwrap();
                    contexts.retain(|(id, _)| id != &context_id);
                    contexts.push((context_id, frame_id));
                }
            }
            "Runtime.executionContextDestroyed" => {
                if let Some(context_id) = str_field(params, "executionContextId") {
                    self.state
                        .contexts
                        .lock()
                        .unwrap()
                        .retain(|(id, _)| id != &context_id);
                }
            }
            "Runtime.executionContextsCleared" => {
                self.state.contexts.lock().unwrap().clear();
            }
            "Page.dialogOpened" => {
                let dialog = Dialog {
                    id: str_field(params, "dialogId").unwrap_or_default(),
                    kind: str_field(params, "type").unwrap_or_default(),
                    message: str_field(params, "message").unwrap_or_default(),
                    default_value: str_field_opt(params, "defaultValue"),
                };
                self.state
                    .dialogs
                    .lock()
                    .unwrap()
                    .insert(dialog.id.clone(), dialog);
            }
            "Page.dialogClosed" => {
                if let Some(dialog_id) = str_field(params, "dialogId") {
                    self.state.dialogs.lock().unwrap().remove(&dialog_id);
                }
            }
            "Page.screencastFrame" => {
                if let Some(frame) = crate::screencast::decode_frame(params) {
                    let mut senders = self.state.screencast_broadcast.lock().unwrap();
                    senders.retain(|sender| {
                        let _ = sender.send(frame.clone());
                        !sender.is_closed()
                    });
                    // Ack so the browser keeps producing frames
                    // (Playwright parity).
                    let connection = self.connection.clone();
                    let session = self.session_id.clone();
                    tokio::spawn(async move {
                        let _ = connection
                            .send_command(
                                Some(&session),
                                "Page.screencastFrameAck",
                                serde_json::json!({}),
                                DEFAULT_COMMAND_TIMEOUT,
                            )
                            .await;
                    });
                }
            }
            "Page.fileChooserOpened" => {
                if let Some((execution_context_id, object_id)) = decode_file_chooser(params) {
                    let frame_id = self
                        .state
                        .contexts
                        .lock()
                        .unwrap()
                        .iter()
                        .rev()
                        .find(|(id, _)| id == &execution_context_id)
                        .map(|(_, frame)| frame.clone())
                        .unwrap_or_default();
                    self.state
                        .file_choosers
                        .lock()
                        .unwrap()
                        .push(PendingFileChooser {
                            object_id,
                            execution_context_id,
                            frame_id,
                        });
                }
            }
            "Page.workerCreated" => {
                if let Some(worker) = crate::worker::decode_worker_created(params) {
                    self.state
                        .workers
                        .lock()
                        .unwrap()
                        .insert(worker.worker_id.clone(), worker.clone());
                    self.broadcast_worker_event(crate::worker::WorkerEvent::Created(worker));
                }
            }
            "Page.workerDestroyed" => {
                if let Some(worker_id) = crate::worker::decode_worker_id(params) {
                    self.state.workers.lock().unwrap().remove(&worker_id);
                    self.broadcast_worker_event(crate::worker::WorkerEvent::Destroyed {
                        worker_id,
                    });
                }
            }
            "Page.dispatchMessageFromWorker" => {
                if let Some((worker_id, message)) = crate::worker::decode_worker_message(params) {
                    self.broadcast_worker_event(crate::worker::WorkerEvent::Message {
                        worker_id,
                        message,
                    });
                }
            }
            "Browser.detachedFromTarget"
                if str_field(params, "targetId").as_deref() == Some(&self.target_id) =>
            {
                self.state.detached.store(true, Ordering::Release);
            }
            _ => {}
        }
    }

    /// Fans a worker event out to subscribers, dropping closed ones.
    fn broadcast_worker_event(&self, event: crate::worker::WorkerEvent) {
        let mut senders = self.state.worker_broadcast.lock().unwrap();
        senders.retain(|sender| {
            let _ = sender.send(event.clone());
            !sender.is_closed()
        });
    }

    /// Waits until the predicate holds, processing events meanwhile.
    async fn wait_until(
        &self,
        mut predicate: impl FnMut() -> bool,
        timeout: Duration,
        what: &str,
    ) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if predicate() {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(JugglerError::Timeout(what.to_string()));
            }
            let event = {
                let mut events = self.events.lock().await;
                match tokio::time::timeout(remaining, events.recv()).await {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        if predicate() {
                            return Ok(());
                        }
                        return Err(JugglerError::Disconnected);
                    }
                    Err(_) => {
                        if predicate() {
                            return Ok(());
                        }
                        return Err(JugglerError::Timeout(what.to_string()));
                    }
                }
            };
            self.handle_event(event);
        }
    }

    /// Drains pending events for a short while (state refresh).
    pub async fn pump(&self, budget: Duration) {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return;
            }
            let event = {
                let mut events = self.events.lock().await;
                match tokio::time::timeout(remaining, events.recv()).await {
                    Ok(Some(event)) => event,
                    _ => return,
                }
            };
            self.handle_event(event);
        }
    }

    /// Navigates the main frame to `url` and waits for the load event.
    pub async fn goto(&self, url: &str) -> Result<String> {
        self.goto_with_timeout(url, Duration::from_secs(60)).await
    }

    /// [`JugglerPage::goto`] with a custom navigation timeout.
    pub async fn goto_with_timeout(&self, url: &str, timeout: Duration) -> Result<String> {
        self.wait_until(
            || self.main_frame_id().is_some(),
            Duration::from_secs(10),
            "main frame",
        )
        .await?;
        let main_frame = self
            .main_frame_id()
            .ok_or_else(|| JugglerError::Protocol("main frame disappeared".into()))?;

        let result = self
            .connection
            .send_command(
                Some(&self.session_id),
                "Page.navigate",
                protocol::navigate(&main_frame, url, None),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        let navigation_id = result
            .get("navigationId")
            .and_then(Value::as_str)
            .map(str::to_string);

        // Wait for our navigation to commit (or a same-document navigation).
        self.wait_until(
            || {
                navigation_id.as_deref().map_or(true, |id| {
                    self.state
                        .commits
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(_, frame, nav)| frame == &main_frame && nav == id)
                }) || self.url().as_deref() == Some(url)
            },
            timeout,
            "navigation commit",
        )
        .await?;
        let commit_seq = self
            .state
            .commits
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, frame, _)| frame == &main_frame)
            .map(|(seq, _, _)| *seq)
            .max()
            .unwrap_or(0);

        // Wait for the load event *after* the commit.
        self.wait_until(
            || {
                self.state
                    .lifecycle
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(seq, frame, name)| {
                        *seq > commit_seq && frame == &main_frame && name == "load"
                    })
            },
            timeout,
            "load event",
        )
        .await?;

        Ok(self.url().unwrap_or_else(|| url.to_string()))
    }

    /// Returns a usable execution context for the main frame.
    async fn execution_context(&self) -> Result<String> {
        self.wait_until(
            || self.current_context().is_some(),
            Duration::from_secs(10),
            "execution context",
        )
        .await?;
        self.current_context()
            .ok_or_else(|| JugglerError::Protocol("no execution context".into()))
    }

    fn current_context(&self) -> Option<String> {
        let contexts = self.state.contexts.lock().unwrap();
        if let Some(main) = self.main_frame_id() {
            if let Some((id, _)) = contexts.iter().rev().find(|(_, frame)| frame == &main) {
                return Some(id.clone());
            }
        }
        contexts.last().map(|(id, _)| id.clone())
    }

    /// Evaluates a JS expression (by value) in the main-frame context.
    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        self.evaluate_with_timeout(expression, DEFAULT_COMMAND_TIMEOUT)
            .await
    }

    /// [`JugglerPage::evaluate`] with a custom timeout.
    pub async fn evaluate_with_timeout(
        &self,
        expression: &str,
        timeout: Duration,
    ) -> Result<Value> {
        let context = self.execution_context().await?;
        let result = self
            .connection
            .send_command(
                Some(&self.session_id),
                "Runtime.evaluate",
                protocol::evaluate(&context, expression, true),
                timeout,
            )
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error)
                if matches!(&error, JugglerError::Protocol(message)
                    if message.contains("execution context")
                        || message.contains("destroyed")
                        || message.contains("not found")) =>
            {
                // Stale context (a navigation raced the evaluate): drop it
                // from the state, pump events and retry once against
                // whichever context comes next.
                self.state
                    .contexts
                    .lock()
                    .unwrap()
                    .retain(|(id, _)| id != &context);
                self.pump(Duration::from_millis(300)).await;
                let fresh = match self.execution_context().await {
                    Ok(fresh) => fresh,
                    Err(_) => return Err(error),
                };
                if fresh == context {
                    // Nothing new arrived; report the original failure.
                    return Err(error);
                }
                self.connection
                    .send_command(
                        Some(&self.session_id),
                        "Runtime.evaluate",
                        protocol::evaluate(&fresh, expression, true),
                        timeout,
                    )
                    .await?
            }
            Err(e) => return Err(e),
        };

        if let Some(details) = result.get("exceptionDetails") {
            let text = details
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("exception");
            let value = details.get("value").cloned().unwrap_or(Value::Null);
            return Err(JugglerError::Protocol(format!(
                "evaluation failed: {text} {value}"
            )));
        }
        let remote = result
            .get("result")
            .ok_or_else(|| JugglerError::Protocol("missing result".into()))?;
        if let Some(unserializable) = remote.get("unserializableValue").and_then(Value::as_str) {
            return Ok(Value::String(unserializable.to_string()));
        }
        Ok(remote.get("value").cloned().unwrap_or(Value::Null))
    }

    /// The fully-rendered HTML of the page.
    pub async fn content(&self) -> Result<String> {
        let html = self.evaluate("document.documentElement.outerHTML").await?;
        html.as_str().map(str::to_string).ok_or_else(|| {
            JugglerError::Protocol("document.outerHTML did not return a string".into())
        })
    }

    /// Sets the viewport size.
    pub async fn set_viewport(&self, width: u32, height: u32) -> Result<()> {
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.setViewportSize",
                protocol::viewport_size(width, height),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Brings the page to the front.
    pub async fn bring_to_front(&self) -> Result<()> {
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.bringToFront",
                serde_json::json!({}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Takes a screenshot of the current viewport and writes it to `path`.
    pub async fn screenshot(&self, path: &std::path::Path) -> Result<()> {
        self.screenshot_with_mime(path, "image/png").await
    }

    /// [`JugglerPage::screenshot`] with a MIME type (`image/png`/`image/jpeg`).
    pub async fn screenshot_with_mime(&self, path: &std::path::Path, mime: &str) -> Result<()> {
        let dims = self
            .evaluate("({w: window.innerWidth, h: window.innerHeight})")
            .await?;
        let width = dims.get("w").and_then(Value::as_f64).unwrap_or(1280.0);
        let height = dims.get("h").and_then(Value::as_f64).unwrap_or(720.0);
        let result = self
            .connection
            .send_command(
                Some(&self.session_id),
                "Page.screenshot",
                protocol::screenshot(mime, 0.0, 0.0, width, height),
                Duration::from_secs(30),
            )
            .await?;
        let data = result
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| JugglerError::Protocol("screenshot returned no data".into()))?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| JugglerError::Protocol(format!("screenshot base64: {e}")))?;
        tokio::fs::write(path, bytes)
            .await
            .map_err(|e| JugglerError::Io(format!("write {}: {e}", path.display())))?;
        Ok(())
    }

    /// Currently open dialogs.
    pub fn dialogs(&self) -> Vec<Dialog> {
        self.state
            .dialogs
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Accepts or dismisses a dialog.
    pub async fn handle_dialog(
        &self,
        dialog_id: &str,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<()> {
        let mut params = serde_json::json!({"dialogId": dialog_id, "accept": accept});
        if let Some(text) = prompt_text {
            params["promptText"] = Value::String(text.to_string());
        }
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.handleDialog",
                params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Reloads the page.
    pub async fn reload(&self) -> Result<()> {
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.reload",
                serde_json::json!({}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Closes the page.
    pub async fn close(&self) -> Result<()> {
        let _ = self
            .connection
            .send_command(
                Some(&self.session_id),
                "Page.close",
                serde_json::json!({"runBeforeUnload": false}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await;
        Ok(())
    }

    /// Adds an init script evaluated before any page script.
    pub async fn add_init_script(&self, script: &str) -> Result<()> {
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.setInitScripts",
                serde_json::json!({"scripts": [{"script": script}]}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    // -- Cookies (context level) ------------------------------------------------

    /// Cookies for this page's browser context.
    pub async fn cookies(&self) -> Result<Vec<Value>> {
        let mut params = serde_json::json!({});
        if let Some(context) = &self.browser_context_id {
            params["browserContextId"] = Value::String(context.clone());
        }
        let result = self
            .connection
            .send_command(None, "Browser.getCookies", params, DEFAULT_COMMAND_TIMEOUT)
            .await?;
        Ok(result
            .get("cookies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Sets cookies in this page's browser context.
    pub async fn set_cookies(&self, cookies: &[Value]) -> Result<()> {
        let mut params = serde_json::json!({"cookies": cookies});
        if let Some(context) = &self.browser_context_id {
            params["browserContextId"] = Value::String(context.clone());
        }
        self.connection
            .send_command(None, "Browser.setCookies", params, DEFAULT_COMMAND_TIMEOUT)
            .await?;
        Ok(())
    }

    // -- Network -------------------------------------------------------------------

    /// Subscribes to this page's network events.
    ///
    /// Each [`NetworkEvent`] is delivered in order; interception requests
    /// (requests with `is_intercepted`) can be decided through
    /// [`JugglerPage::take_intercepted_request`].
    pub fn network_events(&self) -> crate::network::NetworkEvents {
        // A second subscriber would steal the page session's event stream,
        // so events are re-broadcast by the existing pump instead: we
        // subscribe through a dedicated channel fed from the pump loop.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.state.network_broadcast.lock().unwrap().push(tx);
        crate::network::NetworkEvents::new(rx)
    }

    /// Enables or disables request interception on this page.
    ///
    /// While enabled, requests surface as `Network.requestWillBeSent` with
    /// `is_intercepted: true` and block until
    /// [`InterceptedRequest::continue_request`], [`InterceptedRequest::fulfill`]
    /// or [`InterceptedRequest::abort`] answers them (the browser applies a
    /// default continue after the handle is dropped).
    pub async fn set_request_interception(&self, enabled: bool) -> Result<()> {
        crate::network::set_request_interception(&self.connection, &self.session_id, enabled)
            .await?;
        self.state.interception.store(enabled, Ordering::Release);
        Ok(())
    }

    /// Builds an [`InterceptedRequest`] handle for a pending intercepted
    /// request observed through [`JugglerPage::network_events`].
    pub fn take_intercepted_request(
        &self,
        request: &crate::network::NetworkRequest,
    ) -> Arc<crate::network::InterceptedRequest> {
        Arc::new(crate::network::InterceptedRequest::new(
            self.connection.clone(),
            self.session_id.clone(),
            request.clone(),
        ))
    }

    /// Fetches the response body of a finished request.
    pub async fn response_body(&self, request_id: &str) -> Result<Vec<u8>> {
        crate::network::get_response_body(&self.connection, &self.session_id, request_id).await
    }

    // -- WebSocket injection ---------------------------------------------------

    /// Installs the WebSocket registry hook (required before
    /// [`JugglerPage::send_websocket_message`] works).
    ///
    /// Must run *before* the page constructs its WebSockets (call it right
    /// after page creation, before `goto`). Re-installing is a no-op
    /// (`Page.setInitScripts` replaces the script list).
    pub async fn enable_websocket_injection(&self) -> Result<()> {
        self.add_init_script(crate::network::WEBSOCKET_INJECTION_INIT_SCRIPT)
            .await
    }

    /// Sends a text message over a live WebSocket matching `url` as the
    /// page (client→server injection).
    ///
    /// Requires [`JugglerPage::enable_websocket_injection`] to have run
    /// before the socket was constructed. With several sockets open for
    /// the same URL the message goes to the most recent one.
    pub async fn send_websocket_message(&self, url: &str, message: &str) -> Result<()> {
        self.evaluate(&format!(
            "(msg => {{ const list = window.__camoufoxSockets && window.__camoufoxSockets[{url_json}]; \
             if (!list || !list.length) throw new Error('no live WebSocket registered for {url_json}'); \
             const socket = list[list.length - 1]; \
             if (socket.readyState !== 1) throw new Error('WebSocket for {url_json} is not open'); \
             socket.send(msg); }})({payload_json})",
            url_json = serde_json::to_string(url)?,
            payload_json = serde_json::to_string(message)?,
        ))
        .await?;
        Ok(())
    }

    /// Like [`JugglerPage::send_websocket_message`] but sends binary
    /// payload (base64-encoded through the transport, delivered as an
    /// ArrayBuffer).
    pub async fn send_websocket_binary(&self, url: &str, bytes: &[u8]) -> Result<()> {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        self.evaluate(&format!(
            "(b64 => {{ const list = window.__camoufoxSockets && window.__camoufoxSockets[{url_json}]; \
             if (!list || !list.length) throw new Error('no live WebSocket registered for {url_json}'); \
             const socket = list[list.length - 1]; \
             if (socket.readyState !== 1) throw new Error('WebSocket for {url_json} is not open'); \
             const bytes = Uint8Array.from(atob(b64), c => c.charCodeAt(0)); \
             socket.send(bytes.buffer); }})({payload_json})",
            url_json = serde_json::to_string(url)?,
            payload_json = serde_json::to_string(&encoded)?,
        ))
        .await?;
        Ok(())
    }

    /// Lists the URLs of live WebSockets registered by the injection
    /// hook, with their readyState (0 connecting, 1 open, 2 closing,
    /// 3 closed).
    pub async fn live_websockets(&self) -> Result<Vec<(String, u8)>> {
        let value = self
            .evaluate(
                "(() => { const out = []; for (const [url, list] of \
                 Object.entries(window.__camoufoxSockets || {})) { \
                 for (const socket of list) out.push([url, socket.readyState]); } return out; })()",
            )
            .await?;
        let mut sockets = Vec::new();
        if let Some(entries) = value.as_array() {
            for entry in entries {
                if let (Some(url), Some(state)) = (
                    entry.get(0).and_then(Value::as_str),
                    entry.get(1).and_then(Value::as_u64),
                ) {
                    sockets.push((url.to_string(), state as u8));
                }
            }
        }
        Ok(sockets)
    }

    // -- Screencast --------------------------------------------------------------

    /// Subscribes to screencast frames (call before
    /// [`JugglerPage::start_screencast`]; frames are auto-acked so the
    /// browser keeps producing them).
    pub fn screencast_frames(&self) -> crate::screencast::ScreencastFrames {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.state.screencast_broadcast.lock().unwrap().push(tx);
        crate::screencast::ScreencastFrames::new(rx)
    }

    /// Starts the screencast.
    ///
    /// `quality` is the JPEG quality (0-100). Frames arrive through
    /// [`JugglerPage::screencast_frames`] as long as the stream is held.
    pub async fn start_screencast(&self, width: u64, height: u64, quality: u64) -> Result<()> {
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.startScreencast",
                crate::screencast::start_screencast(width, height, quality),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Stops the screencast.
    pub async fn stop_screencast(&self) -> Result<()> {
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.stopScreencast",
                serde_json::json!({}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    // -- File chooser ------------------------------------------------------------

    /// Enables or disables file chooser interception.
    ///
    /// While enabled, `<input type=file>` clicks surface as
    /// `Page.fileChooserOpened` events instead of opening the OS dialog;
    /// collect them with [`JugglerPage::wait_for_file_chooser`].
    pub async fn set_intercept_file_chooser(&self, enabled: bool) -> Result<()> {
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.setInterceptFileChooserDialog",
                serde_json::json!({"enabled": enabled}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Number of file choosers waiting to be handled.
    pub fn file_choosers_pending(&self) -> usize {
        self.state.file_choosers.lock().unwrap().len()
    }

    /// Waits for the next intercepted file chooser.
    ///
    /// Requires [`JugglerPage::set_intercept_file_chooser`] to be enabled.
    /// The returned handle's [`FileChooser::set_files`] assigns local
    /// files to the input element.
    pub async fn wait_for_file_chooser(&self, timeout: Duration) -> Result<FileChooser> {
        self.wait_until(
            || !self.state.file_choosers.lock().unwrap().is_empty(),
            timeout,
            "file chooser",
        )
        .await?;
        let pending = self
            .state
            .file_choosers
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| JugglerError::Protocol("file chooser disappeared".into()))?;
        Ok(FileChooser {
            object_id: pending.object_id,
            execution_context_id: pending.execution_context_id,
            frame_id: pending.frame_id,
            connection: self.connection.clone(),
            session_id: self.session_id.clone(),
        })
    }

    // -- Workers -----------------------------------------------------------------

    /// Active workers in this page.
    pub fn workers(&self) -> Vec<crate::worker::WorkerInfo> {
        self.state
            .workers
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Subscribes to worker lifecycle (created/destroyed) and message
    /// events.
    pub fn worker_events(&self) -> crate::worker::WorkerEvents {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.state.worker_broadcast.lock().unwrap().push(tx);
        crate::worker::WorkerEvents::new(rx)
    }

    /// Sends a message to a worker (`Page.sendMessageToWorker`).
    ///
    /// Payloads are conventionally JSON strings — the channel Playwright
    /// uses to drive a full Juggler session inside the worker.
    pub async fn send_message_to_worker(&self, worker_id: &str, message: &str) -> Result<()> {
        let frame_id = self
            .state
            .workers
            .lock()
            .unwrap()
            .get(worker_id)
            .map(|worker| worker.frame_id.clone())
            .ok_or_else(|| JugglerError::Protocol(format!("unknown worker '{worker_id}'")))?;
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.sendMessageToWorker",
                serde_json::json!({
                    "frameId": frame_id,
                    "workerId": worker_id,
                    "message": message,
                }),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    // -- Emulation ---------------------------------------------------------------

    /// Emulates media features for this page (`Page.setEmulatedMedia`):
    /// media type (`screen`/`print`), color scheme, reduced motion,
    /// forced colors and contrast.
    ///
    /// ```text
    /// page.emulate_media(&EmulatedMedia::dark_mode()).await?;
    /// page.emulate_media(&EmulatedMedia::print()).await?;
    /// page.emulate_media(&EmulatedMedia::reset()).await?;
    /// ```
    pub async fn emulate_media(&self, media: &crate::emulation::EmulatedMedia) -> Result<()> {
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.setEmulatedMedia",
                media.to_params(),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Toggles the touch capability of this page's browser context
    /// (`Browser.setTouchOverride`).
    ///
    /// `Some(true)` makes content behave as a touch device (complements
    /// [`crate::input`]'s `tap`/`touch_event` dispatch); `None` clears
    /// the override. Must be set before the page needs to observe it.
    pub async fn set_touch_override(&self, has_touch: Option<bool>) -> Result<()> {
        self.connection
            .send_command(
                None,
                "Browser.setTouchOverride",
                crate::emulation::touch_override(self.browser_context_id.as_deref(), has_touch),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    // -- Local storage -----------------------------------------------------------

    /// Captures local storage entries for the current origin.
    pub async fn local_storage(
        &self,
    ) -> Result<Option<(String, std::collections::BTreeMap<String, String>)>> {
        let value = self
            .evaluate(
                "(() => { const o = {}; for (let i = 0; i < localStorage.length; i++) { \
                  const k = localStorage.key(i); o[k] = localStorage.getItem(k); } \
                  return {origin: location.origin, entries: o}; })()",
            )
            .await?;
        let origin = value
            .get("origin")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut entries = std::collections::BTreeMap::new();
        if let Some(map) = value.get("entries").and_then(Value::as_object) {
            for (key, value) in map {
                if let Some(value) = value.as_str() {
                    entries.insert(key.clone(), value.to_string());
                }
            }
        }
        Ok(origin.map(|origin| (origin, entries)))
    }

    /// Restores local storage entries for the current origin.
    pub async fn set_local_storage(
        &self,
        entries: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let payload = serde_json::to_string(entries)?;
        let script = format!(
            "(args => {{ for (const [k, v] of Object.entries(args)) localStorage.setItem(k, v); }})({payload})"
        );
        self.evaluate(&script).await?;
        Ok(())
    }
}

fn str_field(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

fn str_field_opt(params: &Value, key: &str) -> Option<String> {
    str_field(params, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_file_chooser_events() {
        let (context_id, object_id) =
            decode_file_chooser(&json!({
                "executionContextId": "ctx-1",
                "element": { "type": "object", "subtype": "node", "objectId": "obj-9", "className": "HTMLInputElement" },
            }))
            .unwrap();
        assert_eq!(context_id, "ctx-1");
        assert_eq!(object_id, "obj-9");

        // Missing objectId → ignored.
        assert!(decode_file_chooser(&json!({
            "executionContextId": "ctx-1",
            "element": { "type": "object" },
        }))
        .is_none());
        // Missing context → ignored.
        assert!(decode_file_chooser(&json!({
            "element": { "objectId": "obj-9" },
        }))
        .is_none());
    }
}
