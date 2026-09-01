//! # camoufox-webgl
//!
//! WebGL fingerprint sampling from the embedded SQLite database, weighted by
//! the real-world market share of each vendor/renderer pair per OS.

use std::collections::HashMap;

use camoufox_core::error::{CamoufoxError, Result};
use camoufox_core::os::OsName;
use rand::Rng;
use rusqlite::Connection;
use serde_json::{Map, Value};

/// The embedded WebGL fingerprint database.
const WEBGL_DB: &[u8] = include_bytes!("data/webgl_data.db");

/// A sampled WebGL fingerprint.
#[derive(Debug, Clone)]
pub struct WebGLFingerprint {
    /// The `webgl:*` config entries.
    pub config: Map<String, Value>,
    /// Whether WebGL2 is enabled for this sample.
    pub webgl2_enabled: bool,
}

/// A vendor/renderer pair available in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorRenderer {
    /// The WebGL vendor string.
    pub vendor: String,
    /// The WebGL renderer string.
    pub renderer: String,
}

/// Samples a WebGL fingerprint for the target OS.
///
/// When `vendor` and `renderer` are given, the exact pair is used (and must
/// have positive market share for the OS); otherwise a pair is chosen weighted
/// by market share.
pub fn sample_webgl(
    os: OsName,
    vendor: Option<&str>,
    renderer: Option<&str>,
) -> Result<WebGLFingerprint> {
    let conn = open_embedded_db()?;

    let os_column = os.as_str();

    match (vendor, renderer) {
        (Some(vendor), Some(renderer)) => {
            let weight: Option<f64> = match conn.query_row(
                &format!(
                    "SELECT {os_column} FROM webgl_fingerprints WHERE vendor = ?1 AND renderer = ?2"
                ),
                rusqlite::params![vendor, renderer],
                |row| row.get(0),
            ) {
                Ok(weight) => Some(weight),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(CamoufoxError::Sqlite(e.to_string())),
            };
            let Some(weight) = weight else {
                return possible_pairs_error(&conn, os, vendor, renderer);
            };
            if weight <= 0.0 {
                return possible_pairs_error(&conn, os, vendor, renderer);
            }
            let data: String = conn
                .query_row(
                    "SELECT data FROM webgl_fingerprints WHERE vendor = ?1 AND renderer = ?2",
                    rusqlite::params![vendor, renderer],
                    |row| row.get(0),
                )
                .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;
            parse_sample(&data)
        }
        _ => {
            let mut statement = conn
                .prepare(&format!(
                    "SELECT data, {os_column} FROM webgl_fingerprints WHERE {os_column} > 0"
                ))
                .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;
            let rows: Vec<(String, f64)> = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;

            if rows.is_empty() {
                return Err(CamoufoxError::Sqlite(format!(
                    "No WebGL data found for OS: {os}"
                )));
            }

            let weights: Vec<f64> = rows.iter().map(|(_, w)| *w).collect();
            let idx = weighted_random_choice(&weights);
            parse_sample(&rows[idx].0)
        }
    }
}

