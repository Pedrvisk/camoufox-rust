//! Cloudflare challenge detection and solving (interstitial + Turnstile).
//!
//! Camoufox's spoofed fingerprint passes Cloudflare's passive checks often
//! enough that managed challenges (the "Just a moment..." interstitial)
//! clear on their own a few seconds after load. This module closes the
//! remaining gap:
//!
//! - [`detect_challenge`] recognizes the interstitial and embedded
//!   Turnstile widgets from main-frame markers only — no cross-origin
//!   frame access needed
//! - [`solve`] waits for automatic clearance and, when the challenge
//!   wants interaction, clicks the Turnstile checkbox with a humanized
//!   cursor path synthesized from viewport-coordinate mouse events
//!
//! Success is never guaranteed — Cloudflare adapts. Headful runs (or the
//! facade's `HeadlessMode::Virtual` on Linux) pass noticeably more often
//! than plain headless, and Turnstile is known to silently fail inside
//! Docker containers. Widgets rendered inside shadow DOM are not covered.
//!
//! Detection heuristics informed by the Apache-2.0 project
//! [playwright-captcha](https://github.com/techinz/playwright-captcha).
//!
//! ```no_run
//! # async fn demo() -> camoufox_juggler::Result<()> {
//! use std::time::Duration;
//! use camoufox_juggler::cloudflare::{solve, CloudflareOptions, CloudflareOutcome};
//! # let page: std::sync::Arc<camoufox_juggler::JugglerPage> = todo!();
//! let options = CloudflareOptions {
//!     timeout: Duration::from_secs(45),
//!     ..Default::default()
//! };
//! match solve(&page, &options).await? {
//!     CloudflareOutcome::Cleared { .. } => println!("through"),
//!     CloudflareOutcome::TimedOut { .. } => println!("blocked"),
//!     CloudflareOutcome::NoChallenge => println!("no challenge"),
//! }
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use rand::Rng;
use serde_json::Value;

use crate::dom::{Point, Quad};
use crate::error::{JugglerError, Result};
use crate::input::{Modifiers, MouseButton};
use crate::page::JugglerPage;

/// Selector matching the Cloudflare challenge iframe (both the
/// interstitial widget and embedded Turnstile render into it).
const CF_IFRAME_SELECTOR: &str = "iframe[src*='challenges.cloudflare.com']";

/// Which kind of Cloudflare challenge the page is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeKind {
    /// Full-page interstitial ("Just a moment...").
    Interstitial,
    /// Embedded Turnstile widget.
    Turnstile,
}

impl ChallengeKind {
    fn label(self) -> &'static str {
        match self {
            ChallengeKind::Interstitial => "interstitial",
            ChallengeKind::Turnstile => "turnstile",
        }
    }
}

/// Result of a [`solve`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudflareOutcome {
    /// No Cloudflare challenge was present.
    NoChallenge,
    /// The challenge cleared — on its own when `clicked` is false.
    Cleared {
        /// Which challenge was cleared.
        kind: ChallengeKind,
        /// Whether the solver clicked the Turnstile checkbox.
        clicked: bool,
    },
    /// The challenge was still present when the timeout expired.
    TimedOut {
        /// Which challenge refused to clear.
        kind: ChallengeKind,
    },
}

impl CloudflareOutcome {
    /// Whether the page is usable (no challenge in the way).
    pub fn passed(self) -> bool {
        !matches!(self, CloudflareOutcome::TimedOut { .. })
    }
}

impl std::fmt::Display for CloudflareOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudflareOutcome::NoChallenge => write!(f, "no cloudflare challenge detected"),
            CloudflareOutcome::Cleared { kind, clicked } => write!(
                f,
                "cloudflare {} challenge cleared ({})",
                kind.label(),
                if *clicked {
                    "checkbox clicked"
                } else {
                    "automatic"
                }
            ),
            CloudflareOutcome::TimedOut { kind } => write!(
                f,
                "cloudflare {} challenge still present after timeout",
                kind.label()
            ),
        }
    }
}

/// Humanized-cursor parameters for the Turnstile checkbox click.
#[derive(Debug, Clone)]
pub struct HumanizeOptions {
    /// Intermediate waypoints along the generated bezier path.
    pub waypoints: usize,
    /// Maximum random offset applied to intermediate waypoints (pixels).
    pub jitter: f64,
    /// Milliseconds slept between mouse moves (inclusive range).
    pub move_delay_ms: (u64, u64),
    /// Milliseconds slept between mousedown and mouseup (inclusive range).
    pub press_delay_ms: (u64, u64),
}

