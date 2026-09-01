//! The Camoufox config map: validation, seeding, warnings and env-var chunking.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;

use rand::Rng;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{CamoufoxError, Result};
use crate::mappings::warnings;
use crate::os::OsName;

/// The config map handed to the Camoufox binary through `CAMOU_CONFIG_*` env
/// vars.
pub type ConfigMap = Map<String, Value>;

/// An entry of the browser's `properties.json` schema.
#[derive(Debug, Clone, Deserialize)]
pub struct PropertyDef {
    /// Property name, e.g. `navigator.userAgent`.
    #[serde(rename = "property")]
    pub property: String,
    /// Declared type: `str`, `int`, `uint`, `double`, `bool`, `array`, `dict`.
    #[serde(rename = "type")]
    pub type_: String,
}

/// `loadProperties`: reads `properties.json` from the executable's directory
/// (when a custom executable is given) or from the install directory.
pub fn load_properties(executable_path: Option<&Path>, install_root: &Path) -> Result<HashMap<String, String>> {
    let prop_file = match executable_path {
        Some(path) => path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("properties.json"),
        None => install_root.join("properties.json"),
    };

    let data = std::fs::read_to_string(&prop_file).map_err(|e| {
        CamoufoxError::FileNotFound(format!(
            "Could not read {}: {e}",
            prop_file.display()
        ))
    })?;
    let defs: Vec<PropertyDef> = serde_json::from_str(&data)?;
    Ok(defs.into_iter().map(|d| (d.property, d.type_)).collect())
}

/// `validateType`: checks a JSON value against the declared property type.
pub fn validate_type(value: &Value, expected: &str) -> bool {
    match expected {
        "str" => value.is_string(),
        "int" => value.as_i64().is_some(),
        "uint" => value.as_u64().is_some(),
        "double" => value.is_number(),
        "bool" => value.is_boolean(),
        "array" => value.is_array(),
        "dict" => value.is_object(),
        _ => false,
    }
}

