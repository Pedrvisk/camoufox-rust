//! Emulation: touch capability and emulated media.
//!
//! - [`crate::page::JugglerPage::set_touch_override`] /
//!   [`crate::browser::JugglerBrowser::set_touch_override`] toggle the
//!   touch capability of a browser context (`Browser.setTouchOverride`),
//!   making content behave as a touch device (`pointer: coarse`, touch
//!   events in the DOM) — the missing piece for full mobile emulation
//!   next to [`crate::input`]'s `tap`/`touch_event` dispatch.
//! - [`crate::page::JugglerPage::emulate_media`] emulates media type,
//!   color scheme, reduced motion, forced colors and contrast
//!   (`Page.setEmulatedMedia`), like Playwright's `page.emulateMedia`.

use serde_json::Value;

/// Emulated media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    /// Standard screen media.
    Screen,
    /// Print media (`@media print` rules apply).
    Print,
    /// Resets the media type to the browser default (empty string).
    Reset,
}

impl MediaType {
    fn as_str(self) -> &'static str {
        match self {
            MediaType::Screen => "screen",
            MediaType::Print => "print",
            MediaType::Reset => "",
        }
    }
}

/// Preferred color scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    /// Dark mode.
    Dark,
    /// Light mode.
    Light,
    /// No preference (default).
    NoPreference,
}

impl ColorScheme {
    fn as_str(self) -> &'static str {
        match self {
            ColorScheme::Dark => "dark",
            ColorScheme::Light => "light",
            ColorScheme::NoPreference => "no-preference",
        }
    }
}

/// Reduced motion preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReducedMotion {
    /// User prefers reduced motion.
    Reduce,
    /// No preference (default).
    NoPreference,
}

impl ReducedMotion {
    fn as_str(self) -> &'static str {
        match self {
            ReducedMotion::Reduce => "reduce",
            ReducedMotion::NoPreference => "no-preference",
        }
    }
}

/// Forced colors mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForcedColors {
    /// Forced colors active (high contrast).
    Active,
    /// Forced colors off (default).
    None,
}

impl ForcedColors {
    fn as_str(self) -> &'static str {
        match self {
            ForcedColors::Active => "active",
            ForcedColors::None => "none",
        }
    }
}

/// Contrast preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contrast {
    /// Less contrast.
    Less,
    /// More contrast.
    More,
    /// Custom contrast.
    Custom,
    /// No preference (default).
    NoPreference,
}

impl Contrast {
    fn as_str(self) -> &'static str {
        match self {
            Contrast::Less => "less",
            Contrast::More => "more",
            Contrast::Custom => "custom",
            Contrast::NoPreference => "no-preference",
        }
    }
}

/// `Page.setEmulatedMedia` settings; unset fields are omitted from the
/// command (left at their current emulation).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmulatedMedia {
    /// Media type (`screen`/`print`, or reset).
    pub media_type: Option<MediaType>,
    /// Preferred color scheme.
    pub color_scheme: Option<ColorScheme>,
    /// Reduced motion preference.
    pub reduced_motion: Option<ReducedMotion>,
    /// Forced colors mode.
    pub forced_colors: Option<ForcedColors>,
    /// Contrast preference.
    pub contrast: Option<Contrast>,
}

impl EmulatedMedia {
    /// Emulates dark mode.
    pub fn dark_mode() -> Self {
        Self {
            color_scheme: Some(ColorScheme::Dark),
            ..Default::default()
        }
    }

    /// Emulates print media.
    pub fn print() -> Self {
        Self {
            media_type: Some(MediaType::Print),
            ..Default::default()
        }
    }

    /// Resets every emulated aspect back to browser defaults.
    pub fn reset() -> Self {
        Self {
            media_type: Some(MediaType::Reset),
            color_scheme: Some(ColorScheme::NoPreference),
            reduced_motion: Some(ReducedMotion::NoPreference),
            forced_colors: Some(ForcedColors::None),
            contrast: Some(Contrast::NoPreference),
        }
    }

