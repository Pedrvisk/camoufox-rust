//! # camoufox-store
//!
//! Pluggable persistence for camoufox personas and session snapshots.
//!
//! The [`StorageProvider`] trait is the provider contract; the bundled
//! implementations cover:
//!
//! - [`MemoryStore`] — in-process (tests)
//! - [`FileStore`] — one JSON document per persona, in a directory
//! - [`SqliteStore`] — a single-file SQLite database
//! - `MySqlStore` (feature `mysql`) — a shared MySQL database
//! - `S3Store` (feature `s3`) — objects under an S3-compatible prefix
//!
//! Personas are keyed by id; session snapshots (cookies + local storage) are
//! keyed by persona id. [`PersonaStore`] layers the fingerprint-cache
//! semantics on top of any provider: `get_or_generate` returns the stored
//! identity for a seed, generating and persisting it on first use.

mod profileblob;
mod provider;

pub use provider::open_blob_store;
pub use provider::{
    default_spec, default_sqlite_path, default_store_dir, open, FileStore, MemoryStore,
    ProviderSpec, StorageProvider, DEFAULT_STORE_SPEC_ENV,
};

#[cfg(feature = "mysql")]
pub use provider::MySqlStore;
#[cfg(feature = "s3")]
pub use provider::S3Store;
#[cfg(feature = "sqlite")]
pub use provider::SqliteStore;

mod persona_store;

pub use persona_store::PersonaStore;
pub use profileblob::ProfileBlobStore;

mod session;

pub use session::SessionPersistence;

#[cfg(test)]
mod tests {
    use super::*;
    use camoufox_core::FingerprintRequest;

    /// Drives the full persona API against a provider factory.
    async fn provider_suite(make: impl Fn() -> camoufox_core::Result<Box<dyn StorageProvider>>) {
        let store = PersonaStore::new(make().unwrap());

        // get_or_generate: creates then reuses.
        let first = store
            .get_or_generate(
                "seed-42",
                Some("Test Persona"),
                FingerprintRequest {
                    seed: Some(42),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(first.id, "seed-42");
        assert_eq!(first.name.as_deref(), Some("Test Persona"));

        let second = store
            .get_or_generate(
                "seed-42",
                Some("ignored on hit"),
                FingerprintRequest {
                    seed: Some(42),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            second.fingerprint.fingerprint.navigator.user_agent,
            first.fingerprint.fingerprint.navigator.user_agent
        );
        assert_eq!(
            second.name.as_deref(),
            Some("Test Persona"),
            "name is sticky"
        );

        // Listing + deletion.
        let summaries = store.list().await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "seed-42");
        assert_eq!(summaries[0].seed, Some(42));
        assert!(store.delete("seed-42").await.unwrap());
        assert!(!store.delete("seed-42").await.unwrap());
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn memory_provider_suite() {
        provider_suite(|| Ok(Box::new(MemoryStore::new()))).await;
    }

    #[tokio::test]
    async fn file_provider_suite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        provider_suite(move || Ok(Box::new(FileStore::new(path.clone())))).await;
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_provider_suite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personas.sqlite");
        provider_suite(move || Ok(Box::new(SqliteStore::open(&path)?))).await;
    }

    #[tokio::test]
    async fn file_store_layout() {
        let dir = tempfile::tempdir().unwrap();
        let store = PersonaStore::new(Box::new(FileStore::new(dir.path())));
        store
            .get_or_generate("p1", None::<&str>, FingerprintRequest::default())
            .await
            .unwrap();
        let persona_file = dir.path().join("personas").join("p1.json");
        assert!(persona_file.exists(), "missing {}", persona_file.display());
        assert!(dir.path().join("sessions").is_dir());
        let record: camoufox_core::persona::PersonaRecord =
            serde_json::from_str(&std::fs::read_to_string(&persona_file).unwrap()).unwrap();
        assert_eq!(record.id, "p1");
    }
}
