//! Input dispatch: mouse, keyboard, touch and wheel events.
//!
//! Typed surface over the Juggler `Page.dispatch*Event` / `Page.insertText`
//! commands. Events are synthesized at the browser level (no OS input
//! required), mirroring Playwright's Firefox input pipeline:
//!
//! - [`JugglerPage::click`] / [`JugglerPage::double_click`] /
//!   [`JugglerPage::mouse_*`] — `Page.dispatchMouseEvent`
//! - [`JugglerPage::type_text`] / [`JugglerPage::press_key`] /
//!   [`JugglerPage::key_*`] — `Page.dispatchKeyEvent` / `Page.insertText`
//! - [`JugglerPage::tap`] — `Page.dispatchTapEvent`
//! - [`JugglerPage::touch_*`] — `Page.dispatchTouchEvent`
//! - [`JugglerPage::wheel`] — `Page.dispatchWheelEvent`
//!
//! Modifier masks follow the DOM convention: Alt = 1, Ctrl = 2, Meta = 4,
//! Shift = 8 (see [`Modifiers`]).

use serde_json::Value;

use crate::connection::DEFAULT_COMMAND_TIMEOUT;
use crate::error::Result;
use crate::page::JugglerPage;

/// DOM modifier bit flags (event.modifiers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers(pub u64);

impl Modifiers {
    /// No modifiers.
    pub const NONE: Modifiers = Modifiers(0);
    /// Alt held.
    pub const ALT: Modifiers = Modifiers(1);
    /// Ctrl held.
    pub const CTRL: Modifiers = Modifiers(2);
    /// Meta (Cmd/Win) held.
    pub const META: Modifiers = Modifiers(4);
    /// Shift held.
    pub const SHIFT: Modifiers = Modifiers(8);

    /// Combines two modifier sets.
    pub const fn union(self, other: Modifiers) -> Modifiers {
        Modifiers(self.0 | other.0)
    }

    /// The wire value.
    pub const fn bits(self) -> u64 {
        self.0
    }
}

/// Mouse buttons (DOM button codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Left button (0).
    Left,
    /// Middle button (1).
    Middle,
    /// Right button (2).
    Right,
}

impl MouseButton {
    /// The DOM button code.
    pub const fn code(self) -> u64 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }
}

/// Parameters of a single `Page.dispatchMouseEvent` call.
#[derive(Debug, Clone)]
struct MouseEventParams<'a> {
    event_type: &'a str,
    x: f64,
    y: f64,
    button: MouseButton,
    modifiers: Modifiers,
    click_count: u64,
    buttons: u64,
}

/// A physical key description for `dispatchKeyEvent`.
#[derive(Debug, Clone)]
pub struct KeyDescriptor {
    /// `key` value (e.g. `a`, `Enter`, `ArrowLeft`).
    pub key: String,
    /// `code` value (e.g. `KeyA`, `Enter`).
    pub code: String,
    /// Virtual key code (e.g. 65 for `A`).
    pub key_code: u64,
    /// Key location: 0 standard, 1 left, 2 right, 3 numpad.
    pub location: u64,
    /// Whether the event comes from an auto-repeat.
    pub repeat: bool,
    /// Text to insert for generating-key events.
    pub text: Option<String>,
}

impl KeyDescriptor {
    /// Builds a descriptor for a plain printable key (single ASCII char).
    pub fn printable(ch: char) -> Self {
        let lower = ch.to_ascii_lowercase();
        Self {
            key: ch.to_string(),
            code: format!("Key{}", ch.to_ascii_uppercase()),
            key_code: 65 + (lower as u64 - 'a' as u64),
            location: 0,
            repeat: false,
            text: Some(ch.to_string()),
        }
    }

