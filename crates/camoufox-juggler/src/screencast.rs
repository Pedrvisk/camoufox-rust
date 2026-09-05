//! Screencast streaming: JPEG frames of the page viewport.
//!
//! [`crate::page::JugglerPage::start_screencast`] starts the capture and
//! [`crate::page::JugglerPage::screencast_frames`] delivers
//! [`ScreencastFrame`]s (decoded JPEG bytes). Frames are acked
//! automatically (`Page.screencastFrameAck`), mirroring Playwright's
//! Firefox pipeline, so the browser keeps producing frames while a
//! subscriber is attached.

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

/// `Page.startScreencast` params.
pub(crate) fn start_screencast(width: u64, height: u64, quality: u64) -> Value {
    serde_json::json!({ "width": width, "height": height, "quality": quality })
}

/// One captured frame.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScreencastFrame {
    /// JPEG image bytes.
    pub data: Vec<u8>,
    /// Viewport width in device pixels.
    pub device_width: u64,
    /// Viewport height in device pixels.
    pub device_height: u64,
    /// Monotonic timestamp (seconds) — anchor to the wall clock yourself
    /// if needed (see Playwright's clock-offset approach).
    pub timestamp: f64,
}

/// Decodes a `Page.screencastFrame` payload.
pub(crate) fn decode_frame(params: &Value) -> Option<ScreencastFrame> {
    use base64::Engine;
    let data = params.get("data")?.as_str()?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some(ScreencastFrame {
        data,
        device_width: params
            .get("deviceWidth")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        device_height: params
            .get("deviceHeight")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        timestamp: params
            .get("timestamp")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
    })
}

/// The screencast frame stream for a page session.
///
/// Obtained from [`crate::page::JugglerPage::screencast_frames`].
pub struct ScreencastFrames {
    frames: UnboundedReceiver<ScreencastFrame>,
}

impl ScreencastFrames {
    pub(crate) fn new(frames: UnboundedReceiver<ScreencastFrame>) -> Self {
        Self { frames }
    }

    /// Waits for the next frame.
    ///
    /// `None` means the session ended.
    pub async fn next(&mut self) -> Option<ScreencastFrame> {
        self.frames.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_start_params() {
        let params = start_screencast(800, 600, 90);
        assert_eq!(params["width"], 800);
        assert_eq!(params["height"], 600);
        assert_eq!(params["quality"], 90);
    }

    #[test]
    fn decodes_frames() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]);
        let frame = decode_frame(&json!({
            "data": encoded,
            "deviceWidth": 1280,
            "deviceHeight": 720,
            "timestamp": 12.5,
        }))
        .unwrap();
        assert_eq!(frame.data, vec![1u8, 2, 3]);
        assert_eq!(frame.device_width, 1280);
        assert_eq!(frame.device_height, 720);
        assert_eq!(frame.timestamp, 12.5);
    }

    #[test]
    fn rejects_malformed_frames() {
        assert!(decode_frame(&json!({})).is_none());
        assert!(decode_frame(&json!({"data": "!!not-base64!!"})).is_none());
    }
}
