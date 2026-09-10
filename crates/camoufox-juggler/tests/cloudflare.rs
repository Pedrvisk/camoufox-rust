//! Cloudflare detection & solving end-to-end.
//!
//! Launches the real browser through the native Juggler driver. Skips
//! (with a notice) when the browser is not installed — run `camoufox
//! fetch` first. Actual challenge solving against live Cloudflare
//! endpoints is flaky in CI (headless Turnstile failures are a known
//! upstream issue), so those tests are gated behind
//! `CAMOUFOX_CF_INTERSTITIAL_TEST=<url>`.

use std::time::Duration;

use camoufox::builder::{HeadlessMode, LaunchOptions};
use camoufox_core::os::SupportedOs;
use camoufox_juggler::cloudflare::{detect_challenge, CloudflareOptions, CloudflareOutcome};

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

fn test_options() -> LaunchOptions {
    LaunchOptions {
        os: vec![SupportedOs::Linux],
        headless: HeadlessMode::On,
        // Best-effort pass rate for interactive challenges.
        humanize: Some(None),
        disable_coop: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn no_false_positive_on_plain_pages() {
    if !browser_installed() {
        eprintln!("skipping: Camoufox not installed (run `camoufox fetch`)");
        return;
    }

    let mut browser = camoufox_juggler::launch_with_juggler(&test_options())
        .await
        .expect("launch");
    let page = browser.new_page().await.expect("new page");
    page.goto("https://example.com").await.expect("goto");

    let detected = detect_challenge(&page).await.expect("detect");
    assert_eq!(detected, None, "example.com misdetected as a challenge");

    let outcome = camoufox_juggler::cloudflare::solve(&page, &CloudflareOptions::default())
        .await
        .expect("solve");
    assert_eq!(outcome, CloudflareOutcome::NoChallenge);

    browser.close().await.expect("close");
}

#[tokio::test]
async fn solve_reports_no_challenge_and_never_hangs() {
    if !browser_installed() {
        eprintln!("skipping: Camoufox not installed (run `camoufox fetch`)");
        return;
    }

    let mut browser = camoufox_juggler::launch_with_juggler(&test_options())
        .await
        .expect("launch");
    let page = browser.new_page().await.expect("new page");
    page.goto("https://example.com").await.expect("goto");

    // A short-budget solve on a clean page returns immediately.
    let started = std::time::Instant::now();
    let outcome = camoufox_juggler::cloudflare::solve(
        &page,
        &CloudflareOptions {
            timeout: Duration::from_secs(5),
            ..Default::default()
        },
    )
    .await
    .expect("solve");
    assert_eq!(outcome, CloudflareOutcome::NoChallenge);
    assert!(started.elapsed() < Duration::from_secs(10));

    browser.close().await.expect("close");
}

/// Live interstitial solve — opt-in because it depends on a
/// Cloudflare-protected site and a residential-looking network:
/// `CAMOUFOX_CF_INTERSTITIAL_TEST=https://protected.example cargo test -p camoufox-juggler --test cloudflare`
#[tokio::test]
async fn live_interstitial_solve() {
    if !browser_installed() {
        eprintln!("skipping: Camoufox not installed (run `camoufox fetch`)");
        return;
    }
    let Some(target) = std::env::var("CAMOUFOX_CF_INTERSTITIAL_TEST")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        eprintln!("skipping: CAMOUFOX_CF_INTERSTITIAL_TEST not set");
        return;
    };

    let mut browser = camoufox_juggler::launch_with_juggler(&test_options())
        .await
        .expect("launch");
    let page = browser.new_page().await.expect("new page");
    page.goto(&target).await.expect("goto");

    let outcome = camoufox_juggler::cloudflare::solve(
        &page,
        &CloudflareOptions {
            timeout: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .await
    .expect("solve");
    assert!(outcome.passed(), "challenge not cleared: {outcome}");

    browser.close().await.expect("close");
}
