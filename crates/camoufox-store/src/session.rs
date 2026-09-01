//! Session persistence helpers: cookie/local-storage snapshots.
//!
//! Snapshots are captured from a live browser through the Juggler driver
//! (`camoufox-juggler`) and persisted through any [`StorageProvider`].

use camoufox_core::error::Result;
use camoufox_core::persona::{PersonaCookie, PersonaLocalStorage, SessionSnapshot};

use crate::provider::StorageProvider;

/// Save/restore of session snapshots (cookies + local storage).
///
/// Profile-level persistence (disk cache, history) is handled by reusing a
/// persistent profile directory at launch time; this type covers the parts
/// that travel with the persona regardless of where the browser runs.
pub struct SessionPersistence {
    provider: Box<dyn StorageProvider>,
}

impl SessionPersistence {
    /// Wraps a provider.
    pub fn new(provider: Box<dyn StorageProvider>) -> Self {
        Self { provider }
    }

    /// Persists a snapshot (insert-or-update).
    pub async fn save(&self, snapshot: &SessionSnapshot) -> Result<()> {
        self.provider.save_session(snapshot).await
    }

    /// Loads the latest snapshot for a persona.
    pub async fn load(&self, persona_id: &str) -> Result<Option<SessionSnapshot>> {
        self.provider
            .load_session(&camoufox_core::persona::PersonaRecord::sanitize_id(
                persona_id,
            ))
            .await
    }

    /// Deletes a persona's snapshot; returns whether it existed.
    pub async fn delete(&self, persona_id: &str) -> Result<bool> {
        self.provider
            .delete_session(&camoufox_core::persona::PersonaRecord::sanitize_id(
                persona_id,
            ))
            .await
    }

    /// Merges captured cookies/local storage into a snapshot.
    pub fn merge(
        &self,
        snapshot: &mut SessionSnapshot,
        cookies: Vec<PersonaCookie>,
        local_storage: Vec<PersonaLocalStorage>,
    ) {
        snapshot.cookies = cookies;
        snapshot.local_storage = local_storage;
    }
}
