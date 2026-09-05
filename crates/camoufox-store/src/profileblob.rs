//! Profile blob storage: database-backed virtual profiles.
//!
//! A [`ProfileBlobStore`] persists [`ProfileFile`] snapshots (see
//! `camoufox_core::profile_snapshot`) under an owner id — the "profile
//! lives in the database" flow:
//!
//! 1. [`load_profile`] → `camoufox_core::profile_snapshot::restore_profile`
//!    materializes the files into a scratch `--profile` directory;
//! 2. the browser runs;
//! 3. after a clean shutdown, `snapshot_profile` captures the directory
//!    and [`save_profile`] puts it back.
//!
//! Supported providers: file, sqlite and mysql (see
//! [`crate::provider::open_blob_store`]).

use camoufox_core::error::Result;
use camoufox_core::profile_snapshot::ProfileFile;

pub use async_trait::async_trait;

/// Persists profile snapshots keyed by owner id (e.g. a CPF, a persona
/// id, any string).
#[async_trait]
pub trait ProfileBlobStore: Send + Sync {
    /// Provider name (diagnostics).
    fn name(&self) -> &'static str;

    /// Replaces the owner's snapshot wholesale.
    async fn save_profile(&self, owner: &str, files: &[ProfileFile]) -> Result<()>;

    /// Loads the owner's snapshot; empty when the owner has none.
    async fn load_profile(&self, owner: &str) -> Result<Vec<ProfileFile>>;

    /// Deletes the owner's snapshot; returns whether it existed.
    async fn delete_profile(&self, owner: &str) -> Result<bool>;
}

/// Validates that `path` is a safe relative path for filesystem-backed
/// blob stores (mirrors `camoufox_core::profile_snapshot`'s guard).
pub(crate) fn ensure_relative(path: &str) -> camoufox_core::error::Result<()> {
    use camoufox_core::error::CamoufoxError;

    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path
            .split('/')
            .any(|chunk| chunk == ".." || chunk.contains('\\') || chunk.contains(':'))
    {
        return Err(CamoufoxError::Storage(format!(
            "unsafe profile blob path: {path}"
        )));
    }
    Ok(())
}

/// Sanitizes an owner id into a single filesystem component.
pub(crate) fn sanitize_owner(owner: &str) -> String {
    owner
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;

    async fn roundtrip(store: &dyn ProfileBlobStore) {
        let files = vec![
            ProfileFile {
                path: "cookies.sqlite".into(),
                data: b"cookies".to_vec(),
            },
            ProfileFile {
                path: "storage/default/origin/ls/data.sqlite".into(),
                data: b"ls".to_vec(),
            },
        ];
        store.save_profile("owner-1", &files).await.unwrap();
        let loaded = store.load_profile("owner-1").await.unwrap();
        assert_eq!(loaded, files);

        // replace wholesale
        let smaller = vec![ProfileFile {
            path: "cookies.sqlite".into(),
            data: b"new".to_vec(),
        }];
        store.save_profile("owner-1", &smaller).await.unwrap();
        assert_eq!(store.load_profile("owner-1").await.unwrap(), smaller);

        // missing owner → empty
        assert!(store.load_profile("nobody").await.unwrap().is_empty());

        // delete
        assert!(store.delete_profile("owner-1").await.unwrap());
        assert!(!store.delete_profile("owner-1").await.unwrap());
    }

    #[tokio::test]
    async fn sqlite_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::provider::SqliteStore::open(dir.path().join("blobs.sqlite")).unwrap();
        roundtrip(&store).await;
    }

    #[tokio::test]
    async fn file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::provider::FileStore::new(dir.path());
        roundtrip(&store).await;
    }

    #[test]
    fn relative_path_guard() {
        assert!(ensure_relative("cookies.sqlite").is_ok());
        assert!(ensure_relative("a/b/c.sqlite").is_ok());
        assert!(ensure_relative("../escape").is_err());
        assert!(ensure_relative("/absolute").is_err());
        assert!(ensure_relative("C:/x").is_err());
        assert!(ensure_relative("a\\b").is_err());
    }
}