impl Default for HumanizeOptions {
    fn default() -> Self {
        Self {
            waypoints: 18,
            jitter: 2.0,
            move_delay_ms: (5, 25),
            press_delay_ms: (40, 90),
        }
    }
}

/// Tuning for [`solve`]. The defaults cover typical managed challenges;
/// raise `timeout` for slow interstitials.
#[derive(Debug, Clone)]
pub struct CloudflareOptions {
    /// Total budget for waiting and clicking.
    pub timeout: Duration,
    /// Delay between challenge-state polls.
    pub poll_interval: Duration,
    /// Maximum checkbox click attempts.
    pub click_attempts: u32,
    /// Offset of the checkbox center from the challenge iframe's left
    /// edge and vertical center (pixels). The standard widget renders the
    /// checkbox at roughly (30, 0).
    pub checkbox_offset: (f64, f64),
    /// Optional selector proving the real content is reachable (counts
    /// as cleared for both challenge kinds).
    pub expected_selector: Option<String>,
    /// Humanized-cursor parameters.
    pub humanize: HumanizeOptions,
}

impl Default for CloudflareOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            poll_interval: Duration::from_millis(500),
            click_attempts: 3,
            checkbox_offset: (30.0, 0.0),
            expected_selector: None,
            humanize: HumanizeOptions::default(),
        }
    }
}

/// One round of main-frame challenge markers.
#[derive(Debug, Clone, Default, PartialEq)]
struct ChallengeProbe {
    /// `script[src*="/cdn-cgi/challenge-platform/"]` present. Also present
    /// on cleared pages behind Bot Management, so never a signal alone.
    script: bool,
    /// Interstitial scaffolding (`#challenge-running` / `#challenge-form`).
    running: bool,
    /// Interstitial-style document title.
    jam_title: bool,
    /// Turnstile markers (token input or widget script).
    turnstile: bool,
    /// Value of `input[name="cf-turnstile-response"]` when it exists.
    token: Option<String>,
    /// `expected_selector` matched.
    expected: bool,
}

/// Builds the main-frame probe expression.
fn probe_expression(expected_selector: Option<&str>) -> String {
    let expected = match expected_selector {
        Some(selector) => format!(
            "document.querySelector({}) !== null",
            serde_json::to_string(selector).unwrap_or_else(|_| "null".into())
        ),
        None => "false".to_string(),
    };
    format!(
        r#"(() => {{
            const q = s => document.querySelector(s) !== null;
            const title = (document.title || '').toLowerCase();
            const token = document.querySelector('input[name="cf-turnstile-response"]');
            return {{
                script: q('script[src*="/cdn-cgi/challenge-platform/"]'),
                running: q('#challenge-running') || q('#challenge-form') || q('#cf-challenge-running'),
                jamTitle: title.includes('just a moment') || title.includes('attention required') || title.includes('verify you are human'),
                turnstile: !!token || q('script[src*="challenges.cloudflare.com/turnstile/v0"]'),
                token: token ? (token.value || '') : null,
                expected: {expected},
            }};
        }})()"#
    )
}

fn decode_probe(value: &Value) -> ChallengeProbe {
    let flag = |name: &str| value.get(name).and_then(Value::as_bool).unwrap_or(false);
    ChallengeProbe {
        script: flag("script"),
        running: flag("running"),
        jam_title: flag("jamTitle"),
        turnstile: flag("turnstile"),
        token: value
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_string),
        expected: flag("expected"),
    }
}

/// Which challenge the probe describes, if any.
fn classify(probe: &ChallengeProbe) -> Option<ChallengeKind> {
    if probe.script && (probe.jam_title || probe.running) {
        Some(ChallengeKind::Interstitial)
    } else if probe.turnstile {
        Some(ChallengeKind::Turnstile)
    } else {
        None
    }
}

/// Whether the challenge has cleared per the probe.
fn cleared(probe: &ChallengeProbe, kind: ChallengeKind) -> bool {
    if probe.expected {
        return true;
    }
    match kind {
        // The script tag survives on cleared pages behind Bot Management,
        // so clearance is judged by the scaffolding and title going away.
        ChallengeKind::Interstitial => !probe.running && !probe.jam_title,
        // Cloudflare writes the solve token into the host page's hidden
        // input as soon as the widget accepts the click.
        ChallengeKind::Turnstile => probe.token.as_deref().is_some_and(|t| !t.is_empty()),
    }
}