    /// Builds a descriptor for a named key from the common set.
    pub fn named(name: &str) -> Self {
        let (key, code, key_code) = match name {
            "Enter" | "Return" => ("Enter", "Enter", 13u64),
            "Tab" => ("Tab", "Tab", 9),
            "Escape" | "Esc" => ("Escape", "Escape", 27),
            "Backspace" => ("Backspace", "Backspace", 8),
            "Delete" | "Del" => ("Delete", "Delete", 46),
            "Insert" => ("Insert", "Insert", 45),
            "Home" => ("Home", "Home", 36),
            "End" => ("End", "End", 35),
            "PageUp" => ("PageUp", "PageUp", 33),
            "PageDown" => ("PageDown", "PageDown", 34),
            "ArrowUp" => ("ArrowUp", "ArrowUp", 38),
            "ArrowDown" => ("ArrowDown", "ArrowDown", 40),
            "ArrowLeft" => ("ArrowLeft", "ArrowLeft", 37),
            "ArrowRight" => ("ArrowRight", "ArrowRight", 39),
            "Space" => (" ", "Space", 32),
            "Shift" | "ShiftLeft" => ("Shift", "ShiftLeft", 16),
            "Control" | "ControlLeft" => ("Control", "ControlLeft", 17),
            "Alt" | "AltLeft" => ("Alt", "AltLeft", 18),
            "Meta" | "MetaLeft" => ("Meta", "MetaLeft", 224),
            "F1" => ("F1", "F1", 112),
            "F2" => ("F2", "F2", 113),
            "F3" => ("F3", "F3", 114),
            "F4" => ("F4", "F4", 115),
            "F5" => ("F5", "F5", 116),
            "F6" => ("F6", "F6", 117),
            "F7" => ("F7", "F7", 118),
            "F8" => ("F8", "F8", 119),
            "F9" => ("F9", "F9", 120),
            "F10" => ("F10", "F10", 121),
            "F11" => ("F11", "F11", 122),
            "F12" => ("F12", "F12", 123),
            other => (other, other, 0),
        };
        Self {
            key: key.to_string(),
            code: code.to_string(),
            key_code,
            location: if name.starts_with("Right") { 2 } else { 0 },
            repeat: false,
            text: if key_code == 32 {
                Some(" ".into())
            } else {
                None
            },
        }
    }
}

/// Key event type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyEventType {
    Down,
    Up,
    RawKeyDown,
}

impl KeyEventType {
    fn as_str(self) -> &'static str {
        match self {
            KeyEventType::Down => "keyDown",
            KeyEventType::Up => "keyUp",
            KeyEventType::RawKeyDown => "rawKeyDown",
        }
    }
}

/// Touch point for touch events.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TouchPoint {
    /// X coordinate (CSS pixels).
    pub x: f64,
    /// Y coordinate (CSS pixels).
    pub y: f64,
}

impl JugglerPage {
    /// Moves the mouse and presses the left button (mousedown+mouseup).
    pub async fn click(&self, x: f64, y: f64) -> Result<()> {
        self.click_with(x, y, MouseButton::Left, Modifiers::NONE)
            .await
    }

    /// Clicks with an explicit button and modifiers.
    pub async fn click_with(
        &self,
        x: f64,
        y: f64,
        button: MouseButton,
        modifiers: Modifiers,
    ) -> Result<()> {
        self.mouse_move(x, y).await?;
        self.mouse_down(x, y, button, modifiers, 1).await?;
        self.mouse_up(x, y, button, modifiers, 1).await?;
        Ok(())
    }

    /// Double-clicks at the position.
    pub async fn double_click(&self, x: f64, y: f64) -> Result<()> {
        self.mouse_move(x, y).await?;
        self.mouse_down(x, y, MouseButton::Left, Modifiers::NONE, 1)
            .await?;
        self.mouse_up(x, y, MouseButton::Left, Modifiers::NONE, 1)
            .await?;
        self.mouse_down(x, y, MouseButton::Left, Modifiers::NONE, 2)
            .await?;
        self.mouse_up(x, y, MouseButton::Left, Modifiers::NONE, 2)
            .await?;
        Ok(())
    }

    /// Dispatches a `mousedown` event.
    pub async fn mouse_down(
        &self,
        x: f64,
        y: f64,
        button: MouseButton,
        modifiers: Modifiers,
        click_count: u64,
    ) -> Result<()> {
        self.dispatch_mouse(MouseEventParams {
            event_type: "mousedown",
            x,
            y,
            button,
            modifiers,
            click_count,
            buttons: 1,
        })
        .await
    }

    /// Dispatches a `mousemove` event.
    pub async fn mouse_move(&self, x: f64, y: f64) -> Result<()> {
        self.dispatch_mouse(MouseEventParams {
            event_type: "mousemove",
            x,
            y,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
            click_count: 0,
            buttons: 0,
        })
        .await
    }

