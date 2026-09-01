//! Boolean env-var parsing helpers.

/// Reads a boolean-ish env var.
///
/// Semantics:
/// - `"false"` or `"0"` → `false`
/// - any other non-empty value → `true`
/// - unset or empty → `default` (or `false` when no default)
pub fn get_as_boolean_from_env(name: &str, default: Option<bool>) -> bool {
    match std::env::var(name) {
        Ok(value) if value == "false" || value == "0" => false,
        Ok(value) if !value.is_empty() => true,
        _ => default.unwrap_or(false),
    }
}

/// Whether browser/addon/GeoIP downloads are skipped
/// (`PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD`).
pub fn skip_browser_download() -> bool {
    get_as_boolean_from_env("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD", Some(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: env-var mutation is process-global; tests use unique names to stay
    // independent from each other.
    #[test]
    fn truthy_values() {
        std::env::set_var("CF_CORE_ENV_TEST_A", "1");
        assert!(get_as_boolean_from_env("CF_CORE_ENV_TEST_A", None));
        std::env::set_var("CF_CORE_ENV_TEST_B", "yes");
        assert!(get_as_boolean_from_env("CF_CORE_ENV_TEST_B", None));
    }

    #[test]
    fn falsy_values() {
        std::env::set_var("CF_CORE_ENV_TEST_C", "0");
        assert!(!get_as_boolean_from_env("CF_CORE_ENV_TEST_C", None));
        std::env::set_var("CF_CORE_ENV_TEST_D", "false");
        assert!(!get_as_boolean_from_env("CF_CORE_ENV_TEST_D", None));
    }

    #[test]
    fn default_values() {
        assert!(get_as_boolean_from_env(
            "CF_CORE_ENV_TEST_UNSET",
            Some(true)
        ));
        assert!(!get_as_boolean_from_env(
            "CF_CORE_ENV_TEST_UNSET",
            Some(false)
        ));
        assert!(!get_as_boolean_from_env("CF_CORE_ENV_TEST_UNSET", None));
    }
}