/// Opens the embedded database into an in-memory connection.
///
/// The database is embedded in the binary; it is materialized into a private
/// temp file once per call (SQLite needs a seekable file) and opened
/// read-only.
fn open_embedded_db() -> Result<Connection> {
    use std::io::Write;

    let mut temp = tempfile::Builder::new()
        .prefix("camoufox-webgl-")
        .suffix(".db")
        .tempfile()
        .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;
    temp.write_all(WEBGL_DB)
        .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;
    temp.flush()
        .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;

    let conn = Connection::open_with_flags(
        temp.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;

    // Keep the temp file alive for the connection's lifetime.
    std::mem::forget(temp);
    Ok(conn)
}

fn parse_sample(data: &str) -> Result<WebGLFingerprint> {
    let value: Value =
        serde_json::from_str(data).map_err(|e| CamoufoxError::Json(e.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| CamoufoxError::Json("webgl sample is not an object".into()))?;

    let mut config = Map::new();
    let mut webgl2_enabled = true;
    for (key, value) in object {
        if key == "webGl2Enabled" {
            webgl2_enabled = value.as_bool().unwrap_or(true);
            continue;
        }
        if key == "webGl:vendor" || key == "webGl:renderer" {
            config.insert(key.clone(), value.clone());
            continue;
        }
        config.insert(key.clone(), value.clone());
    }
    Ok(WebGLFingerprint {
        config,
        webgl2_enabled,
    })
}

fn possible_pairs_error(
    conn: &Connection,
    os: OsName,
    vendor: &str,
    renderer: &str,
) -> Result<WebGLFingerprint> {
    let os_column = os.as_str();
    let mut statement = conn
        .prepare(&format!(
            "SELECT DISTINCT vendor, renderer FROM webgl_fingerprints WHERE {os_column} > 0"
        ))
        .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;
    let pairs: Vec<String> = statement
        .query_map([], |row| {
            Ok(format!(
                "{}, {}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?
            ))
        })
        .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;

    Err(CamoufoxError::Sqlite(format!(
        "Vendor \"{vendor}\" and renderer \"{renderer}\" combination not valid for {os}. \
         Possible pairs: {}",
        pairs.join(", ")
    )))
}

/// Lists the vendor/renderer pairs with positive weight per OS.
pub fn get_possible_pairs() -> Result<HashMap<String, Vec<VendorRenderer>>> {
    let conn = open_embedded_db()?;

    let mut result = HashMap::new();
    for os in [OsName::Win, OsName::Mac, OsName::Lin] {
        let os_column = os.as_str();
        let mut statement = conn
            .prepare(&format!(
                "SELECT DISTINCT vendor, renderer FROM webgl_fingerprints \
                 WHERE {os_column} > 0 ORDER BY {os_column} DESC"
            ))
            .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;
        let pairs: Vec<VendorRenderer> = statement
            .query_map([], |row| {
                Ok(VendorRenderer {
                    vendor: row.get(0)?,
                    renderer: row.get(1)?,
                })
            })
            .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| CamoufoxError::Sqlite(e.to_string()))?;
        result.insert(os.as_str().to_string(), pairs);
    }
    Ok(result)
}

fn weighted_random_choice(weights: &[f64]) -> usize {
    let sum: f64 = weights.iter().sum();
    let threshold: f64 = rand::thread_rng().gen::<f64>() * sum;
    let mut cumulative = 0.0;
    for (i, weight) in weights.iter().enumerate() {
        cumulative += weight;
        if cumulative >= threshold {
            return i;
        }
    }
    weights.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_for_each_os() {
        for os in [OsName::Win, OsName::Mac, OsName::Lin] {
            let fp = sample_webgl(os, None, None).unwrap();
            assert!(fp.config.contains_key("webGl:vendor"), "vendor for {os}");
            assert!(
                fp.config.contains_key("webGl:renderer"),
                "renderer for {os}"
            );
        }
    }

    #[test]
    fn samples_differ_across_draws() {
        let mut vendors = std::collections::HashSet::new();
        for _ in 0..20 {
            let fp = sample_webgl(OsName::Win, None, None).unwrap();
            vendors.insert(
                fp.config
                    .get("webGl:renderer")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        assert!(vendors.len() > 1, "expected variety, got {vendors:?}");
    }

    #[test]
    fn exact_pair_lookup() {
        let pairs = get_possible_pairs().unwrap();
        let win_pairs = &pairs["win"];
        let first = &win_pairs[0];
        let fp = sample_webgl(OsName::Win, Some(&first.vendor), Some(&first.renderer)).unwrap();
        assert_eq!(
            fp.config.get("webGl:vendor").and_then(Value::as_str),
            Some(first.vendor.as_str())
        );
    }

    #[test]
    fn invalid_pair_errors_with_candidates() {
        let err = sample_webgl(OsName::Win, Some("Nonexistent"), Some("GPU")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Nonexistent"));
        assert!(msg.contains("Possible pairs"));
    }

    #[test]
    fn possible_pairs_cover_all_os() {
        let pairs = get_possible_pairs().unwrap();
        assert!(pairs["win"].len() > 5);
        assert!(pairs["mac"].len() > 1);
        assert!(pairs["lin"].len() > 1);
    }
}
