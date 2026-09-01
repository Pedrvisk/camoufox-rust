//! End-to-end fingerprint injection verification.
//!
//! Launches the real browser through the native Juggler driver, reads the
//! spoofed surfaces from a live page and asserts they match the generated
//! fingerprint. Skips (with a notice) when the browser is not installed —
//! run `camoufox fetch` first.

use camoufox::builder::{HeadlessMode, LaunchOptions};
use camoufox_core::os::SupportedOs;

fn browser_installed() -> bool {
    use std::path::PathBuf;
    let install = std::env::var_os("CAMOUFOX_INSTALL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".cache").join("camoufox")
        });
    install.join("camoufox-bin").exists() || install.join("camoufox").exists()
}

#[tokio::test]
async fn fingerprint_injection_matches_generated_identity() {
    if !browser_installed() {
        eprintln!("skipping: Camoufox not installed (run `camoufox fetch`)");
        return;
    }

    let options = LaunchOptions {
        os: vec![SupportedOs::Linux],
        headless: HeadlessMode::On,
        ..Default::default()
    };

    let mut browser = match camoufox_juggler::launch_with_juggler(&options).await {
        Ok(browser) => browser,
        Err(e) => panic!("launch failed: {e}"),
    };

    let (version, user_agent) = browser.info().await.expect("Browser.getInfo");
    assert!(!version.is_empty(), "empty browser version");
    assert!(user_agent.contains("Firefox"), "unexpected UA {user_agent}");

    let page = browser.new_page().await.expect("new page");
    page.goto("about:blank").await.expect("goto about:blank");

    let report = camoufox_juggler::verify_fingerprint(&page, &browser.prepared.config)
        .await
        .expect("verification");

    // At minimum: UA, platform, oscpu, screen dimensions and hardware
    // concurrency are always present in the generated config.
    let surfaces: Vec<&str> = report.checks.iter().map(|c| c.surface.as_str()).collect();
    for required in [
        "navigator.userAgent",
        "navigator.platform",
        "navigator.oscpu",
        "screen.width",
        "screen.height",
        "navigator.hardwareConcurrency",
    ] {
        assert!(
            surfaces.contains(&required),
            "surface {required} not covered (got {surfaces:?})"
        );
    }

    if !report.passed() {
        panic!("fingerprint mismatch:\n{}", report.render());
    }

    // User agent spoof is Firefox-flavored and matches the config exactly.
    let ua = browser
        .prepared
        .config
        .get("navigator.userAgent")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let live_ua = page
        .evaluate("navigator.userAgent")
        .await
        .expect("evaluate UA");
    assert_eq!(live_ua.as_str(), Some(ua.as_str()));

    // Deterministic screen dimensions surface.
    let width = browser
        .prepared
        .config
        .get("screen.width")
        .and_then(|v| v.as_u64())
        .unwrap_or_default();
    let live_width = page.evaluate("screen.width").await.expect("screen.width");
    assert_eq!(live_width.as_u64(), Some(width));

    // JS evaluation works end-to-end.
    let sum = page
        .evaluate("(() => ({ answer: 6 * 7 }))()")
        .await
        .expect("evaluate object");
    assert_eq!(sum.get("answer").and_then(|v| v.as_i64()), Some(42));

    browser.close().await.expect("close");
}

#[tokio::test]
async fn persistent_profile_and_session_roundtrip() {
    if !browser_installed() {
        eprintln!("skipping: Camoufox not installed (run `camoufox fetch`)");
        return;
    }

    let profile = tempfile::tempdir().unwrap();
    let options = LaunchOptions {
        os: vec![SupportedOs::Linux],
        headless: HeadlessMode::On,
        persistent_profile: Some(profile.path().to_path_buf()),
        ..Default::default()
    };

    // Session 1: set a cookie + localStorage entry.
    {
        let mut browser = camoufox_juggler::launch_with_juggler(&options)
            .await
            .expect("launch 1");
        let page = browser.new_page().await.expect("page 1");
        page.goto("about:blank").await.expect("goto 1");
        // Firefox caps cookie expiry at ~400 days; far-future cookies are
        // silently dropped. Use a 1-day cookie.
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 86400;
        page.set_cookies(&[serde_json::json!({
            "name": "camoufox-test",
            "value": "persisted",
            "domain": "example.com",
            "path": "/",
            "expires": expires,
        })])
        .await
        .expect("set cookies");

        let cookies = page.cookies().await.expect("cookies");
        let found = cookies
            .iter()
            .any(|c| c.get("name").and_then(|v| v.as_str()) == Some("camoufox-test"));
        assert!(found, "cookie not visible: {cookies:?}");
        browser.close().await.expect("close 1");
    }

    // Session 2: same persistent profile — the cookie survives.
    {
        let mut browser = camoufox_juggler::launch_with_juggler(&options)
            .await
            .expect("launch 2");
        let page = browser.new_page().await.expect("page 2");
        page.goto("about:blank").await.expect("goto 2");
        let cookies = page.cookies().await.expect("cookies 2");
        let survived = cookies
            .iter()
            .any(|c| c.get("name").and_then(|v| v.as_str()) == Some("camoufox-test"));
        assert!(survived, "cookie did not survive the profile restart");
        browser.close().await.expect("close 2");
    }
}