async fn probe_page(page: &JugglerPage, expected_selector: Option<&str>) -> Result<ChallengeProbe> {
    let value = page.evaluate(&probe_expression(expected_selector)).await?;
    Ok(decode_probe(&value))
}

/// Detects which Cloudflare challenge (if any) the page is currently
/// showing. `None` on plain pages.
///
/// A probe is a snapshot: interstitial pages navigate as they clear, so
/// prefer [`solve`] when the goal is getting through.
pub async fn detect_challenge(page: &JugglerPage) -> Result<Option<ChallengeKind>> {
    let probe = probe_page(page, None).await?;
    Ok(classify(&probe))
}

/// Detects the Cloudflare challenge on the page (if any) and gets through
/// it: managed interstitials usually clear by themselves once the
/// fingerprint passes the passive checks, and interactive challenges get
/// their Turnstile checkbox clicked with a humanized cursor.
///
/// Returns [`CloudflareOutcome::NoChallenge`] immediately on clean pages;
/// timeouts are reported as [`CloudflareOutcome::TimedOut`] rather than
/// errors (only protocol/transport failures error).
///
/// Pair with the facade's `humanize` launch option and `disable_coop`
/// for the best pass rate — see the CLI's `--solve-cloudflare`.
pub async fn solve(page: &JugglerPage, options: &CloudflareOptions) -> Result<CloudflareOutcome> {
    let mut probe = probe_page(page, options.expected_selector.as_deref()).await?;
    let Some(kind) = classify(&probe) else {
        return Ok(CloudflareOutcome::NoChallenge);
    };
    log::info!("cloudflare {} challenge detected", kind.label());

    let deadline = tokio::time::Instant::now() + options.timeout;
    let mut rng = rand::thread_rng();
    let mut clicks = 0u32;
    loop {
        if cleared(&probe, kind) {
            log::info!("cloudflare {} challenge cleared", kind.label());
            return Ok(CloudflareOutcome::Cleared {
                kind,
                clicked: clicks > 0,
            });
        }

        // Interactive widget: click the checkbox while it is there. A
        // click landing on an already-solving (or gone) challenge is
        // harmless — the clearance poll below decides when we are through.
        if clicks < options.click_attempts {
            match checkbox_target(page, options).await {
                Ok(Some(target)) => {
                    clicks += 1;
                    log::debug!("clicking turnstile checkbox (attempt {clicks})");
                    humanized_click(page, target, &options.humanize, &mut rng).await?;
                }
                Ok(None) => {}
                Err(error) if is_stale_context(&error) => {}
                Err(error) => return Err(error),
            }
        }

        if tokio::time::Instant::now() >= deadline {
            log::warn!(
                "cloudflare {} challenge not cleared within {:?}",
                kind.label(),
                options.timeout
            );
            return Ok(CloudflareOutcome::TimedOut { kind });
        }
        tokio::time::sleep(options.poll_interval).await;

        match probe_page(page, options.expected_selector.as_deref()).await {
            Ok(next) => probe = next,
            // The interstitial navigating away races the probe; retry.
            Err(error) if is_stale_context(&error) => {}
            Err(error) => return Err(error),
        }
    }
}

/// Errors meaning "the page navigated under the probe", safe to retry.
fn is_stale_context(error: &JugglerError) -> bool {
    matches!(&error, JugglerError::Protocol(message)
        if message.contains("destroyed")
            || message.contains("not found")
            || message.contains("execution context"))
}

/// Viewport-coordinate center of the Turnstile checkbox inside the
/// challenge iframe, when the iframe is rendered.
async fn checkbox_target(page: &JugglerPage, options: &CloudflareOptions) -> Result<Option<Point>> {
    let Some(object_id) = page.query_object_id(CF_IFRAME_SELECTOR).await? else {
        return Ok(None);
    };
    page.scroll_into_view(&object_id).await?;
    let quads = page.content_quads(&object_id).await?;
    let Some((x, y, _, height)) = largest_box(&quads) else {
        return Ok(None);
    };
    let (dx, dy) = options.checkbox_offset;
    Ok(Some(Point {
        x: x + dx,
        y: y + height / 2.0 + dy,
    }))
}

