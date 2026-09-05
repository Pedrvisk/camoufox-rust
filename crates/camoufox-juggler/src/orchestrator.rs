//! Multi-browser orchestration: persona pools with rotation state.
//!
//! [`Orchestrator`] runs N browser sessions in parallel, each bound to a
//! persona from the pool, applying a rotation policy and persisting the
//! rotation state (use counters, domain assignments) so identities stay
//! coherent across runs.
//!
//! ```no_run
//! # async fn demo() -> camoufox_core::Result<()> {
//! use std::sync::Arc;
//! use camoufox_juggler::orchestrator::{Orchestrator, OrchestratorOptions};
//! use camoufox_core::rotation::RotationPolicy;
//! use camoufox::builder::LaunchOptions;
//!
//! let orchestrator = Orchestrator::new(OrchestratorOptions {
//!     base_options: LaunchOptions::default(),
//!     store_spec: "sqlite".into(),
//!     policy: RotationPolicy::PerDomain,
//!     concurrency: 4,
//! }).await?;
//!
//! // Run four sessions against different domains in parallel.
//! let mut handles = Vec::new();
//! for domain in ["a.example", "b.example", "c.example", "d.example"] {
//!     let session = orchestrator.launch_for_domain(domain).await?;
//!     handles.push(session);
//! }
//!
//! for session in handles {
//!     // …drive each browser…
//!     session.close().await?;
//! }
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use camoufox_core::error::{CamoufoxError, Result};
use camoufox_core::persona::PersonaRecord;
use camoufox_core::rotation::{RotationContext, RotationDecision, RotationPolicy, RotationState};
use camoufox_store::{open as open_store, PersonaStore};

use camoufox::builder::LaunchOptions;

/// Orchestrator configuration.
#[derive(Debug, Clone)]
pub struct OrchestratorOptions {
    /// Base launch options; the persona is injected per session.
    pub base_options: LaunchOptions,
    /// Persona store spec (`file`, `sqlite:…`, `mysql:…`, `s3://…`).
    pub store_spec: String,
    /// Rotation policy applied before each session.
    pub policy: RotationPolicy,
    /// Maximum concurrent browsers.
    pub concurrency: usize,
}

/// Persistent rotation bookkeeping (use counters + domain assignments),
/// stored in the persona store's metadata so it survives restarts.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RotationBookkeeping {
    /// persona id → launch count.
    #[serde(default)]
    pub uses: BTreeMap<String, u64>,
    /// persona id → domains the persona has been used on.
    #[serde(default)]
    pub persona_domains: BTreeMap<String, Vec<String>>,
}

const BOOKKEEPING_PERSONA_ID: &str = "__rotation_state__";

impl RotationBookkeeping {
    /// Converts into the pure rotation-state shape.
    fn to_rotation_state(&self) -> RotationState {
        RotationState {
            uses: self.uses.clone(),
        }
    }
}

/// A running orchestrated session.
pub struct OrchestratedSession {
    /// The driven browser.
    pub browser: crate::browser::JugglerBrowser,
    /// The persona this session runs as.
    pub persona: PersonaRecord,
    /// The domain the session was launched for, when any.
    pub domain: Option<String>,
    orchestrator: Arc<OrchestratorInner>,
}

impl OrchestratedSession {
    /// Closes the browser and records the use in the rotation state.
    pub async fn close(mut self) -> Result<()> {
        let persona_id = self.persona.id.clone();
        let domain = self.domain.clone();
        let _ = self.browser.close().await;
        self.orchestrator
            .record_use(&persona_id, domain.as_deref())
            .await?;
        Ok(())
    }

    /// Closes the browser without recording the use.
    pub async fn close_untracked(mut self) -> Result<()> {
        self.browser
            .close()
            .await
            .map_err(|e| CamoufoxError::Juggler(e.to_string()))?;
        Ok(())
    }
}

struct OrchestratorInner {
    options: OrchestratorOptions,
    /// Runtime mutex over the persistent bookkeeping.
    bookkeeping: tokio::sync::Mutex<RotationBookkeeping>,
}

/// Pool of persona-driven browser sessions.
pub struct Orchestrator {
    inner: Arc<OrchestratorInner>,
}

impl Orchestrator {
    /// Creates the orchestrator, loading persisted rotation state.
    pub async fn new(options: OrchestratorOptions) -> Result<Self> {
        let bookkeeping = load_bookkeeping(&options.store_spec).await?;
        Ok(Self {
            inner: Arc::new(OrchestratorInner {
                options,
                bookkeeping: tokio::sync::Mutex::new(bookkeeping),
            }),
        })
    }

    /// The orchestrator configuration.
    pub fn options(&self) -> &OrchestratorOptions {
        &self.inner.options
    }

    /// Launches a session against a specific domain (per-domain rotation).
    pub async fn launch_for_domain(&self, domain: &str) -> Result<OrchestratedSession> {
        self.launch_inner(Some(domain)).await
    }

    /// Launches a session without a domain context.
    pub async fn launch(&self) -> Result<OrchestratedSession> {
        self.launch_inner(None).await
    }

    async fn launch_inner(&self, domain: Option<&str>) -> Result<OrchestratedSession> {
        let persona = self.inner.select_persona(domain).await?;
        let mut options = self.inner.options.base_options.clone();
        options.persona = Some(persona.clone());
        let browser = crate::driver::launch_with_juggler(&options)
            .await
            .map_err(crate::driver::core_error)?;
        Ok(OrchestratedSession {
            browser,
            persona,
            domain: domain.map(str::to_string),
            orchestrator: self.inner.clone(),
        })
    }
}