    /// Sets the media type (builder style).
    pub const fn with_media_type(mut self, media_type: MediaType) -> Self {
        self.media_type = Some(media_type);
        self
    }

    /// Sets the color scheme (builder style).
    pub const fn with_color_scheme(mut self, color_scheme: ColorScheme) -> Self {
        self.color_scheme = Some(color_scheme);
        self
    }

    /// Sets the reduced motion preference (builder style).
    pub const fn with_reduced_motion(mut self, reduced_motion: ReducedMotion) -> Self {
        self.reduced_motion = Some(reduced_motion);
        self
    }

    /// Sets the forced colors mode (builder style).
    pub const fn with_forced_colors(mut self, forced_colors: ForcedColors) -> Self {
        self.forced_colors = Some(forced_colors);
        self
    }

    /// Sets the contrast preference (builder style).
    pub const fn with_contrast(mut self, contrast: Contrast) -> Self {
        self.contrast = Some(contrast);
        self
    }

    /// Serializes into `Page.setEmulatedMedia` params.
    pub(crate) fn to_params(self) -> Value {
        let mut params = serde_json::json!({});
        if let Some(media_type) = self.media_type {
            params["type"] = Value::String(media_type.as_str().to_string());
        }
        if let Some(color_scheme) = self.color_scheme {
            params["colorScheme"] = Value::String(color_scheme.as_str().to_string());
        }
        if let Some(reduced_motion) = self.reduced_motion {
            params["reducedMotion"] = Value::String(reduced_motion.as_str().to_string());
        }
        if let Some(forced_colors) = self.forced_colors {
            params["forcedColors"] = Value::String(forced_colors.as_str().to_string());
        }
        if let Some(contrast) = self.contrast {
            params["contrast"] = Value::String(contrast.as_str().to_string());
        }
        params
    }
}

/// `Browser.setTouchOverride` params.
pub(crate) fn touch_override(browser_context_id: Option<&str>, has_touch: Option<bool>) -> Value {
    let mut params = serde_json::json!({});
    if let Some(id) = browser_context_id {
        params["browserContextId"] = Value::String(id.to_string());
    }
    // `null` clears the override; omitting keeps it unchanged.
    params["hasTouch"] = match has_touch {
        Some(has_touch) => Value::Bool(has_touch),
        None => Value::Null,
    };
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_emulated_media_params() {
        let media = EmulatedMedia::dark_mode().with_media_type(MediaType::Print);
        let params = media.to_params();
        assert_eq!(params["type"], "print");
        assert_eq!(params["colorScheme"], "dark");
        assert!(params.get("reducedMotion").is_none());

        let params = EmulatedMedia::default().to_params();
        assert!(params.as_object().unwrap().is_empty());
    }

    #[test]
    fn reset_sends_all_defaults() {
        let params = EmulatedMedia::reset().to_params();
        assert_eq!(params["type"], "");
        assert_eq!(params["colorScheme"], "no-preference");
        assert_eq!(params["reducedMotion"], "no-preference");
        assert_eq!(params["forcedColors"], "none");
        assert_eq!(params["contrast"], "no-preference");
    }

    #[test]
    fn builds_touch_override_params() {
        let params = touch_override(Some("ctx-1"), Some(true));
        assert_eq!(params["browserContextId"], "ctx-1");
        assert_eq!(params["hasTouch"], true);

        let params = touch_override(None, None);
        assert!(params.get("browserContextId").is_none());
        assert!(params["hasTouch"].is_null());
    }

    #[test]
    fn presets() {
        assert_eq!(EmulatedMedia::print().media_type, Some(MediaType::Print));
        assert_eq!(
            EmulatedMedia::dark_mode().color_scheme,
            Some(ColorScheme::Dark)
        );
        let custom = EmulatedMedia::default()
            .with_reduced_motion(ReducedMotion::Reduce)
            .with_forced_colors(ForcedColors::Active)
            .with_contrast(Contrast::More);
        assert_eq!(custom.to_params()["reducedMotion"], "reduce");
        assert_eq!(custom.to_params()["forcedColors"], "active");
        assert_eq!(custom.to_params()["contrast"], "more");
    }
}