    /// Dispatches a `mouseup` event.
    pub async fn mouse_up(
        &self,
        x: f64,
        y: f64,
        button: MouseButton,
        modifiers: Modifiers,
        click_count: u64,
    ) -> Result<()> {
        self.dispatch_mouse(MouseEventParams {
            event_type: "mouseup",
            x,
            y,
            button,
            modifiers,
            click_count,
            buttons: 0,
        })
        .await
    }

    async fn dispatch_mouse(&self, event: MouseEventParams<'_>) -> Result<()> {
        let params = serde_json::json!({
            "type": event.event_type,
            "button": event.button.code(),
            "x": event.x,
            "y": event.y,
            "modifiers": event.modifiers.bits(),
            "clickCount": event.click_count,
            "buttons": event.buttons,
        });
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.dispatchMouseEvent",
                params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Dispatches a tap (mobile-style) at the position.
    pub async fn tap(&self, x: f64, y: f64) -> Result<()> {
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.dispatchTapEvent",
                serde_json::json!({"x": x, "y": y, "modifiers": 0}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Dispatches a touch event with the given points.
    pub async fn touch_event(
        &self,
        event_type: TouchEventType,
        points: &[TouchPoint],
    ) -> Result<bool> {
        let params = serde_json::json!({
            "type": event_type.as_str(),
            "touchPoints": points,
            "modifiers": 0,
        });
        let result = self
            .connection
            .send_command(
                Some(&self.session_id),
                "Page.dispatchTouchEvent",
                params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(result
            .get("defaultPrevented")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// Dispatches a wheel event (scroll).
    pub async fn wheel(&self, x: f64, y: f64, delta_x: f64, delta_y: f64) -> Result<()> {
        self.wheel_with(x, y, delta_x, delta_y, 0.0, Modifiers::NONE)
            .await
    }

    /// Wheel event with a delta-z axis and modifiers.
    pub async fn wheel_with(
        &self,
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
        delta_z: f64,
        modifiers: Modifiers,
    ) -> Result<()> {
        let params = serde_json::json!({
            "x": x,
            "y": y,
            "deltaX": delta_x,
            "deltaY": delta_y,
            "deltaZ": delta_z,
            "modifiers": modifiers.bits(),
        });
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.dispatchWheelEvent",
                params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Presses and releases a named key (e.g. `Enter`, `ArrowLeft`).
    pub async fn press_key(&self, name: &str) -> Result<()> {
        self.press_key_with(name, Modifiers::NONE).await
    }

    /// Presses and releases a named key with modifiers held.
    pub async fn press_key_with(&self, name: &str, modifiers: Modifiers) -> Result<()> {
        let descriptor = KeyDescriptor::named(name);
        // Modifiers are dispatched as their own down/up pair first.
        self.hold_modifiers(modifiers).await?;
        self.dispatch_key(KeyEventType::Down, &descriptor, modifiers)
            .await?;
        self.dispatch_key(KeyEventType::Up, &descriptor, modifiers)
            .await?;
        self.release_modifiers(modifiers).await?;
        Ok(())
    }

    /// Types text as keyboard input (printable characters only).
    ///
    /// Uses `Page.insertText` for fast bulk input; falls back per-character
    /// `dispatchKeyEvent` pairs for newline-ish input when needed.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.insertText",
                serde_json::json!({"text": text}),
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    /// Dispatches a raw key down event.
    pub async fn key_down(&self, descriptor: &KeyDescriptor) -> Result<()> {
        self.dispatch_key(KeyEventType::Down, descriptor, Modifiers::NONE)
            .await
    }

    /// Dispatches a raw key up event.
    pub async fn key_up(&self, descriptor: &KeyDescriptor) -> Result<()> {
        self.dispatch_key(KeyEventType::Up, descriptor, Modifiers::NONE)
            .await
    }

    async fn dispatch_key(
        &self,
        event_type: KeyEventType,
        descriptor: &KeyDescriptor,
        modifiers: Modifiers,
    ) -> Result<()> {
        let mut params = serde_json::json!({
            "type": event_type.as_str(),
            "key": descriptor.key,
            "keyCode": descriptor.key_code,
            "location": descriptor.location,
            "code": descriptor.code,
            "repeat": descriptor.repeat,
            "modifiers": modifiers.bits(),
        });
        if let Some(text) = &descriptor.text {
            params["text"] = Value::String(text.clone());
        }
        self.connection
            .send_command(
                Some(&self.session_id),
                "Page.dispatchKeyEvent",
                params,
                DEFAULT_COMMAND_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    async fn hold_modifiers(&self, modifiers: Modifiers) -> Result<()> {
        for (flag, descriptor) in [
            (Modifiers::ALT, KeyDescriptor::named("Alt")),
            (Modifiers::CTRL, KeyDescriptor::named("Control")),
            (Modifiers::META, KeyDescriptor::named("Meta")),
            (Modifiers::SHIFT, KeyDescriptor::named("Shift")),
        ] {
            if modifiers.0 & flag.0 != 0 {
                self.dispatch_key(KeyEventType::RawKeyDown, &descriptor, Modifiers::NONE)
                    .await?;
            }
        }
        Ok(())
    }

    async fn release_modifiers(&self, modifiers: Modifiers) -> Result<()> {
        for (flag, descriptor) in [
            (Modifiers::SHIFT, KeyDescriptor::named("Shift")),
            (Modifiers::META, KeyDescriptor::named("Meta")),
            (Modifiers::CTRL, KeyDescriptor::named("Control")),
            (Modifiers::ALT, KeyDescriptor::named("Alt")),
        ] {
            if modifiers.0 & flag.0 != 0 {
                self.dispatch_key(KeyEventType::Up, &descriptor, Modifiers::NONE)
                    .await?;
            }
        }
        Ok(())
    }
}

/// Touch event phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchEventType {
    /// Fingers placed.
    Start,
    /// Fingers lifted.
    End,
    /// Fingers moved.
    Move,
    /// Touch cancelled.
    Cancel,
}

impl TouchEventType {
    fn as_str(self) -> &'static str {
        match self {
            TouchEventType::Start => "touchStart",
            TouchEventType::End => "touchEnd",
            TouchEventType::Move => "touchMove",
            TouchEventType::Cancel => "touchCancel",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_flags() {
        assert_eq!(Modifiers::ALT.bits(), 1);
        assert_eq!(Modifiers::CTRL.bits(), 2);
        assert_eq!(Modifiers::META.bits(), 4);
        assert_eq!(Modifiers::SHIFT.bits(), 8);
        assert_eq!(Modifiers::CTRL.union(Modifiers::SHIFT).bits(), 10);
        assert_eq!(Modifiers::NONE.bits(), 0);
    }

    #[test]
    fn printable_key_descriptors() {
        let a = KeyDescriptor::printable('a');
        assert_eq!(a.key, "a");
        assert_eq!(a.code, "KeyA");
        assert_eq!(a.key_code, 65);
        assert_eq!(a.text.as_deref(), Some("a"));

        let z = KeyDescriptor::printable('Z');
        assert_eq!(z.code, "KeyZ");
        assert_eq!(z.key_code, 90);
    }

    #[test]
    fn named_key_descriptors() {
        let enter = KeyDescriptor::named("Enter");
        assert_eq!(enter.key, "Enter");
        assert_eq!(enter.key_code, 13);

        let esc = KeyDescriptor::named("Esc");
        assert_eq!(esc.key, "Escape");

        let space = KeyDescriptor::named("Space");
        assert_eq!(space.key, " ");
        assert_eq!(space.text.as_deref(), Some(" "));

        let right_shift = KeyDescriptor::named("RightShift");
        assert_eq!(right_shift.location, 2);

        // Unknown keys fall back to the name as-is.
        let unknown = KeyDescriptor::named("Pause");
        assert_eq!(unknown.key, "Pause");
        assert_eq!(unknown.key_code, 0);
    }

    #[test]
    fn mouse_button_codes() {
        assert_eq!(MouseButton::Left.code(), 0);
        assert_eq!(MouseButton::Middle.code(), 1);
        assert_eq!(MouseButton::Right.code(), 2);
    }

    #[test]
    fn touch_event_names() {
        assert_eq!(TouchEventType::Start.as_str(), "touchStart");
        assert_eq!(TouchEventType::End.as_str(), "touchEnd");
        assert_eq!(TouchEventType::Move.as_str(), "touchMove");
        assert_eq!(TouchEventType::Cancel.as_str(), "touchCancel");
    }
}
