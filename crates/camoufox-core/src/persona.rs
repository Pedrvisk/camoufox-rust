//! Persona domain types: stable identities persisted across runs.
//!
//! A persona binds a deterministic seed to a generated fingerprint (plus any
//! extra Firefox prefs), so the same browser identity can be reused across
//! sessions and machines. Persistence itself lives in the `camoufox-store`
//! crate; this module only defines the pure data model.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use veilus_fingerprint::BrowserProfile;

use crate::error::{CamoufoxError, Result};
use crate::fingerprint::{generate_fingerprint, FingerprintRequest};

/// A persisted browser identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaRecord {
    /// Unique persona identifier (filesystem- and SQL-safe).
    pub id: String,
    /// Human-readable label.
    pub name: Option<String>,
    /// The seed the fingerprint was generated from, when deterministic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Unix timestamp (seconds) of creation.
    pub created_at: u64,
    /// The generated fingerprint.
    pub fingerprint: BrowserProfile,
    /// Extra Firefox user prefs applied on launch for this persona.
    #[serde(default)]
    pub firefox_user_prefs: BTreeMap<String, Value>,
    /// Free-form metadata.
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

/// Characters allowed in a persona id: `[a-zA-Z0-9._-]`.
fn valid_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

impl PersonaRecord {
    /// Sanitizes an arbitrary string into a valid persona id.
    pub fn sanitize_id(input: &str) -> String {
        let mut id: String = input
            .chars()
            .map(|c| if valid_id_char(c) { c } else { '-' })
            .collect();
        while id.starts_with('.') || id.starts_with('-') {
            id.remove(0);
        }
        while id.ends_with('-') {
            id.pop();
        }
        while id.contains("--") {
            id = id.replace("--", "-");
        }
        if id.is_empty() {
            id.push_str("persona");
        }
        id
    }

    /// Validates a persona id.
    pub fn validate_id(id: &str) -> Result<()> {
        if id.is_empty() || id.len() > 128 || !id.chars().all(valid_id_char) {
            return Err(CamoufoxError::InvalidPersona(format!(
                "invalid persona id '{id}': must be 1-128 chars of [a-zA-Z0-9._-]"
            )));
        }
        Ok(())
    }

    /// Generates a fresh persona from a fingerprint request.
    pub fn generate(id: &str, request: &FingerprintRequest) -> Result<Self> {
        let seed = request.seed;
        let fingerprint = generate_fingerprint(request)?;
        Ok(Self {
            id: Self::sanitize_id(id),
            name: None,
            seed,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
            fingerprint,
            firefox_user_prefs: BTreeMap::new(),
            metadata: BTreeMap::new(),
        })
    }
}

/// A cookie persisted for a persona session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaCookie {
    /// Cookie name.
    pub name: String,
    /// Cookie value.
    pub value: String,
    /// Cookie domain.
    pub domain: String,
    /// Cookie path.
    pub path: String,
    /// Expiry (unix seconds); `None` means session cookie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    /// Secure-only cookie.
    #[serde(default)]
    pub secure: bool,
    /// HttpOnly cookie.
    #[serde(default)]
    pub http_only: bool,
    /// Same-site policy.
    #[serde(default)]
    pub same_site: Option<String>,
}

/// Local-storage entries captured for a single origin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaLocalStorage {
    /// Origin the entries belong to (e.g. `https://example.com`).
    pub origin: String,
    /// Key/value pairs.
    pub entries: BTreeMap<String, String>,
}

/// A persisted browsing session (cookies + local storage) for a persona.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// The persona this session belongs to.
    pub persona_id: String,
    /// Unix timestamp (seconds) of the snapshot.
    pub created_at: u64,
    /// Cookies across all visited sites.
    #[serde(default)]
    pub cookies: Vec<PersonaCookie>,
    /// Local storage, grouped by origin.
    #[serde(default)]
    pub local_storage: Vec<PersonaLocalStorage>,
}

impl SessionSnapshot {
    /// Creates an empty snapshot for a persona.
    pub fn new(persona_id: &str) -> Self {
        Self {
            persona_id: PersonaRecord::sanitize_id(persona_id),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
            cookies: Vec::new(),
            local_storage: Vec::new(),
        }
    }
}

/// Short listing entry (no fingerprint payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaSummary {
    /// Persona id.
    pub id: String,
    /// Human-readable label.
    pub name: Option<String>,
    /// Deterministic seed, when known.
    pub seed: Option<u64>,
    /// Creation timestamp (unix seconds).
    pub created_at: u64,
    /// The spoofed user agent.
    pub user_agent: String,
}

impl From<&PersonaRecord> for PersonaSummary {
    fn from(record: &PersonaRecord) -> Self {
        Self {
            id: record.id.clone(),
            name: record.name.clone(),
            seed: record.seed,
            created_at: record.created_at,
            user_agent: record.fingerprint.fingerprint.navigator.user_agent.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_id_replaces_bad_chars() {
        assert_eq!(PersonaRecord::sanitize_id("a b/c:d"), "a-b-c-d");
        assert_eq!(PersonaRecord::sanitize_id(".."), "persona");
        assert_eq!(PersonaRecord::sanitize_id("ok_id.1-2"), "ok_id.1-2");
    }

    #[test]
    fn validate_id_rejects_bad_ids() {
        assert!(PersonaRecord::validate_id("ok").is_ok());
        assert!(PersonaRecord::validate_id("").is_err());
        assert!(PersonaRecord::validate_id("a b").is_err());
        assert!(PersonaRecord::validate_id("../etc/passwd").is_err());
    }

    #[test]
    fn generate_is_deterministic_for_seed() {
        let req = FingerprintRequest {
            seed: Some(1234),
            ..Default::default()
        };
        let a = PersonaRecord::generate("persona-a", &req).unwrap();
        let b = PersonaRecord::generate("persona-b", &req).unwrap();
        assert_eq!(
            a.fingerprint.fingerprint.navigator.user_agent,
            b.fingerprint.fingerprint.navigator.user_agent
        );
        assert_eq!(a.seed, Some(1234));
    }

    #[test]
    fn record_roundtrips_through_json() {
        let record = PersonaRecord::generate("rt", &FingerprintRequest::default()).unwrap();
        let json = serde_json::to_string(&record).unwrap();
        let back: PersonaRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, record.id);
        assert_eq!(
            back.fingerprint.fingerprint.navigator.user_agent,
            record.fingerprint.fingerprint.navigator.user_agent
        );
    }

    #[test]
    fn snapshot_defaults_are_empty() {
        let snap = SessionSnapshot::new("../x");
        assert_eq!(snap.persona_id, "x");
        assert!(snap.cookies.is_empty());
        assert!(snap.local_storage.is_empty());
        let json = serde_json::to_string(&snap).unwrap();
        let back: SessionSnapshot = serde_json::from_str(&json).unwrap();
        assert!(back.cookies.is_empty());
    }
}
