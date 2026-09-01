//! Fingerprint generation and Camoufox conversion.
//!
//! Generation is backed by the [`veilus_fingerprint`] crate — a Rust,
//! BrowserForge-compatible Bayesian-network generator — constrained to
//! **Firefox** profiles, as required by Camoufox.

use rand::Rng;
use serde_json::{Map, Value};
use veilus_fingerprint::{BrowserFamily, BrowserProfile, FingerprintGenerator};

use crate::error::{CamoufoxError, Result};
use crate::mappings::browserforge::from_browserforge;
use crate::mappings::warnings;
use crate::os::SupportedOs;

/// Constraints for the screen dimensions of the generated fingerprint.
///
/// Constraints are applied via bounded rejection sampling.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScreenConstraints {
    /// Minimum screen width.
    pub min_width: Option<u32>,
    /// Maximum screen width.
    pub max_width: Option<u32>,
    /// Minimum screen height.
    pub min_height: Option<u32>,
    /// Maximum screen height.
    pub max_height: Option<u32>,
}

impl ScreenConstraints {
    fn satisfied_by(&self, screen: &veilus_fingerprint::ScreenFingerprint) -> bool {
        if let Some(min) = self.min_width {
            if screen.width < min {
                return false;
            }
        }
        if let Some(max) = self.max_width {
            if screen.width > max {
                return false;
            }
        }
        if let Some(min) = self.min_height {
            if screen.height < min {
                return false;
            }
        }
        if let Some(max) = self.max_height {
            if screen.height > max {
                return false;
            }
        }
        true
    }
}

/// Input for [`generate_fingerprint`].
#[derive(Debug, Clone, Default)]
pub struct FingerprintRequest {
    /// Fixed window size `(width, height)`; the screen fingerprint is adjusted
    /// around it.
    pub window: Option<(u32, u32)>,
    /// OS constraints; `None` picks from the full [`crate::os::SUPPORTED_OS`]
    /// distribution.
    pub operating_systems: Option<Vec<SupportedOs>>,
    /// Screen dimension constraints (rejection sampling).
    pub screen: Option<ScreenConstraints>,
    /// Deterministic seed, for session-stable identities.
    pub seed: Option<u64>,
}

/// Maximum attempts when rejection-sampling screen constraints.
const MAX_SCREEN_ATTEMPTS: usize = 100;

/// Maximum attempts when rejection-sampling for a Firefox header resolution.
///
/// The generator samples the header and fingerprint Bayesian networks
/// independently, so the fingerprint network's user agent frequently disagrees
/// with the requested browser. The fix is to retry until the **header network**
/// resolves Firefox (~10% hit rate), then reconcile the fingerprint network
/// fields with the header user agent.
const MAX_BROWSER_ATTEMPTS: usize = 200;

/// Reconciles the fingerprint network fields with the header network's
/// Firefox user agent.
///
/// The fingerprint network samples its user agent independently from the
/// header network and frequently disagrees with the requested browser. A
/// Chrome user agent inside a Firefox browser is an immediate detection
/// signal, so the fingerprint fields are rebuilt around the header UA:
///
/// - `userAgent` is taken from the headers verbatim
/// - `oscpu` is derived from the UA platform segment
/// - `platform` is derived from the UA platform
/// - `appVersion` is the UA minus the `Mozilla/` prefix
///
/// Screen, battery and the remaining fields stay as sampled — they do not
/// depend on the browser family.
fn reconcile_with_header_ua(profile: &mut BrowserProfile) {
    let Some(header_ua) = profile
        .headers
        .get("user-agent")
        .or_else(|| profile.headers.get("User-Agent"))
        .cloned()
    else {
        return;
    };
    if !header_ua.contains("Firefox") {
        return;
    }

    let nav = &mut profile.fingerprint.navigator;
    nav.user_agent = header_ua.clone();

    // platform segment: "(Windows NT 10.0; Win64; x64)", "(X11; Linux x86_64)",
    // "(Macintosh; Intel Mac OS X 10.15)"
    // The rv token is not part of the platform; it is re-appended below.
    let platform_segment = header_ua
        .split('(')
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .unwrap_or("")
        .split("; rv:")
        .next()
        .unwrap_or("")
        .trim_end_matches(';')
        .to_string();

    if !platform_segment.is_empty() {
        // oscpu = platform segment with rv info appended
        let rv = header_ua
            .split("rv:")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .map(|v| format!("; rv:{v}"))
            .unwrap_or_default();
        nav.oscpu = Some(format!("{platform_segment}{rv}"));

        nav.platform = if platform_segment.contains("Windows") {
            "Win32".to_string()
        } else if platform_segment.contains("Mac") {
            "MacIntel".to_string()
        } else {
            "Linux x86_64".to_string()
        };

        nav.app_version = Some(
            header_ua
                .strip_prefix("Mozilla/")
                .map(|rest| rest.to_string())
                .unwrap_or_else(|| header_ua.clone()),
        );
    }
}