impl OrchestratorInner {
    /// Applies the rotation policy and returns the persona to run as,
    /// generating and persisting a new one when required.
    async fn select_persona(&self, domain: Option<&str>) -> Result<PersonaRecord> {
        let store = PersonaStore::new(open_store(&self.options.store_spec).await?);
        let ids: Vec<String> = store
            .list()
            .await?
            .into_iter()
            .map(|summary| summary.id)
            .collect();
        let pool: Vec<PersonaRecord> =
            futures_util::future::try_join_all(ids.iter().map(|id| store.load(id)))
                .await?
                .into_iter()
                .flatten()
                .collect();

        let bookkeeping = self.bookkeeping.lock().await;
        let current = pool.first().cloned();
        let state = bookkeeping.to_rotation_state();
        let ctx = RotationContext {
            current: current.as_ref(),
            pool: &pool,
            state: &state,
            domain,
            persona_domains: &bookkeeping.persona_domains,
        };
        let decision = self.options.policy.decide(&ctx);
        drop(bookkeeping);

        match decision {
            RotationDecision::Keep { persona_id } | RotationDecision::Rotate { persona_id, .. } => {
                match store.load(&persona_id).await? {
                    Some(record) => Ok(record),
                    // Rotation picked a persona that vanished: fall back to
                    // the first pool entry or generate.
                    None => self.generate_fallback(&store, domain).await,
                }
            }
            RotationDecision::Generate { suggested_id, .. } => {
                let request = fingerprint_request(&self.options.base_options);
                let record = store
                    .get_or_generate(&suggested_id, None::<String>, request)
                    .await?;
                Ok(record)
            }
        }
    }

    async fn generate_fallback(
        &self,
        store: &PersonaStore,
        domain: Option<&str>,
    ) -> Result<PersonaRecord> {
        let request = fingerprint_request(&self.options.base_options);
        let id = domain
            .map(|d| format!("persona-{}", d.to_ascii_lowercase()))
            .unwrap_or_else(|| "persona-pool".into());
        store.get_or_generate(&id, None::<String>, request).await
    }

    /// Records a session use and persists the state.
    async fn record_use(&self, persona_id: &str, domain: Option<&str>) -> Result<()> {
        let store_spec = self.options.store_spec.clone();
        let mut bookkeeping = self.bookkeeping.lock().await;
        *bookkeeping.uses.entry(persona_id.to_string()).or_insert(0) += 1;
        if let Some(domain) = domain {
            let entry = bookkeeping
                .persona_domains
                .entry(persona_id.to_string())
                .or_default();
            let domain = domain.to_ascii_lowercase();
            if !entry.contains(&domain) {
                entry.push(domain);
            }
        }
        let snapshot = bookkeeping.clone();
        drop(bookkeeping);
        persist_bookkeeping(&store_spec, &snapshot).await
    }
}

/// Builds a fingerprint request matching the base launch options.
fn fingerprint_request(options: &LaunchOptions) -> camoufox_core::fingerprint::FingerprintRequest {
    camoufox_core::fingerprint::FingerprintRequest {
        window: options.window,
        operating_systems: if options.os.is_empty() {
            None
        } else {
            Some(options.os.clone())
        },
        screen: options.screen,
        seed: options.fingerprint_seed,
    }
}

/// Loads the persisted bookkeeping from the persona store metadata.
async fn load_bookkeeping(store_spec: &str) -> Result<RotationBookkeeping> {
    let store = PersonaStore::new(open_store(store_spec).await?);
    match store.load(BOOKKEEPING_PERSONA_ID).await? {
        Some(record) => {
            // Recover the bookkeeping from the record's metadata.
            let metadata = &record.metadata;
            let uses = metadata
                .get("uses")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default();
            let persona_domains = metadata
                .get("persona_domains")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default();
            Ok(RotationBookkeeping {
                uses,
                persona_domains,
            })
        }
        None => Ok(RotationBookkeeping::default()),
    }
}

/// Persists the bookkeeping into the persona store metadata.
async fn persist_bookkeeping(store_spec: &str, bookkeeping: &RotationBookkeeping) -> Result<()> {
    let store = PersonaStore::new(open_store(store_spec).await?);
    let mut record = match store.load(BOOKKEEPING_PERSONA_ID).await? {
        Some(record) => record,
        None => {
            let request = camoufox_core::fingerprint::FingerprintRequest::default();
            PersonaRecord::generate(BOOKKEEPING_PERSONA_ID, &request)?
        }
    };
    record
        .metadata
        .insert("uses".into(), serde_json::to_value(&bookkeeping.uses)?);
    record.metadata.insert(
        "persona_domains".into(),
        serde_json::to_value(&bookkeeping.persona_domains)?,
    );
    store.save(&record).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookkeeping_roundtrips_through_metadata() {
        let mut bookkeeping = RotationBookkeeping::default();
        bookkeeping.uses.insert("persona-a".into(), 5);
        bookkeeping
            .persona_domains
            .insert("persona-a".into(), vec!["example.com".into()]);

        let uses: BTreeMap<String, u64> =
            serde_json::from_value(serde_json::to_value(&bookkeeping.uses).unwrap()).unwrap();
        let domains: BTreeMap<String, Vec<String>> =
            serde_json::from_value(serde_json::to_value(&bookkeeping.persona_domains).unwrap())
                .unwrap();
        let restored = RotationBookkeeping {
            uses,
            persona_domains: domains,
        };
        assert_eq!(restored.uses.get("persona-a"), Some(&5));
        assert_eq!(
            restored
                .persona_domains
                .get("persona-a")
                .map(|v| v.as_slice()),
            Some(&["example.com".to_string()][..])
        );
    }
}
