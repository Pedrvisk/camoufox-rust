//! Download management: observe, configure and cancel downloads.
//!
//! Firefox reports downloads through `Browser.downloadCreated` /
//! `Browser.downloadFinished` events. Configure the behavior first
//! ([`crate::browser::JugglerBrowser::set_download_options`]):
//!
//! - `saveToDisk` writes files into the configured downloads directory
//!   (relative paths resolve against it) and the finished event reports
//!   the final path;
//! - `cancel` drops every download.
//!
//! Downloads can be cancelled mid-flight via
//! [`crate::browser::JugglerBrowser::cancel_download`].

use serde_json::Value;

/// What the browser should do with downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadBehavior {
    /// Save downloads to the given directory.
    SaveToDisk(String),
    /// Cancel every download.
    Cancel,
}

/// `Browser.setDownloadOptions` params.
pub(crate) fn download_options(
    browser_context_id: Option<&str>,
    behavior: Option<&DownloadBehavior>,
) -> Value {
    let mut params = serde_json::json!({});
    if let Some(id) = browser_context_id {
        params["browserContextId"] = Value::String(id.to_string());
    }
    match behavior {
        Some(DownloadBehavior::SaveToDisk(dir)) => {
            params["downloadOptions"] = serde_json::json!({
                "behavior": "saveToDisk",
                "downloadsDir": dir,
            });
        }
        Some(DownloadBehavior::Cancel) => {
            params["downloadOptions"] = serde_json::json!({
                "behavior": "cancel",
            });
        }
        None => {
            params["downloadOptions"] = Value::Null;
        }
    }
    params
}

/// `Browser.cancelDownload` params.
pub(crate) fn cancel_download(uuid: Option<&str>) -> Value {
    match uuid {
        Some(uuid) => serde_json::json!({"uuid": uuid}),
        None => serde_json::json!({}),
    }
}

/// A download that started (`Browser.downloadCreated`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadCreated {
    /// Download uuid.
    pub uuid: String,
    /// Browser context the download belongs to, when reported.
    pub browser_context_id: Option<String>,
    /// Target id of the page that started the download.
    pub page_target_id: String,
    /// Frame that triggered the download.
    pub frame_id: String,
    /// Download URL.
    pub url: String,
    /// Filename suggested by the server/browser.
    pub suggested_file_name: String,
}

/// A download that finished (`Browser.downloadFinished`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadFinished {
    /// Download uuid.
    pub uuid: String,
    /// Whether the download was canceled.
    pub canceled: bool,
    /// Error text, when the download failed.
    pub error: Option<String>,
}

pub(crate) fn decode_download_created(params: &Value) -> Option<DownloadCreated> {
    let uuid = params.get("uuid")?.as_str()?.to_string();
    Some(DownloadCreated {
        uuid,
        browser_context_id: params
            .get("browserContextId")
            .and_then(Value::as_str)
            .map(str::to_string),
        page_target_id: params
            .get("pageTargetId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
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
        suggested_file_name: params
            .get("suggestedFileName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

pub(crate) fn decode_download_finished(params: &Value) -> Option<DownloadFinished> {
    let uuid = params.get("uuid")?.as_str()?.to_string();
    Some(DownloadFinished {
        uuid,
        canceled: params
            .get("canceled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        error: params
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// One decoded download event.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DownloadEvent {
    /// A download started.
    Created(DownloadCreated),
    /// A download finished (or was canceled / failed).
    Finished(DownloadFinished),
}

/// The download event stream for a browser session.
///
/// Obtained from [`crate::browser::JugglerBrowser::download_events`].
pub struct DownloadEvents {
    events: tokio::sync::mpsc::UnboundedReceiver<DownloadEvent>,
}

impl DownloadEvents {
    pub(crate) fn new(events: tokio::sync::mpsc::UnboundedReceiver<DownloadEvent>) -> Self {
        Self { events }
    }

    /// Waits for the next download event.
    ///
    /// `Ok(None)` means the browser connection closed.
    pub async fn next(&mut self) -> Option<DownloadEvent> {
        self.events.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_download_options() {
        let params = download_options(
            Some("ctx-1"),
            Some(&DownloadBehavior::SaveToDisk("/tmp/dl".into())),
        );
        assert_eq!(params["browserContextId"], "ctx-1");
        assert_eq!(params["downloadOptions"]["behavior"], "saveToDisk");
        assert_eq!(params["downloadOptions"]["downloadsDir"], "/tmp/dl");

        let params = download_options(None, Some(&DownloadBehavior::Cancel));
        assert!(params.get("browserContextId").is_none());
        assert_eq!(params["downloadOptions"]["behavior"], "cancel");

        let params = download_options(None, None);
        assert!(params["downloadOptions"].is_null());
    }

    #[test]
    fn builds_cancel_download_params() {
        let params = cancel_download(Some("uuid-1"));
        assert_eq!(params["uuid"], "uuid-1");
        let params = cancel_download(None);
        assert!(params.get("uuid").is_none());
    }

    #[test]
    fn decodes_download_events() {
        let created = decode_download_created(&json!({
            "uuid": "u1",
            "browserContextId": "ctx-1",
            "pageTargetId": "t1",
            "frameId": "f1",
            "url": "https://example.com/file.zip",
            "suggestedFileName": "file.zip",
        }))
        .unwrap();
        assert_eq!(created.uuid, "u1");
        assert_eq!(created.browser_context_id.as_deref(), Some("ctx-1"));
        assert_eq!(created.suggested_file_name, "file.zip");

        let finished = decode_download_finished(&json!({
            "uuid": "u1",
            "canceled": false,
        }))
        .unwrap();
        assert_eq!(finished.uuid, "u1");
        assert!(!finished.canceled);
        assert!(finished.error.is_none());

        let failed = decode_download_finished(&json!({
            "uuid": "u2",
            "canceled": true,
            "error": "NS_BINDING_ABORTED",
        }))
        .unwrap();
        assert!(failed.canceled);
        assert_eq!(failed.error.as_deref(), Some("NS_BINDING_ABORTED"));
    }
}