/// Generates a Firefox fingerprint.
pub fn generate_fingerprint(request: &FingerprintRequest) -> Result<BrowserProfile> {
    let os_choices: Vec<SupportedOs> = request
        .operating_systems
        .clone()
        .unwrap_or_else(|| crate::os::SUPPORTED_OS.to_vec());

    let mut attempt: u64 = 0;
    loop {
        let os = if os_choices.len() == 1 {
            os_choices[0]
        } else {
            os_choices[rand::thread_rng().gen_range(0..os_choices.len())]
        };

        let mut generator = FingerprintGenerator::new()
            .browser(BrowserFamily::Firefox)
            .os(os.to_family());
        if let Some(seed) = request.seed {
            generator = generator.seeded(seed.wrapping_add(attempt));
        }

        let mut profile = generator
            .generate()
            .map_err(|e| CamoufoxError::Fingerprint(e.to_string()))?;

        // The header network must have resolved an actual Firefox user agent
        // (browser family alone can disagree with the header UA). When it
        // does, the fingerprint fields are aligned with it.
        let header_ua = profile
            .headers
            .get("user-agent")
            .or_else(|| profile.headers.get("User-Agent"))
            .cloned();
        let header_resolved = header_ua
            .as_deref()
            .is_some_and(|ua| ua.contains("Firefox"));
        // Desktop-only: reject mobile/tablet UAs (Camoufox is a desktop
        // browser; a mobile UA with desktop APIs is a detection signal).
        let desktop = header_ua
            .as_deref()
            .map(|ua| !ua.contains("Mobile") && !ua.contains("Tablet"))
            .unwrap_or(true);
        // The UA platform must match the requested OS. The generator silently
        // relaxes constraints when its retry budget is exhausted, so the
        // platform is verified here rather than trusted.
        let platform_matches = header_ua.as_deref().map(|ua| match os {
            SupportedOs::Windows => ua.contains("Windows"),
            SupportedOs::Macos => ua.contains("Mac OS") || ua.contains("Macintosh"),
            SupportedOs::Linux => ua.contains("Linux") || ua.contains("X11") || ua.contains("Ubuntu"),
        }).unwrap_or(true);
        // Desktop screens: reject portrait/mobile-sized samples, which the
        // fingerprint network can emit even under a desktop header UA.
        let screen = &profile.fingerprint.screen;
        let desktop_screen =
            screen.width >= 800 && screen.height >= 600 && screen.width >= screen.height;
        let header_resolved = header_resolved && desktop && platform_matches && desktop_screen;
        if header_resolved {
            // Keep profile.browser.family consistent with the header UA.
            profile.browser.family = BrowserFamily::Firefox;
            reconcile_with_header_ua(&mut profile);
        }

        let fits = request
            .screen
            .as_ref()
            .map(|c| c.satisfied_by(&profile.fingerprint.screen))
            .unwrap_or(true);
        let browser_exhausted = attempt >= MAX_BROWSER_ATTEMPTS as u64;
        let screen_exhausted = attempt >= MAX_SCREEN_ATTEMPTS as u64;

        if (header_resolved && fits)
            || (browser_exhausted && fits)
            || screen_exhausted
        {
            let profile = match request.window {
                Some((width, height)) => handle_window_size(profile, width, height),
                None => profile,
            };
            return Ok(profile);
        }
        attempt += 1;
    }
}

/// Adjusts the fingerprint screen around a fixed window size.
pub fn handle_window_size(mut profile: BrowserProfile, outer_width: u32, outer_height: u32) -> BrowserProfile {
    let screen = &mut profile.fingerprint.screen;

    let centered_x = (i64::from(screen.width) - i64::from(outer_width)).div_euclid(2);
    screen.screen_x = Some(screen.screen_x.unwrap_or(0) + centered_x as i32);

    // `screenX` drives `window.screenX`; `window.screenY` is derived later by
    // `handle_screen_xy`, so no `screenY` is stashed here.

    if screen.inner_width > 0 {
        let old_outer = u64::from(screen.outer_width.unwrap_or(outer_width));
        let inner = (u64::from(outer_width) + u64::from(screen.inner_width)).saturating_sub(old_outer);
        screen.inner_width = inner as u32;
    }
    if screen.inner_height > 0 {
        let old_outer = u64::from(screen.outer_height.unwrap_or(outer_height));
        let inner = (u64::from(outer_height) + u64::from(screen.inner_height)).saturating_sub(old_outer);
        screen.inner_height = inner as u32;
    }

    screen.outer_width = Some(outer_width);
    screen.outer_height = Some(outer_height);
    profile
}

