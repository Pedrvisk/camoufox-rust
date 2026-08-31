//! BrowserForge fingerprint → Camoufox config mapping.
//!
//! This module holds the mapping table plus the conversion helpers
//! (`cast_to_properties`, `handle_screen_xy`, the Firefox version rewrite).
//!
//! Two behaviors matter here:
//!
//! 1. **Falsy values are skipped**: `0`, `""`, `false` and `null` never reach
//!    the config map (Camoufox applies its own defaults).
//! 2. **The Firefox version rewrite**: every string value goes through the
//!    `(?<!\d)(1[0-9]{2})(\.0)(?!\d)` rewrite, replacing stale generated
//!    Firefox versions (e.g. `132.0`) with the installed Camoufox version.

use serde_json::{Map, Value};

use veilus_fingerprint::BrowserProfile;

use crate::os::OsName;

/// Replaces `1xx.0` Firefox version segments with the given version.
///
/// Replaces stale `1xx.0` Firefox version segments. The `regex` crate has no
/// look-around, so this is a hand-rolled scanner over the pattern
/// `(?<!\d)(1[0-9]{2})(\.0)(?!\d)`.
pub fn replace_ff_version(value: &str, ff_version: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut i = 0usize;
    while i < chars.len() {
        let matches_pattern = chars[i] == '1'
            && i + 4 < chars.len()
            && chars[i + 1].is_ascii_digit()
            && chars[i + 2].is_ascii_digit()
            && chars[i + 3] == '.'
            && chars[i + 4] == '0'
            && (i == 0 || !chars[i - 1].is_ascii_digit())
            && (i + 5 >= chars.len() || !chars[i + 5].is_ascii_digit());
        if matches_pattern {
            out.push_str(ff_version);
            out.push_str(".0");
            i += 5;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn put_str(out: &mut Map<String, Value>, key: &str, value: Option<&str>, ff_version: Option<&str>) {
    // Empty strings are skipped.
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        let replaced = match ff_version {
            Some(ff) => replace_ff_version(value, ff),
            None => value.to_string(),
        };
        out.insert(key.to_string(), Value::String(replaced));
    }
}

fn put_num(out: &mut Map<String, Value>, key: &str, value: Option<f64>) {
    // Zeros are skipped.
    if let Some(value) = value.filter(|v| *v != 0.0) {
        let number = if value.fract() == 0.0 && value.abs() < 9.007_199_254_740_992e15 {
            serde_json::Number::from(value as i64)
        } else if let Some(n) = serde_json::Number::from_f64(value) {
            n
        } else {
            return;
        };
        out.insert(key.to_string(), Value::Number(number));
    }
}

fn put_bool(out: &mut Map<String, Value>, key: &str, value: Option<bool>) {
    // `false` is skipped.
    if let Some(value) = value.filter(|v| *v) {
        out.insert(key.to_string(), Value::Bool(value));
    }
}

/// Computes the window viewport `(inner_width, inner_height)`.
///
/// The generator can sample zero viewport dimensions; a real window always has
/// one, so a zero sample falls back to `outer` (or screen) minus typical
/// browser chrome (~40px vertical, ~16px horizontal).
fn window_viewport(screen: &veilus_fingerprint::ScreenFingerprint) -> (f64, f64) {
    let outer_w = screen.outer_width.unwrap_or(screen.width);
    let outer_h = screen.outer_height.unwrap_or(screen.avail_height.max(screen.height));

    let inner_w = if screen.inner_width > 0 {
        f64::from(screen.inner_width)
    } else {
        f64::from(outer_w.saturating_sub(16)).max(200.0)
    };
    let inner_h = if screen.inner_height > 0 {
        f64::from(screen.inner_height)
    } else {
        f64::from(outer_h.saturating_sub(40)).max(200.0)
    };
    (inner_w, inner_h)
}

/// Computes `window.screenY` from the fingerprint screen data.
///
/// `window.screenY` is never produced by the mapping table, so this always runs.
pub fn handle_screen_xy(out: &mut Map<String, Value>, screen: &veilus_fingerprint::ScreenFingerprint) {
    use rand::Rng;

    if out.contains_key("window.screenY") {
        return;
    }

    let screen_x = screen.screen_x.unwrap_or(0);
    if screen_x == 0 {
        out.insert("window.screenX".into(), Value::from(0));
        out.insert("window.screenY".into(), Value::from(0));
        return;
    }
    if (-50..=50).contains(&screen_x) {
        out.insert("window.screenY".into(), Value::from(screen_x));
        return;
    }

    let avail_height = screen.avail_height as i64;
    // A missing outerHeight falls back to availHeight, yielding a deterministic 0.
    let outer_height = screen
        .outer_height
        .map(u64::from)
        .unwrap_or(u64::from(screen.avail_height)) as i64;
    let screen_y = avail_height - outer_height;
    let value = if screen_y == 0 {
        0
    } else if screen_y > 0 {
        rand::thread_rng().gen_range(0..=screen_y)
    } else {
        rand::thread_rng().gen_range(screen_y..=0)
    };
    out.insert("window.screenY".into(), Value::from(value));
}

/// Converts a generated BrowserForge-compatible profile into the Camoufox
/// config map.
pub fn cast_to_properties(
    profile: &BrowserProfile,
    ff_version: Option<&str>,
    out: &mut Map<String, Value>,
) {
    let fp = &profile.fingerprint;
    let nav = &fp.navigator;
    let screen = &fp.screen;

    // navigator.*
    put_str(out, "navigator.userAgent", Some(&nav.user_agent), ff_version);
    put_str(out, "navigator.doNotTrack", nav.do_not_track.as_deref(), ff_version);
    put_str(out, "navigator.appCodeName", nav.app_code_name.as_deref(), ff_version);
    put_str(out, "navigator.appName", nav.app_name.as_deref(), ff_version);
    put_str(out, "navigator.appVersion", nav.app_version.as_deref(), ff_version);
    put_str(out, "navigator.oscpu", nav.oscpu.as_deref(), ff_version);
    put_str(out, "navigator.platform", Some(&nav.platform), ff_version);
    put_num(out, "navigator.hardwareConcurrency", Some(f64::from(nav.hardware_concurrency)));
    put_str(out, "navigator.product", nav.product.as_deref(), ff_version);
    put_num(out, "navigator.maxTouchPoints", nav.max_touch_points.map(f64::from));
    if let Some(extra) = &nav.extra_properties {
        put_bool(out, "navigator.globalPrivacyControl", extra.global_privacy_control);
    }

    // screen.* / window.* dimensions
    put_num(out, "screen.availLeft", screen.avail_left.map(f64::from));
    put_num(out, "screen.availTop", screen.avail_top.map(f64::from));
    put_num(out, "screen.availWidth", Some(f64::from(screen.avail_width)));
    put_num(out, "screen.availHeight", Some(f64::from(screen.avail_height)));
    put_num(out, "screen.height", Some(f64::from(screen.height)));
    put_num(out, "screen.width", Some(f64::from(screen.width)));
    put_num(out, "screen.colorDepth", Some(f64::from(screen.color_depth)));
    put_num(out, "screen.pixelDepth", Some(f64::from(screen.pixel_depth)));
    put_num(out, "screen.pageXOffset", screen.page_x_offset.map(f64::from));
    put_num(out, "screen.pageYOffset", screen.page_y_offset.map(f64::from));
    put_num(out, "window.outerHeight", screen.outer_height.map(f64::from));
    put_num(out, "window.outerWidth", screen.outer_width.map(f64::from));
    // A window always has a viewport; a zero sample falls back to the screen
    // dimensions minus typical browser chrome.
    let (inner_w, inner_h) = window_viewport(screen);
    put_num(out, "window.innerHeight", Some(inner_h));
    put_num(out, "window.innerWidth", Some(inner_w));
    put_num(out, "window.screenX", screen.screen_x.map(f64::from));

    // headers.*
    put_str(out, "headers.Accept-Encoding", profile.headers.get("Accept-Encoding").map(String::as_str), None);

    // battery:*
    if let Some(battery) = &fp.battery {
        put_bool(out, "battery:charging", Some(battery.charging));
        put_num(out, "battery:chargingTime", battery.charging_time);
        put_num(out, "battery:dischargingTime", battery.discharging_time);
    }

    handle_screen_xy(out, screen);
}

/// Convenience wrapper returning a fresh config map.
pub fn from_browserforge(profile: &BrowserProfile, ff_version: Option<&str>) -> Map<String, Value> {
    let mut out = Map::new();
    cast_to_properties(profile, ff_version, &mut out);
    out
}

/// Determines the target OS from a user agent (`determineUAOS`).
///
/// Structural matching: `Windows` → win, `Macintosh`/`Mac OS` → mac,
/// anything else → lin (Android/iOS map to `lin` too).
pub fn determine_ua_os(user_agent: &str) -> crate::error::Result<OsName> {
    if user_agent.trim().is_empty() {
        return Err(CamoufoxError::Io("Could not determine OS from user agent".into()));
    }
    if user_agent.contains("Windows") {
        return Ok(OsName::Win);
    }
    if user_agent.contains("Macintosh") || user_agent.contains("Mac OS") {
        return Ok(OsName::Mac);
    }
    Ok(OsName::Lin)
}

use crate::error::CamoufoxError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ff_version_replacement() {
        assert_eq!(replace_ff_version("Firefox/132.0", "115"), "Firefox/115.0");
        assert_eq!(replace_ff_version("rv:132.0)", "115"), "rv:115.0)");
        // Not surrounded by digits boundaries.
        assert_eq!(replace_ff_version("1132.0", "115"), "1132.0");
        assert_eq!(replace_ff_version("132.01", "115"), "132.01");
        assert_eq!(replace_ff_version("32.0", "115"), "32.0");
        assert_eq!(replace_ff_version("132.1", "115"), "132.1");
    }

    #[test]
    fn ua_os_detection() {
        assert_eq!(
            determine_ua_os("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:132.0) Gecko/20100101 Firefox/132.0").unwrap(),
            OsName::Win
        );
        assert_eq!(
            determine_ua_os("Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:132.0) Gecko/20100101 Firefox/132.0").unwrap(),
            OsName::Mac
        );
        assert_eq!(
            determine_ua_os("Mozilla/5.0 (X11; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0").unwrap(),
            OsName::Lin
        );
    }

    #[test]
    fn screen_xy_zero_and_small_offsets() {
        let mut screen = default_screen();
        screen.screen_x = Some(0);
        let mut out = Map::new();
        handle_screen_xy(&mut out, &screen);
        assert_eq!(out.get("window.screenX"), Some(&Value::from(0)));
        assert_eq!(out.get("window.screenY"), Some(&Value::from(0)));

        screen.screen_x = Some(30);
        let mut out = Map::new();
        handle_screen_xy(&mut out, &screen);
        assert_eq!(out.get("window.screenY"), Some(&Value::from(30)));
    }

    #[test]
    fn screen_xy_large_offset_is_random_in_range() {
        let mut screen = default_screen();
        screen.screen_x = Some(500);
        screen.avail_height = 1040;
        screen.outer_height = Some(1000);
        for _ in 0..20 {
            let mut out = Map::new();
            handle_screen_xy(&mut out, &screen);
            let y = out.get("window.screenY").unwrap().as_i64().unwrap();
            assert!((0..=40).contains(&y));
        }
    }

    fn default_screen() -> veilus_fingerprint::ScreenFingerprint {
        veilus_fingerprint::ScreenFingerprint {
            width: 1920,
            height: 1080,
            avail_width: 1920,
            avail_height: 1040,
            color_depth: 24,
            pixel_depth: 24,
            device_pixel_ratio: 1.0,
            inner_width: 1900,
            inner_height: 1000,
            avail_top: Some(0),
            avail_left: Some(0),
            outer_width: Some(1920),
            outer_height: Some(1040),
            screen_x: None,
            page_x_offset: None,
            page_y_offset: None,
            client_width: None,
            client_height: None,
            has_hdr: None,
        }
    }
}
