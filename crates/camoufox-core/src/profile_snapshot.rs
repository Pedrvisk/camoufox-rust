//! Virtual profiles: capture a browser profile's identity-carrying
//! files into memory so it can live in a database instead of a folder.
//!
//! A Firefox profile at rest is a directory tree where only a small
//! subset carries real state — cookies, per-origin storage, extension
//! data, certificates. Everything else (caches, telemetry, lock files,
//! shader caches) is disposable. [`snapshot_profile`] filters the tree
//! down to that subset; [`restore_profile`] materializes it back into a
//! directory that Firefox can use as a `--profile`.
//!
//! The flow that makes a profile "virtual":
//!
//! 1. before launch: [`restore_profile`] materializes the snapshot from
//!    the database into a scratch directory;
//! 2. the browser runs with `--profile <scratch dir>`;
//! 3. after a clean shutdown: [`snapshot_profile`] recaptures the
//!    directory and the snapshot goes back to the database.
//!
//! Snapshots are only valid at rest — never capture while the browser
//! is running (Firefox's inner SQLite databases may hold un-checkpointed
//! WAL data).

use std::path::{Path, PathBuf};

use crate::error::{CamoufoxError, Result};

/// One captured profile file.
///
/// `path` is relative to the profile root, using `/` separators
/// (e.g. `cookies.sqlite`, `storage/default/https example.com/ls/data.sqlite`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFile {
    /// Relative path inside the profile, `/`-separated.
    pub path: String,
    /// Raw file bytes.
    pub data: Vec<u8>,
}

/// Maximum size of a single captured file (profiles are small; anything
/// past this is cache junk or a runaway database).
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum total snapshot size.
const MAX_TOTAL_BYTES: u64 = 96 * 1024 * 1024;

/// Top-level entries (files or directories) that carry profile state.
/// Everything else is skipped.
const KEEP_ENTRIES: &[&str] = &[
    // session + auth cookies
    "cookies.sqlite",
    "cookies.sqlite-wal",
    "cookies.sqlite-shm",
    // site permissions
    "permissions.sqlite",
    "permissions.sqlite-wal",
    "permissions.sqlite-shm",
    // certificates + keys
    "cert9.db",
    "cert9.db-wal",
    "cert9.db-shm",
    "key4.db",
    "key4.db-wal",
    "key4.db-shm",
    // per-origin storage: localStorage, IndexedDB manifests, extension
    // storage
    "storage",
    // extension registrations and settings
    "browser-extension-data",
    "extensions",
    "addons.json",
    "addonStartup.json.lz4",
    "extension-preferences.json",
    "extension-settings.json",
];

/// Captures the identity-carrying files of `profile_dir`.
///
/// Returns the files in a stable order (path-sorted). Unreadable files
/// are skipped silently — a snapshot is best-effort by design.
pub fn snapshot_profile(profile_dir: &Path) -> Result<Vec<ProfileFile>> {
    if !profile_dir.is_dir() {
        return Err(CamoufoxError::Io(format!(
            "profile dir does not exist: {}",
            profile_dir.display()
        )));
    }
    let mut files = Vec::new();
    let mut total: u64 = 0;
    for entry in KEEP_ENTRIES {
        let path = profile_dir.join(entry);
        if !path.exists() {
            continue;
        }
        collect_files(&path, entry, &mut files, &mut total)?;
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Recursively collects `rel` (a file or directory relative to the
/// profile root) into `files`.
fn collect_files(
    abs: &Path,
    rel: &str,
    files: &mut Vec<ProfileFile>,
    total: &mut u64,
) -> Result<()> {
    if abs.is_dir() {
        let entries = std::fs::read_dir(abs)
            .map_err(|e| CamoufoxError::Io(format!("read dir {}: {e}", abs.display())))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = format!("{rel}/{name}");
            collect_files(&entry.path(), &child_rel, files, total)?;
        }
        return Ok(());
    }
    let meta = match std::fs::metadata(abs) {
        Ok(meta) => meta,
        Err(_) => return Ok(()),
    };
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Ok(());
    }
    if *total + meta.len() > MAX_TOTAL_BYTES {
        return Err(CamoufoxError::Io(
            "profile snapshot exceeds the total size cap".into(),
        ));
    }
    let data = match std::fs::read(abs) {
        Ok(data) => data,
        Err(_) => return Ok(()),
    };
    *total += data.len() as u64;
    files.push(ProfileFile {
        path: rel.to_string(),
        data,
    });
    Ok(())
}

/// Materializes a snapshot into `target_dir` (created when missing).
///
/// Paths are validated: no absolute paths, no `..` traversal, no
/// Windows drive letters — a snapshot loaded from an untrusted store
/// must never escape the target directory.
pub fn restore_profile(files: &[ProfileFile], target_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(target_dir)
        .map_err(|e| CamoufoxError::Io(format!("mkdir {}: {e}", target_dir.display())))?;
    let root = target_dir
        .canonicalize()
        .map_err(|e| CamoufoxError::Io(format!("canonicalize {}: {e}", target_dir.display())))?;
    for file in files {
        let rel = safe_relative_path(&file.path)?;
        let dest = root.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CamoufoxError::Io(format!("mkdir {}: {e}", parent.display())))?;
        }
        std::fs::write(&dest, &file.data)
            .map_err(|e| CamoufoxError::Io(format!("write {}: {e}", dest.display())))?;
    }
    Ok(())
}