/// Converts a profile into the Camoufox config map
/// into the Camoufox config map.
pub fn from_browserforge_convert(profile: &BrowserProfile, ff_version: Option<&str>) -> Map<String, Value> {
    from_browserforge(profile, ff_version)
}

/// Validates a user-supplied fingerprint (`checkCustomFingerprint`).
///
/// Non-Firefox fingerprints are rejected: Camoufox is a Firefox fork and
/// mismatched engine fingerprints lead to detection.
pub fn check_custom_fingerprint(profile: &BrowserProfile) -> Result<()> {
    if profile.browser.family != BrowserFamily::Firefox {
        return Err(CamoufoxError::NonFirefoxFingerprint(format!(
            "\"{}\" fingerprints are not supported in Camoufox. Using fingerprints from a browser \
             other than Firefox WILL lead to detection. If this is intentional, pass \
             i_know_what_im_doing=true.",
            profile.browser.name
        )));
    }
    warnings::warn_leak("custom_fingerprint", Some(false));
    Ok(())
}

/// Re-exported so callers don't need to depend on `veilus-fingerprint` directly
/// unless they want to.
pub use crate::mappings::browserforge::{determine_ua_os, handle_screen_xy, replace_ff_version};
pub use veilus_fingerprint;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::OsName;

    #[test]
    fn generates_firefox_profiles_for_each_os() {
        for os in crate::os::SUPPORTED_OS {
            let profile = generate_fingerprint(&FingerprintRequest {
                operating_systems: Some(vec![*os]),
                seed: Some(42),
                ..Default::default()
            })
            .unwrap();
            assert!(profile.fingerprint.navigator.user_agent.contains("Firefox"));
            assert!(!profile.fingerprint.navigator.webdriver);
        }
    }

    #[test]
    fn window_size_is_applied() {
        let profile = generate_fingerprint(&FingerprintRequest {
            window: Some((1280, 720)),
            seed: Some(7),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(profile.fingerprint.screen.outer_width, Some(1280));
        assert_eq!(profile.fingerprint.screen.outer_height, Some(720));
    }

    #[test]
    fn screen_constraints_are_respected() {
        let profile = generate_fingerprint(&FingerprintRequest {
            screen: Some(ScreenConstraints {
                max_width: Some(1920),
                max_height: Some(1080),
                ..Default::default()
            }),
            seed: Some(99),
            ..Default::default()
        })
        .unwrap();
        assert!(profile.fingerprint.screen.width <= 1920);
        assert!(profile.fingerprint.screen.height <= 1080);
    }

    #[test]
    fn conversion_contains_expected_keys() {
        let profile = generate_fingerprint(&FingerprintRequest {
            seed: Some(1),
            ..Default::default()
        })
        .unwrap();
        let config = from_browserforge_convert(&profile, None);
        for key in [
            "navigator.userAgent",
            "navigator.platform",
            "navigator.hardwareConcurrency",
            "screen.width",
            "screen.height",
            "window.innerWidth",
            "window.innerHeight",
        ] {
            assert!(config.contains_key(key), "missing key {key}");
        }
        assert!(config.contains_key("window.screenY"));
    }

    #[test]
    fn conversion_applies_ff_version() {
        let mut profile = generate_fingerprint(&FingerprintRequest {
            seed: Some(3),
            ..Default::default()
        })
        .unwrap();
        // Force a recognizable UA segment.
        profile.fingerprint.navigator.user_agent =
            "Mozilla/5.0 (X11; Linux x86_64; rv:132.0) Gecko/20100101 Firefox/132.0".into();
        let config = from_browserforge_convert(&profile, Some("115"));
        assert_eq!(
            config.get("navigator.userAgent").unwrap().as_str().unwrap(),
            "Mozilla/5.0 (X11; Linux x86_64; rv:115.0) Gecko/20100101 Firefox/115.0"
        );
    }

    #[test]
    fn custom_fingerprint_must_be_firefox() {
        let firefox = generate_fingerprint(&FingerprintRequest {
            seed: Some(5),
            ..Default::default()
        })
        .unwrap();
        assert!(check_custom_fingerprint(&firefox).is_ok());

        let mut chrome_ish = firefox.clone();
        chrome_ish.browser.family = BrowserFamily::Chrome;
        chrome_ish.browser.name = "Chrome".into();
        let err = check_custom_fingerprint(&chrome_ish).unwrap_err();
        assert_eq!(err.name(), "NonFirefoxFingerprint");
    }

    #[test]
    fn ua_os_detection_reexport() {
        assert_eq!(
            determine_ua_os("Mozilla/5.0 (Windows NT 10.0; rv:132.0) Gecko/20100101 Firefox/132.0")
                .unwrap(),
            OsName::Win
        );
    }
}