/// `validateConfig`: rejects unknown properties and type mismatches.
pub fn validate_config(config: &ConfigMap, property_types: &HashMap<String, String>) -> Result<()> {
    for (key, value) in config {
        let Some(expected) = property_types.get(key) else {
            return Err(CamoufoxError::UnknownProperty(format!(
                "Unknown property {key} in config"
            )));
        };
        if !validate_type(value, expected) {
            return Err(CamoufoxError::InvalidPropertyType(format!(
                "Invalid type for property {key}. Expected {expected}, got {}",
                json_type_name(value)
            )));
        }
    }
    Ok(())
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `setInto`: sets the value only when the key is absent (JS `setdefault`).
pub fn set_into(config: &mut ConfigMap, key: &str, value: Value) {
    if !config.contains_key(key) {
        config.insert(key.to_string(), value);
    }
}

/// `mergeInto`: copies absent keys from `source` into `target`.
pub fn merge_into(target: &mut ConfigMap, source: &ConfigMap) {
    for (key, value) in source {
        if !target.contains_key(key) {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// `isDomainSet`: whether any of the given domains is present in the config.
///
/// Entries ending with `.` or `:` are treated as key prefixes.
pub fn is_domain_set(config: &ConfigMap, properties: &[&str]) -> bool {
    properties.iter().any(|prop| {
        if prop.ends_with('.') || prop.ends_with(':') {
            config.keys().any(|key| key.starts_with(prop))
        } else {
            config.contains_key(*prop)
        }
    })
}

/// `warnManualConfig`: warns when the caller manually sets leak-prone domains.
pub fn warn_manual_config(config: &ConfigMap) {
    if is_domain_set(
        config,
        &[
            "navigator.language",
            "navigator.languages",
            "headers.Accept-Language",
            "locale:",
        ],
    ) {
        warnings::warn_leak("locale", Some(false));
    }
    if is_domain_set(config, &["geolocation:", "timezone"]) {
        warnings::warn_leak("geolocation", Some(false));
    }
    if is_domain_set(config, &["headers.User-Agent"]) {
        warnings::warn_leak("header-ua", Some(false));
    }
    if is_domain_set(config, &["navigator."]) {
        warnings::warn_leak("navigator", Some(false));
    }
    if is_domain_set(config, &["screen.", "window.", "document.body."]) {
        warnings::warn_leak("viewport", Some(false));
    }
}

/// Keys whose presence means the config spoofs window dimensions.
pub const WINDOW_DIM_KEYS: &[&str] = &[
    "window.outerWidth",
    "window.outerHeight",
    "window.innerWidth",
    "window.innerHeight",
    "document.body.clientWidth",
    "document.body.clientHeight",
];

/// Whether the config spoofs any window dimension.
///
/// The config map is inspected directly; no env-var reassembly is needed.
pub fn spoofs_window_dimensions(config: &ConfigMap) -> bool {
    WINDOW_DIM_KEYS.iter().any(|key| config.contains_key(*key))
}

/// Camoufox preferences to cache previous pages and requests (`CACHE_PREFS`).
pub fn cache_prefs() -> BTreeMap<String, Value> {
    let mut prefs = BTreeMap::new();
    prefs.insert("browser.sessionhistory.max_entries".into(), Value::from(10));
    prefs.insert("browser.sessionhistory.max_total_viewers".into(), Value::from(-1));
    prefs.insert("browser.cache.memory.enable".into(), Value::Bool(true));
    prefs.insert("browser.cache.disk_cache_ssl".into(), Value::Bool(true));
    prefs.insert("browser.cache.disk.smart_size.enabled".into(), Value::Bool(true));
    prefs
}

/// Random seed in `1..=u32::MAX` (0 is a no-op in the C++ managers, so it is
/// excluded).
pub fn random_seed() -> u64 {
    rand::thread_rng().gen_range(1..=u32::MAX) as u64
}

/// The seed properties injected after the fingerprint merge, when the
/// installed browser schema declares them.
pub const SEED_PROPERTIES: &[&str] = &["fonts:spacing_seed", "audio:seed", "canvas:seed"];

/// `getEnvVars`: serializes the config into `CAMOU_CONFIG_<n>` chunks and
/// points `FONTCONFIG_PATH` at the bundled fontconfig on Linux hosts.
///
/// Chunk sizes: 2047 on Windows, 32767 elsewhere (env var value limits).
pub fn get_env_vars(
    config: &ConfigMap,
    target_os: OsName,
    fontconfig_root: Option<&Path>,
) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();

    let config_str = serde_json::to_string(config)?;
    let host_is_win = cfg!(windows);
    let chunk_size = if host_is_win { 2047 } else { 32767 };

    let chars: Vec<char> = config_str.chars().collect();
    let mut index = 0usize;
    let mut chunk_no = 1usize;
    while index < chars.len() {
        let end = (index + chunk_size).min(chars.len());
        let chunk: String = chars[index..end].iter().collect();
        env.insert(format!("CAMOU_CONFIG_{chunk_no}"), chunk);
        index = end;
        chunk_no += 1;
    }

    // FONTCONFIG_PATH is only injected when the *host* is Linux (the bundled
    // fontconfig layout only exists there).
    if crate::os::host_os() == OsName::Lin {
        if let Some(root) = fontconfig_root {
            env.insert(
                "FONTCONFIG_PATH".to_string(),
                root.join("fontconfig").join(target_os.as_str()).to_string_lossy().into_owned(),
            );
        }
    }

    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_from_json(json: &str) -> ConfigMap {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn set_and_merge_only_fill_gaps() {
        let mut config = config_from_json(r#"{"a": 1}"#);
        set_into(&mut config, "a", Value::from(2));
        set_into(&mut config, "b", Value::from(3));
        assert_eq!(config.get("a").unwrap(), &Value::from(1));
        assert_eq!(config.get("b").unwrap(), &Value::from(3));

        let source = config_from_json(r#"{"a": 9, "c": 4}"#);
        merge_into(&mut config, &source);
        assert_eq!(config.get("a").unwrap(), &Value::from(1));
        assert_eq!(config.get("c").unwrap(), &Value::from(4));
    }

    #[test]
    fn is_domain_set_prefix_and_exact() {
        let config = config_from_json(r#"{"navigator.userAgent": "x", "timezone": "UTC"}"#);
        assert!(is_domain_set(&config, &["navigator."]));
        assert!(is_domain_set(&config, &["timezone"]));
        assert!(!is_domain_set(&config, &["geolocation:"]));
        assert!(!is_domain_set(&config, &["screen."]));
    }

    #[test]
    fn validate_type_matrix() {
        assert!(validate_type(&Value::from("s"), "str"));
        assert!(!validate_type(&Value::from(1), "str"));
        assert!(validate_type(&Value::from(3), "int"));
        assert!(validate_type(&Value::from(-3), "int"));
        assert!(validate_type(&Value::from(3), "uint"));
        assert!(!validate_type(&Value::from(-3), "uint"));
        assert!(validate_type(&Value::from(3.5), "double"));
        assert!(validate_type(&Value::Bool(true), "bool"));
        assert!(validate_type(&Value::Array(vec![]), "array"));
        assert!(validate_type(&Value::Object(Map::new()), "dict"));
        assert!(!validate_type(&Value::Null, "dict"));
        assert!(!validate_type(&Value::from(1), "nope"));
    }

    #[test]
    fn validate_config_rejects_unknown_and_mismatched() {
        let mut types = HashMap::new();
        types.insert("navigator.userAgent".to_string(), "str".into());
        types.insert("navigator.hardwareConcurrency".to_string(), "uint".into());

        let config = config_from_json(r#"{"navigator.userAgent": "x"}"#);
        assert!(validate_config(&config, &types).is_ok());

        let config = config_from_json(r#"{"nope": "x"}"#);
        let err = validate_config(&config, &types).unwrap_err();
        assert_eq!(err.name(), "UnknownProperty");
        assert!(err.to_string().contains("Unknown property nope in config"));

        let config = config_from_json(r#"{"navigator.hardwareConcurrency": "many"}"#);
        let err = validate_config(&config, &types).unwrap_err();
        assert_eq!(err.name(), "InvalidPropertyType");
    }

    #[test]
    fn env_chunking_splits_and_numbers_chunks() {
        let mut config = ConfigMap::new();
        config.insert("navigator.userAgent".into(), Value::String("x".repeat(10)));
        // Force a second chunk with a small effective size by using a long value.
        config.insert("padding".into(), Value::String("y".repeat(50)));

        let env = get_env_vars(&config, OsName::Lin, None).unwrap();
        let chunks: Vec<(&String, &String)> = env.iter().collect();
        assert!(!chunks.is_empty());
        // Reassemble and compare.
        let reassembled: String = chunks.iter().map(|(_, v)| v.as_str()).collect();
        assert_eq!(reassembled, serde_json::to_string(&config).unwrap());
        // Chunk names are CAMOU_CONFIG_1..N with no gaps.
        for (i, (key, _)) in chunks.iter().enumerate() {
            assert_eq!(key.as_str(), format!("CAMOU_CONFIG_{}", i + 1));
        }
    }

    #[test]
    fn env_chunking_sets_fontconfig_path_per_target_os() {
        // FONTCONFIG_PATH is only injected on Linux hosts, for any target OS.
        let config = ConfigMap::new();
        let env = get_env_vars(&config, OsName::Win, Some(Path::new("/install"))).unwrap();
        if crate::os::host_os() == OsName::Lin {
            assert_eq!(
                env.get("FONTCONFIG_PATH").unwrap(),
                "/install/fontconfig/win"
            );
        } else {
            assert!(!env.contains_key("FONTCONFIG_PATH"));
        }
        let env = get_env_vars(&config, OsName::Lin, Some(Path::new("/install"))).unwrap();
        if crate::os::host_os() == OsName::Lin {
            assert_eq!(
                env.get("FONTCONFIG_PATH").unwrap(),
                "/install/fontconfig/lin"
            );
        } else {
            assert!(!env.contains_key("FONTCONFIG_PATH"));
        }
        // No fontconfig root → no FONTCONFIG_PATH.
        let env = get_env_vars(&config, OsName::Lin, None).unwrap();
        assert!(!env.contains_key("FONTCONFIG_PATH"));
    }

    #[test]
    fn spoofs_window_dimensions_detection() {
        let config = config_from_json(r#"{"window.outerWidth": 1280}"#);
        assert!(spoofs_window_dimensions(&config));
        let config = config_from_json(r#"{"navigator.platform": "Win32"}"#);
        assert!(!spoofs_window_dimensions(&config));
        assert!(!spoofs_window_dimensions(&ConfigMap::new()));
    }

    #[test]
    fn seeds_exclude_zero() {
        for _ in 0..50 {
            assert!(random_seed() >= 1);
            assert!(random_seed() <= u32::MAX as u64);
        }
    }
}