/// Validates a snapshot path and converts it into an OS path chunk list.
fn safe_relative_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() {
        return Err(CamoufoxError::Storage("empty profile file path".into()));
    }
    // absolute paths are never valid inside a profile snapshot
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(CamoufoxError::Storage(format!(
            "profile snapshot path is absolute: {path}"
        )));
    }
    let mut clean = PathBuf::new();
    for chunk in path.split('/') {
        match chunk {
            "" | "." => continue,
            ".." => {
                return Err(CamoufoxError::Storage(format!(
                    "profile snapshot path escapes the root: {path}"
                )))
            }
            chunk if chunk.contains('\\') || chunk.contains(':') => {
                return Err(CamoufoxError::Storage(format!(
                    "invalid chunk in profile snapshot path: {path}"
                )))
            }
            chunk => clean.push(chunk),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(CamoufoxError::Storage(format!(
            "profile snapshot path has no components: {path}"
        )));
    }
    Ok(clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_fixture(dir: &Path) {
        // kept files
        std::fs::create_dir_all(dir.join("storage/default/origin/ls")).unwrap();
        std::fs::write(dir.join("cookies.sqlite"), b"cookies").unwrap();
        std::fs::write(dir.join("storage/default/origin/ls/data.sqlite"), b"ls").unwrap();
        std::fs::write(dir.join("addons.json"), b"{}").unwrap();
        // junk that must be skipped
        std::fs::create_dir_all(dir.join("cache2/entries")).unwrap();
        std::fs::write(dir.join("cache2/entries/junk"), b"junk").unwrap();
        std::fs::write(dir.join("prefs.js"), b"// prefs").unwrap();
        std::fs::write(dir.join(".parentlock"), b"").unwrap();
    }

    #[test]
    fn snapshot_keeps_only_state_files() {
        let dir = tempfile::tempdir().unwrap();
        profile_fixture(dir.path());
        let files = snapshot_profile(dir.path()).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "addons.json",
                "cookies.sqlite",
                "storage/default/origin/ls/data.sqlite"
            ]
        );
        assert_eq!(files[1].data, b"cookies");
    }

    #[test]
    fn snapshot_missing_dir_errors() {
        assert!(snapshot_profile(Path::new("/does/not/exist/at/all")).is_err());
    }

    #[test]
    fn restore_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        profile_fixture(dir.path());
        let files = snapshot_profile(dir.path()).unwrap();

        let target = tempfile::tempdir().unwrap();
        restore_profile(&files, target.path()).unwrap();
        assert_eq!(
            std::fs::read(target.path().join("cookies.sqlite")).unwrap(),
            b"cookies"
        );
        assert_eq!(
            std::fs::read(target.path().join("storage/default/origin/ls/data.sqlite")).unwrap(),
            b"ls"
        );
    }

    #[test]
    fn restore_rejects_traversal() {
        let target = tempfile::tempdir().unwrap();
        let evil = [
            ProfileFile {
                path: "../escape".into(),
                data: b"x".into(),
            },
            ProfileFile {
                path: "a/../../escape".into(),
                data: b"x".into(),
            },
            ProfileFile {
                path: "/absolute".into(),
                data: b"x".into(),
            },
            ProfileFile {
                path: "C:/windows/evil".into(),
                data: b"x".into(),
            },
            ProfileFile {
                path: "a\\b".into(),
                data: b"x".into(),
            },
        ];
        for file in &evil {
            assert!(restore_profile(std::slice::from_ref(file), target.path()).is_err());
        }
        // nothing escaped
        assert!(!target.path().parent().unwrap().join("escape").exists());
    }
}
