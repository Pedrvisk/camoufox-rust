//! The persona store: provider + fingerprint-cache semantics.

use camoufox_core::error::Result;
use camoufox_core::fingerprint::FingerprintRequest;
use camoufox_core::persona::{PersonaRecord, PersonaSummary};

use crate::provider::StorageProvider;

/// High-level persona API over any [`StorageProvider`].
///
/// This is the fingerprint cache from the roadmap: identities are generated
/// once and persisted keyed by seed (through the persona id), so repeated
/// runs with the same seed reuse the same browser identity.
pub struct PersonaStore {
    provider: Box<dyn StorageProvider>,
}

impl PersonaStore {
    /// Wraps a provider.
    pub fn new(provider: Box<dyn StorageProvider>) -> Self {
        Self { provider }
    }

    /// The backing provider (diagnostics/advanced use).
    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    /// Returns the persona `id`, generating and persisting it on first use.
    ///
    /// The name is only applied on creation; stored records win on lookups.
    pub async fn get_or_generate(
        &self,
        id: &str,
        name: Option<impl Into<String>>,
        request: FingerprintRequest,
    ) -> Result<PersonaRecord> {
        let id = PersonaRecord::sanitize_id(id);
        if let Some(record) = self.provider.load_persona(&id).await? {
            return Ok(record);
        }
        let mut record = PersonaRecord::generate(&id, &request)?;
        if let Some(name) = name {
            record.name = Some(name.into());
        }
        self.provider.save_persona(&record).await?;
        Ok(record)
    }

    /// Persists a record (insert-or-update).
    pub async fn save(&self, record: &PersonaRecord) -> Result<()> {
        self.provider.save_persona(record).await
    }

    /// Loads a persona by id.
    pub async fn load(&self, id: &str) -> Result<Option<PersonaRecord>> {
        let id = PersonaRecord::sanitize_id(id);
        self.provider.load_persona(&id).await
    }

    /// Loads a persona or fails with `PersonaNotFound`.
    pub async fn require(&self, id: &str) -> Result<PersonaRecord> {
        match self.load(id).await? {
            Some(record) => Ok(record),
            None => Err(camoufox_core::error::CamoufoxError::PersonaNotFound(
                format!(
                    "persona '{id}' not found in the {} store",
                    self.provider.name()
                ),
            )),
        }
    }

    /// Deletes a persona; returns whether it existed.
    pub async fn delete(&self, id: &str) -> Result<bool> {
        let id = PersonaRecord::sanitize_id(id);
        self.provider.delete_persona(&id).await
    }

    /// Lists all personas.
    pub async fn list(&self) -> Result<Vec<PersonaSummary>> {
        self.provider.list_personas().await
    }
}