/// Largest non-degenerate bounding box among the quads — the interactive
/// widget (~300×65 CSS px) dwarfs the hidden bookkeeping iframes.
fn largest_box(quads: &[Quad]) -> Option<(f64, f64, f64, f64)> {
    quads
        .iter()
        .map(Quad::bounding_box)
        .filter(|(_, _, width, height)| *width > 0.0 && *height > 0.0)
        .max_by(|a, b| {
            let area = |box_: &(f64, f64, f64, f64)| box_.2 * box_.3;
            area(a)
                .partial_cmp(&area(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// A random starting point 60–180 px away from the target, so the cursor
/// travels a believable distance instead of teleporting in.
fn offset_start(target: Point, rng: &mut impl Rng) -> Point {
    let angle = rng.gen_range(0.0..std::f64::consts::TAU);
    let distance = rng.gen_range(60.0..=180.0);
    Point {
        x: target.x + distance * angle.cos(),
        y: target.y + distance * angle.sin(),
    }
}

/// Cubic bezier path from `from` to `to` with randomized control points,
/// `waypoints` intermediate samples and up to `jitter` px of noise on the
/// intermediate points (endpoints stay exact).
fn bezier_path(
    from: Point,
    to: Point,
    waypoints: usize,
    jitter: f64,
    rng: &mut impl Rng,
) -> Vec<Point> {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length < f64::EPSILON || waypoints == 0 {
        return vec![from, to];
    }
    // Control points at ~1/3 and ~2/3 of the segment, pushed sideways by
    // up to a fifth of its length.
    let perpendicular = Point {
        x: -dy / length,
        y: dx / length,
    };
    let bulge = |frac: f64, sign: f64| Point {
        x: from.x + dx * frac + perpendicular.x * length * sign,
        y: from.y + dy * frac + perpendicular.y * length * sign,
    };
    let c1 = bulge(1.0 / 3.0, rng.gen_range(-0.2..=0.2));
    let c2 = bulge(2.0 / 3.0, rng.gen_range(-0.2..=0.2));

    let steps = waypoints + 1;
    let mut path = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let t = step as f64 / steps as f64;
        let u = 1.0 - t;
        let point = Point {
            x: u * u * u * from.x
                + 3.0 * u * u * t * c1.x
                + 3.0 * u * t * t * c2.x
                + t * t * t * to.x,
            y: u * u * u * from.y
                + 3.0 * u * u * t * c1.y
                + 3.0 * u * t * t * c2.y
                + t * t * t * to.y,
        };
        if step == 0 || step == steps {
            path.push(point);
        } else {
            path.push(Point {
                x: point.x + rng.gen_range(-jitter..=jitter),
                y: point.y + rng.gen_range(-jitter..=jitter),
            });
        }
    }
    path
}

/// Moves the cursor along a humanized path and clicks at `target`.
async fn humanized_click(
    page: &JugglerPage,
    target: Point,
    options: &HumanizeOptions,
    rng: &mut impl Rng,
) -> Result<()> {
    let start = offset_start(target, rng);
    for point in bezier_path(start, target, options.waypoints, options.jitter, rng) {
        page.mouse_move(point.x, point.y).await?;
        let delay = rng.gen_range(options.move_delay_ms.0..=options.move_delay_ms.1);
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let press = rng.gen_range(options.press_delay_ms.0..=options.press_delay_ms.1);
    tokio::time::sleep(Duration::from_millis(press)).await;
    page.mouse_down(target.x, target.y, MouseButton::Left, Modifiers::NONE, 1)
        .await?;
    let hold = rng.gen_range(options.press_delay_ms.0..=options.press_delay_ms.1);
    tokio::time::sleep(Duration::from_millis(hold)).await;
    page.mouse_up(target.x, target.y, MouseButton::Left, Modifiers::NONE, 1)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use serde_json::json;

    fn seeded_rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    #[test]
    fn classifies_probes() {
        // Interstitial: challenge-platform script plus title/scaffolding.
        assert_eq!(
            classify(&ChallengeProbe {
                script: true,
                jam_title: true,
                ..Default::default()
            }),
            Some(ChallengeKind::Interstitial)
        );
        assert_eq!(
            classify(&ChallengeProbe {
                script: true,
                running: true,
                ..Default::default()
            }),
            Some(ChallengeKind::Interstitial)
        );
        // The script alone (Bot Management on a cleared page) is not a
        // challenge.
        assert_eq!(
            classify(&ChallengeProbe {
                script: true,
                ..Default::default()
            }),
            None
        );
        // Turnstile: token input or widget script.
        assert_eq!(
            classify(&ChallengeProbe {
                turnstile: true,
                ..Default::default()
            }),
            Some(ChallengeKind::Turnstile)
        );
        assert_eq!(classify(&ChallengeProbe::default()), None);
    }

    #[test]
    fn clearance_rules() {
        // Interstitial clears when scaffolding and title are gone, even
        // though the script tag survives.
        assert!(cleared(
            &ChallengeProbe {
                script: true,
                ..Default::default()
            },
            ChallengeKind::Interstitial
        ));
        assert!(!cleared(
            &ChallengeProbe {
                script: true,
                jam_title: true,
                ..Default::default()
            },
            ChallengeKind::Interstitial
        ));
        // Turnstile clears when the token lands in the host page.
        assert!(cleared(
            &ChallengeProbe {
                turnstile: true,
                token: Some("0.ABCabc".into()),
                ..Default::default()
            },
            ChallengeKind::Turnstile
        ));
        assert!(!cleared(
            &ChallengeProbe {
                turnstile: true,
                token: Some(String::new()),
                ..Default::default()
            },
            ChallengeKind::Turnstile
        ));
        assert!(!cleared(
            &ChallengeProbe {
                turnstile: true,
                ..Default::default()
            },
            ChallengeKind::Turnstile
        ));
        // The expected selector overrides both kinds.
        assert!(cleared(
            &ChallengeProbe {
                expected: true,
                running: true,
                jam_title: true,
                ..Default::default()
            },
            ChallengeKind::Interstitial
        ));
    }

    #[test]
    fn decodes_probe_objects() {
        let probe = decode_probe(&json!({
            "script": true,
            "running": false,
            "jamTitle": true,
            "turnstile": false,
            "token": null,
            "expected": false,
        }));
        assert_eq!(
            probe,
            ChallengeProbe {
                script: true,
                jam_title: true,
                token: None,
                ..Default::default()
            }
        );
        // Missing fields default to false/None instead of failing.
        let probe = decode_probe(&json!({}));
        assert_eq!(probe, ChallengeProbe::default());
    }

    #[test]
    fn probe_expression_embeds_expected_selector() {
        let expression = probe_expression(Some("#content"));
        assert!(expression.contains("document.querySelector(\"#content\")"));
        // The fallback probe never references document.querySelector for
        // the expected check.
        assert!(!probe_expression(None).contains("#content"));
    }

    #[test]
    fn bezier_paths_keep_exact_endpoints() {
        let from = Point { x: 10.0, y: 20.0 };
        let to = Point { x: 300.0, y: 260.0 };
        let path = bezier_path(from, to, 18, 2.0, &mut seeded_rng());
        assert_eq!(path.len(), 20);
        assert_eq!(path.first().copied(), Some(from));
        assert_eq!(path.last().copied(), Some(to));
        for point in &path {
            assert!(point.x.is_finite() && point.y.is_finite());
        }
        // Degenerate segment: straight to the endpoints.
        let path = bezier_path(from, from, 18, 2.0, &mut seeded_rng());
        assert_eq!(path, vec![from, from]);
    }

    #[test]
    fn offset_start_keeps_human_distance() {
        let target = Point { x: 400.0, y: 300.0 };
        for _ in 0..64 {
            let start = offset_start(target, &mut seeded_rng());
            let distance = ((start.x - target.x).powi(2) + (start.y - target.y).powi(2)).sqrt();
            assert!((60.0..=180.0).contains(&distance), "distance {distance}");
        }
    }

    #[test]
    fn largest_box_skips_hidden_iframes() {
        let quad = |x: f64, y: f64, w: f64, h: f64| Quad {
            p1: Point { x, y },
            p2: Point { x: x + w, y },
            p3: Point { x: x + w, y: y + h },
            p4: Point { x, y: y + h },
        };
        let quads = [
            quad(0.0, 0.0, 0.0, 0.0),
            quad(5.0, 5.0, 1.0, 1.0),
            quad(10.0, 10.0, 300.0, 65.0),
        ];
        assert_eq!(largest_box(&quads), Some((10.0, 10.0, 300.0, 65.0)));
        assert_eq!(largest_box(&quads[..1]), None);
    }
}
