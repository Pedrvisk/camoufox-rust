//! Fingerprint injection verification.
//!
//! Launches (or reuses) a browser, reads the spoofed surfaces from the live
//! page and compares them against the launch's config map. This is the
//! regression net for the whole injection pipeline: if a surface stops
//! matching the generated fingerprint, `verify_fingerprint` reports it.

use serde_json::Value;

use crate::error::Result;
use crate::page::JugglerPage;

/// One spoofed surface check.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SurfaceCheck {
    /// JS surface, e.g. `navigator.userAgent`.
    pub surface: String,
    /// Value from the generated fingerprint (config map).
    pub expected: Value,
    /// Value observed in the running browser.
    pub actual: Value,
    /// Whether expected and actual agree.
    pub passed: bool,
}

/// The full verification outcome.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerificationReport {
    /// One check per verified surface.
    pub checks: Vec<SurfaceCheck>,
}

impl VerificationReport {
    /// Whether every check passed.
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.passed)
    }

    /// Human-readable rendering (table-ish, CLI friendly).
    pub fn render(&self) -> String {
        let mut out = String::new();
        for check in &self.checks {
            let status = if check.passed { "OK " } else { "FAIL" };
            out.push_str(&format!(
                "[{status}] {:<28} expected {} got {}\n",
                check.surface,
                compact(&check.expected),
                compact(&check.actual)
            ));
        }
        if self.checks.iter().all(|c| c.passed) {
            out.push_str("fingerprint verification: all surfaces match\n");
        } else {
            out.push_str(&format!(
                "fingerprint verification: {} of {} checks failed\n",
                self.checks.iter().filter(|c| !c.passed).count(),
                self.checks.len()
            ));
        }
        out
    }
}

fn compact(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Surfaces verified, mapped to config keys.
const SURFACES: &[(&str, &str)] = &[
    ("navigator.userAgent", "userAgent"),
    ("navigator.platform", "platform"),
    ("navigator.oscpu", "oscpu"),
    ("navigator.appVersion", "appVersion"),
    ("navigator.hardwareConcurrency", "hardwareConcurrency"),
    ("navigator.language", "language"),
    ("screen.width", "screenW"),
    ("screen.height", "screenH"),
];

/// Reads the live surfaces and compares them with `config`.
///
/// Only keys present in `config` are checked; the page should have finished
/// at least its initial navigation (about:blank is enough).
pub async fn verify_fingerprint(
    page: &JugglerPage,
    config: &serde_json::Map<String, Value>,
) -> Result<VerificationReport> {
    let expression = r#"(() => ({
  userAgent: navigator.userAgent,
  platform: navigator.platform,
  oscpu: navigator.oscpu,
  appVersion: navigator.appVersion,
  hardwareConcurrency: navigator.hardwareConcurrency,
  language: navigator.language,
  screenW: screen.width,
  screenH: screen.height,
}))()"#;
    let live = page.evaluate(expression).await?;

    let mut checks = Vec::new();
    for (config_key, js_key) in SURFACES {
        let Some(expected) = config.get(*config_key) else {
            continue;
        };
        let actual = live.get(*js_key).cloned().unwrap_or(Value::Null);
        let passed = values_equal(expected, &actual);
        checks.push(SurfaceCheck {
            surface: config_key.to_string(),
            expected: expected.clone(),
            actual,
            passed,
        });
    }
    Ok(VerificationReport { checks })
}

/// JSON comparison tolerant of int/float representation differences.
fn values_equal(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .is_some_and(|a| b.as_f64().is_some_and(|b| a == b)),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn number_and_string_equality() {
        assert!(values_equal(&json!(8), &json!(8.0)));
        assert!(values_equal(&json!("x"), &json!("x")));
        assert!(!values_equal(&json!(8), &json!("8")));
    }

    #[test]
    fn report_renders_failures() {
        let report = VerificationReport {
            checks: vec![
                SurfaceCheck {
                    surface: "navigator.userAgent".into(),
                    expected: json!("A"),
                    actual: json!("A"),
                    passed: true,
                },
                SurfaceCheck {
                    surface: "screen.width".into(),
                    expected: json!(1920),
                    actual: json!(1366),
                    passed: false,
                },
            ],
        };
        assert!(!report.passed());
        let rendered = report.render();
        assert!(rendered.contains("OK "));
        assert!(rendered.contains("FAIL"));
        assert!(rendered.contains("1 of 2 checks failed"));
    }
}
