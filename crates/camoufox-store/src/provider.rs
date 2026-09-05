//! Storage backends and the provider trait.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use async_trait::async_trait;
use camoufox_core::error::{CamoufoxError, Result};
use camoufox_core::persona::{PersonaRecord, PersonaSummary, SessionSnapshot};

/// Env var with the default store spec (e.g. `sqlite:/var/lib/camoufox/personas.sqlite`).
pub const DEFAULT_STORE_SPEC_ENV: &str = "CAMOUFOX_PERSONA_STORE";

/// The persistence contract for personas and sessions.
///
/// Implementations must be safe to share across tasks (`Send + Sync`).
/// Ids are pre-sanitized by [`crate::PersonaStore`].
#[async_trait]
pub trait StorageProvider: Send + Sync {
    /// Provider name (diagnostics).
    fn name(&self) -> &'static str;

    /// Persists a persona (insert-or-update).
    async fn save_persona(&self, record: &PersonaRecord) -> Result<()>;

    /// Loads a persona by id; `Ok(None)` when missing.
    async fn load_persona(&self, id: &str) -> Result<Option<PersonaRecord>>;

    /// Deletes a persona; returns whether it existed.
    async fn delete_persona(&self, id: &str) -> Result<bool>;

    /// Lists stored personas (without fingerprint payloads).
    async fn list_personas(&self) -> Result<Vec<PersonaSummary>>;

    /// Persists a session snapshot (insert-or-update).
    async fn save_session(&self, snapshot: &SessionSnapshot) -> Result<()>;

    /// Loads the latest session snapshot for a persona.
    async fn load_session(&self, persona_id: &str) -> Result<Option<SessionSnapshot>>;

    /// Deletes a persona's session; returns whether it existed.
    async fn delete_session(&self, persona_id: &str) -> Result<bool>;
}

/// Storage destination: built-in providers plus user-provided ones.
pub enum ProviderSpec {
    /// In-memory (tests, throwaway runs).
    Memory,
    /// Directory of JSON documents.
    File(PathBuf),
    /// SQLite database file.
    Sqlite(PathBuf),
    /// MySQL DSN (`mysql://user:pass@host:port/db`).
    Mysql(String),
    /// S3 bucket + key prefix.
    S3 {
        /// Bucket name.
        bucket: String,
        /// Key prefix under which documents live.
        prefix: String,
        /// Region override (defaults to `AWS_REGION`/`AWS_DEFAULT_REGION`).
        region: Option<String>,
        /// Endpoint override (self-hosted MinIO etc.).
        endpoint: Option<String>,
    },
    /// A custom provider instance.
    Custom(Box<dyn StorageProvider>),
}

impl ProviderSpec {
    /// Parses a store spec string.
    ///
    /// Formats:
    /// - `memory`
    /// - `file` or `file:<dir>` (default dir: `~/.cache/camoufox/personas`)
    /// - `sqlite` or `sqlite:<path>` (default path: `~/.cache/camoufox/personas.sqlite`)
    /// - `mysql:<dsn>`
    /// - `s3://bucket/prefix[?region=<r>&endpoint=<url>]`
    pub fn parse(spec: &str) -> Result<Self> {
        let spec = spec.trim();
        if spec.is_empty() || spec == "memory" {
            return Ok(Self::Memory);
        }
        if let Some(rest) = spec.strip_prefix("file") {
            let rest = rest.strip_prefix(':').unwrap_or(rest).trim();
            let dir = if rest.is_empty() {
                default_store_dir()
            } else {
                PathBuf::from(expand_tilde(rest))
            };
            return Ok(Self::File(dir));
        }
        if let Some(rest) = spec.strip_prefix("sqlite") {
            let rest = rest.strip_prefix(':').unwrap_or(rest).trim();
            let path = if rest.is_empty() {
                default_sqlite_path()
            } else {
                PathBuf::from(expand_tilde(rest))
            };
            return Ok(Self::Sqlite(path));
        }
        if spec.starts_with("mysql://") {
            return Ok(Self::Mysql(spec.to_string()));
        }
        if let Some(dsn) = spec.strip_prefix("mysql:").filter(|s| !s.trim().is_empty()) {
            return Ok(Self::Mysql(dsn.trim().to_string()));
        }
        if let Some(rest) = spec.strip_prefix("s3://") {
            return parse_s3(rest);
        }
        Err(CamoufoxError::Storage(format!(
            "unknown store spec '{spec}': expected memory | file[:<dir>] | sqlite[:<path>] | \
             mysql:<dsn> | s3://<bucket>/<prefix>"
        )))
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Default persona directory: `~/.cache/camoufox/personas`.
pub fn default_store_dir() -> PathBuf {
    home_dir().join(".cache").join("camoufox").join("personas")
}

/// Default SQLite database: `~/.cache/camoufox/personas.sqlite`.
pub fn default_sqlite_path() -> PathBuf {
    home_dir()
        .join(".cache")
        .join("camoufox")
        .join("personas.sqlite")
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        home_dir().join(rest).to_string_lossy().into_owned()
    } else {
        path.to_string()
    }
}

fn parse_s3(rest: &str) -> Result<ProviderSpec> {
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    let (bucket, prefix) = match path.split_once('/') {
        Some((b, p)) => (b.to_string(), p.trim_matches('/').to_string()),
        None => (path.to_string(), String::new()),
    };
    if bucket.is_empty() {
        return Err(CamoufoxError::Storage(
            "s3 store spec requires a bucket: s3://<bucket>/<prefix>".into(),
        ));
    }
    let mut region = None;
    let mut endpoint = None;
    if let Some(query) = query {
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "region" => region = Some(percent_decode(value)),
                "endpoint" => endpoint = Some(percent_decode(value)),
                _ => {}
            }
        }
    }
    Ok(ProviderSpec::S3 {
        bucket,
        prefix,
        region,
        endpoint,
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn storage_io(context: &str, err: impl std::fmt::Display) -> CamoufoxError {
    CamoufoxError::Storage(format!("{context}: {err}"))
}

/// Builds a provider from a spec.
///
/// Use [`default_spec`] for the ambient default (`CAMOUFOX_PERSONA_STORE`).
pub async fn open(spec: &str) -> Result<Box<dyn StorageProvider>> {
    match ProviderSpec::parse(spec)? {
        ProviderSpec::Memory => Ok(Box::new(MemoryStore::new())),
        ProviderSpec::File(dir) => Ok(Box::new(FileStore::new(dir))),
        #[cfg(feature = "sqlite")]
        ProviderSpec::Sqlite(path) => Ok(Box::new(SqliteStore::open(&path)?)),
        #[cfg(not(feature = "sqlite"))]
        ProviderSpec::Sqlite(_) => Err(CamoufoxError::Storage(
            "sqlite store requires the `sqlite` feature".into(),
        )),
        #[cfg(feature = "mysql")]
        ProviderSpec::Mysql(dsn) => {
            let store = MySqlStore::connect(&dsn).await?;
            Ok(Box::new(store))
        }
        #[cfg(not(feature = "mysql"))]
        ProviderSpec::Mysql(_) => Err(CamoufoxError::Storage(
            "mysql store requires the `mysql` feature".into(),
        )),
        #[cfg(feature = "s3")]
        ProviderSpec::S3 {
            bucket,
            prefix,
            region,
            endpoint,
        } => Ok(Box::new(S3Store::new(bucket, prefix, region, endpoint))),
        #[cfg(not(feature = "s3"))]
        ProviderSpec::S3 { .. } => Err(CamoufoxError::Storage(
            "s3 store requires the `s3` feature".into(),
        )),
        ProviderSpec::Custom(provider) => Ok(provider),
    }
}

/// The default store spec: [`DEFAULT_STORE_SPEC_ENV`] or `file` (default dir).
pub fn default_spec() -> String {
    std::env::var(DEFAULT_STORE_SPEC_ENV).unwrap_or_else(|_| "file".into())
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// In-process provider. Every instance is its own database.
pub struct MemoryStore {
    personas: RwLock<BTreeMap<String, PersonaRecord>>,
    sessions: RwLock<BTreeMap<String, SessionSnapshot>>,
}

impl MemoryStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self {
            personas: RwLock::new(BTreeMap::new()),
            sessions: RwLock::new(BTreeMap::new()),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StorageProvider for MemoryStore {
    fn name(&self) -> &'static str {
        "memory"
    }

    async fn save_persona(&self, record: &PersonaRecord) -> Result<()> {
        self.personas
            .write()
            .map_err(|e| storage_io("memory lock", e))?
            .insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn load_persona(&self, id: &str) -> Result<Option<PersonaRecord>> {
        Ok(self
            .personas
            .read()
            .map_err(|e| storage_io("memory lock", e))?
            .get(id)
            .cloned())
    }

    async fn delete_persona(&self, id: &str) -> Result<bool> {
        Ok(self
            .personas
            .write()
            .map_err(|e| storage_io("memory lock", e))?
            .remove(id)
            .is_some())
    }

    async fn list_personas(&self) -> Result<Vec<PersonaSummary>> {
        Ok(self
            .personas
            .read()
            .map_err(|e| storage_io("memory lock", e))?
            .values()
            .map(PersonaSummary::from)
            .collect())
    }

    async fn save_session(&self, snapshot: &SessionSnapshot) -> Result<()> {
        self.sessions
            .write()
            .map_err(|e| storage_io("memory lock", e))?
            .insert(snapshot.persona_id.clone(), snapshot.clone());
        Ok(())
    }

    async fn load_session(&self, persona_id: &str) -> Result<Option<SessionSnapshot>> {
        Ok(self
            .sessions
            .read()
            .map_err(|e| storage_io("memory lock", e))?
            .get(persona_id)
            .cloned())
    }

    async fn delete_session(&self, persona_id: &str) -> Result<bool> {
        Ok(self
            .sessions
            .write()
            .map_err(|e| storage_io("memory lock", e))?
            .remove(persona_id)
            .is_some())
    }
}

// ---------------------------------------------------------------------------
// File
// ---------------------------------------------------------------------------

/// One JSON document per persona: `<dir>/personas/<id>.json` and
/// `<dir>/sessions/<id>.json`. Writes are atomic (tmp + rename).
pub struct FileStore {
    personas_dir: PathBuf,
    sessions_dir: PathBuf,
    profiles_dir: PathBuf,
}

impl FileStore {
    /// Creates (and prepares) a store rooted at `dir`.
    pub fn new(dir: impl AsRef<Path>) -> Self {
        let root = dir.as_ref().to_path_buf();
        let personas_dir = root.join("personas");
        let sessions_dir = root.join("sessions");
        let profiles_dir = root.join("profiles");
        for dir in [&personas_dir, &sessions_dir, &profiles_dir] {
            let _ = std::fs::create_dir_all(dir);
        }
        Self {
            personas_dir,
            sessions_dir,
            profiles_dir,
        }
    }

    fn persona_path(&self, id: &str) -> PathBuf {
        self.personas_dir.join(format!("{id}.json"))
    }

    fn session_path(&self, id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{id}.json"))
    }
}

#[async_trait]
impl StorageProvider for FileStore {
    fn name(&self) -> &'static str {
        "file"
    }

    async fn save_persona(&self, record: &PersonaRecord) -> Result<()> {
        let path = self.persona_path(&record.id);
        let json = serde_json::to_string_pretty(record)?;
        let mut tmp = path.clone();
        tmp.set_extension("json.tmp");
        tokio::fs::write(&tmp, json)
            .await
            .map_err(|e| storage_io(&format!("write {}", tmp.display()), e))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| storage_io(&format!("rename {}", path.display()), e))?;
        Ok(())
    }

    async fn load_persona(&self, id: &str) -> Result<Option<PersonaRecord>> {
        let path = self.persona_path(id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let record: PersonaRecord = serde_json::from_slice(&bytes)
                    .map_err(|e| storage_io(&format!("parse {}", path.display()), e))?;
                Ok(Some(record))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage_io(&format!("read {}", path.display()), e)),
        }
    }

    async fn delete_persona(&self, id: &str) -> Result<bool> {
        match tokio::fs::remove_file(self.persona_path(id)).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(storage_io("delete persona", e)),
        }
    }

    async fn list_personas(&self) -> Result<Vec<PersonaSummary>> {
        let mut entries = tokio::fs::read_dir(&self.personas_dir)
            .await
            .map_err(|e| storage_io(&format!("read_dir {}", self.personas_dir.display()), e))?;
        let mut paths = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| storage_io("read_dir entry", e))?
        {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                paths.push(path);
            }
        }
        paths.sort();
        let mut summaries = Vec::new();
        for path in paths {
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    if let Ok(record) = serde_json::from_slice::<PersonaRecord>(&bytes) {
                        summaries.push(PersonaSummary::from(&record));
                    } else {
                        log::warn!("skipping unparseable persona file {}", path.display());
                    }
                }
                Err(e) => log::warn!("skipping unreadable persona file {}: {e}", path.display()),
            }
        }
        Ok(summaries)
    }

    async fn save_session(&self, snapshot: &SessionSnapshot) -> Result<()> {
        let path = self.session_path(&snapshot.persona_id);
        let json = serde_json::to_string_pretty(snapshot)?;
        let mut tmp = path.clone();
        tmp.set_extension("json.tmp");
        tokio::fs::write(&tmp, json)
            .await
            .map_err(|e| storage_io(&format!("write {}", tmp.display()), e))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| storage_io(&format!("rename {}", path.display()), e))?;
        Ok(())
    }

    async fn load_session(&self, persona_id: &str) -> Result<Option<SessionSnapshot>> {
        let path = self.session_path(persona_id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let snapshot: SessionSnapshot = serde_json::from_slice(&bytes)
                    .map_err(|e| storage_io(&format!("parse {}", path.display()), e))?;
                Ok(Some(snapshot))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage_io(&format!("read {}", path.display()), e)),
        }
    }

    async fn delete_session(&self, persona_id: &str) -> Result<bool> {
        match tokio::fs::remove_file(self.session_path(persona_id)).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(storage_io("delete session", e)),
        }
    }
}

// ---------------------------------------------------------------------------
// SQLite
// ---------------------------------------------------------------------------

#[cfg(feature = "sqlite")]
mod sqlite_impl {
    use super::*;
    use camoufox_core::persona::{PersonaRecord, PersonaSummary, SessionSnapshot};
    use rusqlite::{Connection, OptionalExtension};
    use std::sync::Mutex;

    const SCHEMA: &str = "
        CREATE TABLE IF NOT EXISTS personas (
            id         TEXT PRIMARY KEY,
            name       TEXT,
            seed       INTEGER,
            created_at INTEGER NOT NULL,
            data       TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sessions (
            persona_id TEXT PRIMARY KEY,
            created_at INTEGER NOT NULL,
            data       TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS profile_files (
            owner TEXT NOT NULL,
            path  TEXT NOT NULL,
            data  BLOB NOT NULL,
            PRIMARY KEY (owner, path)
        );
    ";

    /// SQLite-backed provider (bundled SQLite; no server needed).
    pub struct SqliteStore {
        conn: Mutex<Connection>,
    }

    impl SqliteStore {
        /// Opens (creating when needed) the database at `path`.
        pub fn open(path: impl AsRef<Path>) -> Result<Self> {
            if let Some(parent) = path.as_ref().parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let display = path.as_ref().display().to_string();
            let conn =
                Connection::open(path).map_err(|e| storage_io(&format!("open {display}"), e))?;
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(|e| storage_io("set WAL", e))?;
            conn.execute_batch(SCHEMA)
                .map_err(|e| storage_io("create schema", e))?;
            Ok(Self {
                conn: Mutex::new(conn),
            })
        }

        pub(crate) fn with_conn<T>(
            &self,
            context: &str,
            f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
        ) -> Result<T> {
            let conn = self.conn.lock().map_err(|e| storage_io("sqlite lock", e))?;
            f(&conn).map_err(|e| storage_io(context, e))
        }
    }

    #[async_trait]
    impl StorageProvider for SqliteStore {
        fn name(&self) -> &'static str {
            "sqlite"
        }

        async fn save_persona(&self, record: &PersonaRecord) -> Result<()> {
            let data = serde_json::to_string(record)?;
            self.with_conn("save persona", |conn| {
                conn.execute(
                    "INSERT INTO personas (id, name, seed, created_at, data) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(id) DO UPDATE SET name=?2, seed=?3, created_at=?4, data=?5",
                    rusqlite::params![
                        record.id,
                        record.name,
                        record.seed.map(|s| s as i64),
                        record.created_at as i64,
                        data
                    ],
                )
                .map(|_| ())
            })
        }

        async fn load_persona(&self, id: &str) -> Result<Option<PersonaRecord>> {
            let id = id.to_string();
            self.with_conn("load persona", |conn| {
                conn.query_row("SELECT data FROM personas WHERE id = ?1", [&id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()
            })
            .map(|data| data.map(|d| serde_json::from_str(&d)).transpose())?
            .map_err(|e| storage_io("parse persona", e))
        }

        async fn delete_persona(&self, id: &str) -> Result<bool> {
            self.with_conn("delete persona", |conn| {
                conn.execute("DELETE FROM personas WHERE id = ?1", [id])
            })
            .map(|n| n > 0)
        }

        async fn list_personas(&self) -> Result<Vec<PersonaSummary>> {
            self.with_conn("list personas", |conn| {
                let mut stmt = conn.prepare("SELECT data FROM personas")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            })
            .map(|datas| {
                datas
                    .iter()
                    .filter_map(|d| serde_json::from_str::<PersonaRecord>(d).ok())
                    .map(|record| PersonaSummary::from(&record))
                    .collect()
            })
        }

        async fn save_session(&self, snapshot: &SessionSnapshot) -> Result<()> {
            let data = serde_json::to_string(snapshot)?;
            self.with_conn("save session", |conn| {
                conn.execute(
                    "INSERT INTO sessions (persona_id, created_at, data) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(persona_id) DO UPDATE SET created_at=?2, data=?3",
                    rusqlite::params![snapshot.persona_id, snapshot.created_at as i64, data],
                )
                .map(|_| ())
            })
        }

        async fn load_session(&self, persona_id: &str) -> Result<Option<SessionSnapshot>> {
            let persona_id = persona_id.to_string();
            self.with_conn("load session", |conn| {
                conn.query_row(
                    "SELECT data FROM sessions WHERE persona_id = ?1",
                    [&persona_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })
            .map(|data| data.map(|d| serde_json::from_str(&d)).transpose())?
            .map_err(|e| storage_io("parse session", e))
        }

        async fn delete_session(&self, persona_id: &str) -> Result<bool> {
            self.with_conn("delete session", |conn| {
                conn.execute("DELETE FROM sessions WHERE persona_id = ?1", [persona_id])
            })
            .map(|n| n > 0)
        }
    }
}

#[cfg(feature = "sqlite")]
pub use sqlite_impl::SqliteStore;

// ---------------------------------------------------------------------------
// MySQL
// ---------------------------------------------------------------------------

#[cfg(feature = "mysql")]
mod mysql_impl {
    use super::*;
    use camoufox_core::persona::{PersonaRecord, PersonaSummary, SessionSnapshot};
    use sqlx::mysql::MySqlPool;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// MySQL-backed provider for shared persona stores.
    pub struct MySqlStore {
        pub(crate) pool: MySqlPool,
        dsn: String,
        schema_ready: AtomicBool,
    }

    const SCHEMA: &str = "
        CREATE TABLE IF NOT EXISTS personas (
            id         VARCHAR(128) PRIMARY KEY,
            name       VARCHAR(255),
            seed       BIGINT UNSIGNED NULL,
            created_at BIGINT UNSIGNED NOT NULL,
            data       LONGTEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sessions (
            persona_id VARCHAR(128) PRIMARY KEY,
            created_at BIGINT UNSIGNED NOT NULL,
            data       LONGTEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS profile_files (
            owner VARCHAR(128) NOT NULL,
            path  VARCHAR(512) NOT NULL,
            data  LONGBLOB NOT NULL,
            PRIMARY KEY (owner, path)
        );
    ";

    impl MySqlStore {
        /// Connects and ensures the schema exists.
        pub async fn connect(dsn: &str) -> Result<Self> {
            let pool = MySqlPool::connect(dsn)
                .await
                .map_err(|e| storage_io(&format!("mysql connect {}", Self::redact_dsn(dsn)), e))?;
            let store = Self {
                pool,
                dsn: dsn.to_string(),
                schema_ready: AtomicBool::new(false),
            };
            store.ensure_schema().await?;
            Ok(store)
        }

        fn redact_dsn(dsn: &str) -> String {
            match dsn.split_once('@') {
                Some((_, rest)) => format!("mysql://***@{rest}"),
                None => "mysql:<dsn>".into(),
            }
        }

        pub(crate) async fn ensure_schema(&self) -> Result<()> {
            if self.schema_ready.load(Ordering::Relaxed) {
                return Ok(());
            }
            sqlx::query(SCHEMA)
                .execute(&self.pool)
                .await
                .map_err(|e| storage_io("mysql create schema", e))?;
            self.schema_ready.store(true, Ordering::Relaxed);
            Ok(())
        }

        /// Verifies connectivity.
        pub async fn ping(&self) -> Result<()> {
            sqlx::query("SELECT 1")
                .execute(&self.pool)
                .await
                .map_err(|e| {
                    storage_io(&format!("mysql ping {}", Self::redact_dsn(&self.dsn)), e)
                })?;
            Ok(())
        }
    }

    #[async_trait]
    impl StorageProvider for MySqlStore {
        fn name(&self) -> &'static str {
            "mysql"
        }

        async fn save_persona(&self, record: &PersonaRecord) -> Result<()> {
            self.ensure_schema().await?;
            let data = serde_json::to_string(record)?;
            sqlx::query(
                "INSERT INTO personas (id, name, seed, created_at, data) VALUES (?, ?, ?, ?, ?) \
                 ON DUPLICATE KEY UPDATE name=VALUES(name), seed=VALUES(seed), \
                 created_at=VALUES(created_at), data=VALUES(data)",
            )
            .bind(&record.id)
            .bind(&record.name)
            .bind(record.seed)
            .bind(record.created_at)
            .bind(&data)
            .execute(&self.pool)
            .await
            .map_err(|e| storage_io("mysql save persona", e))?;
            Ok(())
        }

        async fn load_persona(&self, id: &str) -> Result<Option<PersonaRecord>> {
            self.ensure_schema().await?;
            let row: Option<(String,)> = sqlx::query_as("SELECT data FROM personas WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| storage_io("mysql load persona", e))?;
            row.map(|(d,)| serde_json::from_str(&d))
                .transpose()
                .map_err(|e| storage_io("mysql parse persona", e))
        }

        async fn delete_persona(&self, id: &str) -> Result<bool> {
            self.ensure_schema().await?;
            let result = sqlx::query("DELETE FROM personas WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(|e| storage_io("mysql delete persona", e))?;
            Ok(result.rows_affected() > 0)
        }

        async fn list_personas(&self) -> Result<Vec<PersonaSummary>> {
            self.ensure_schema().await?;
            let rows: Vec<(String,)> = sqlx::query_as("SELECT data FROM personas")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| storage_io("mysql list personas", e))?;
            let mut summaries: Vec<PersonaSummary> = rows
                .iter()
                .filter_map(|(d,)| serde_json::from_str::<PersonaRecord>(d).ok())
                .map(|record| PersonaSummary::from(&record))
                .collect();
            summaries.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(summaries)
        }

        async fn save_session(&self, snapshot: &SessionSnapshot) -> Result<()> {
            self.ensure_schema().await?;
            let data = serde_json::to_string(snapshot)?;
            sqlx::query(
                "INSERT INTO sessions (persona_id, created_at, data) VALUES (?, ?, ?) \
                 ON DUPLICATE KEY UPDATE created_at=VALUES(created_at), data=VALUES(data)",
            )
            .bind(&snapshot.persona_id)
            .bind(snapshot.created_at)
            .bind(&data)
            .execute(&self.pool)
            .await
            .map_err(|e| storage_io("mysql save session", e))?;
            Ok(())
        }

        async fn load_session(&self, persona_id: &str) -> Result<Option<SessionSnapshot>> {
            self.ensure_schema().await?;
            let row: Option<(String,)> =
                sqlx::query_as("SELECT data FROM sessions WHERE persona_id = ?")
                    .bind(persona_id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| storage_io("mysql load session", e))?;
            row.map(|(d,)| serde_json::from_str(&d))
                .transpose()
                .map_err(|e| storage_io("mysql parse session", e))
        }

        async fn delete_session(&self, persona_id: &str) -> Result<bool> {
            self.ensure_schema().await?;
            let result = sqlx::query("DELETE FROM sessions WHERE persona_id = ?")
                .bind(persona_id)
                .execute(&self.pool)
                .await
                .map_err(|e| storage_io("mysql delete session", e))?;
            Ok(result.rows_affected() > 0)
        }
    }
}

#[cfg(feature = "mysql")]
pub use mysql_impl::MySqlStore;

// ---------------------------------------------------------------------------
// S3
// ---------------------------------------------------------------------------

#[cfg(feature = "s3")]
mod s3_impl {
    use super::*;
    use camoufox_core::persona::{PersonaRecord, PersonaSummary, SessionSnapshot};
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    type HmacSha256 = Hmac<Sha256>;

    /// S3-compatible provider (AWS S3, MinIO, R2…) using SigV4 over HTTPS.
    ///
    /// Credentials come from the standard AWS environment variables
    /// (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, optional
    /// `AWS_SESSION_TOKEN`).
    pub struct S3Store {
        bucket: String,
        prefix: String,
        region: String,
        endpoint: Option<String>,
        access_key: String,
        secret_key: String,
        session_token: Option<String>,
        client: reqwest::Client,
    }

    impl S3Store {
        /// Creates the store; region/endpoint fall back to the environment.
        pub fn new(
            bucket: impl Into<String>,
            prefix: impl Into<String>,
            region: Option<String>,
            endpoint: Option<String>,
        ) -> Self {
            let region = region
                .or_else(|| std::env::var("AWS_REGION").ok())
                .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
                .unwrap_or_else(|| "us-east-1".into());
            Self {
                bucket: bucket.into(),
                prefix: prefix.into().trim_matches('/').to_string(),
                region,
                endpoint,
                access_key: std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_default(),
                secret_key: std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_default(),
                session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
                client: reqwest::Client::new(),
            }
        }

        fn key(&self, document: &str) -> String {
            if self.prefix.is_empty() {
                document.to_string()
            } else {
                format!("{}/{}", self.prefix, document)
            }
        }

        fn persona_key(&self, id: &str) -> String {
            self.key(&format!("personas/{id}.json"))
        }

        fn session_key(&self, id: &str) -> String {
            self.key(&format!("sessions/{id}.json"))
        }

        fn host(&self) -> String {
            if let Some(endpoint) = &self.endpoint {
                return endpoint
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_string();
            }
            format!("s3.{}.amazonaws.com", self.region)
        }

        fn scheme(&self) -> &str {
            match &self.endpoint {
                Some(endpoint) if endpoint.starts_with("http://") => "http",
                _ => "https",
            }
        }

        async fn request(
            &self,
            method: &str,
            key: &str,
            query: &str,
            body: Option<Vec<u8>>,
        ) -> Result<reqwest::Response> {
            if self.access_key.is_empty() || self.secret_key.is_empty() {
                return Err(CamoufoxError::Storage(
                    "s3 store requires AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY".into(),
                ));
            }
            let host = self.host();
            let canonical_uri = format!(
                "/{}/{}",
                uri_encode(&self.bucket, false),
                uri_encode(key, false)
            );
            let now = iso8601_now();
            let payload_hash = match &body {
                Some(bytes) => hex(&Sha256::digest(bytes)),
                None => hex(&Sha256::digest(&[] as &[u8])),
            };

            let canonical_query = query.to_string();
            let mut canonical_headers =
                format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{now}\n");
            let mut signed_headers = "host;x-amz-content-sha256;x-amz-date".to_string();
            if let Some(token) = &self.session_token {
                canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
                signed_headers.push_str(";x-amz-security-token");
            }

            let canonical_request = format!(
                "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
            );
            let scope = format!("{}/{}/s3/aws4_request", &now[..8], self.region);
            let string_to_sign = format!(
                "AWS4-HMAC-SHA256\n{now}\n{scope}\n{}",
                hex(&Sha256::digest(canonical_request.as_bytes()))
            );

            let mut signing_key = hmac_chain(&self.secret_key, &now[..8], &self.region);
            signing_key.update(string_to_sign.as_bytes());
            let signature = hex(&signing_key.finalize().into_bytes());

            let authorization = format!(
                "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
                self.access_key, scope, signed_headers, signature
            );

            let url = format!("{}://{}/{}{}", self.scheme(), host, canonical_uri, query);
            let method_name = method.to_string();
            let reqwest_method = reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|e| storage_io("method", e))?;
            let mut request = self.client.request(reqwest_method, &url);
            request = request
                .header("x-amz-date", &now)
                .header("x-amz-content-sha256", &payload_hash)
                .header("Authorization", authorization);
            if let Some(token) = &self.session_token {
                request = request.header("x-amz-security-token", token);
            }
            if let Some(bytes) = body {
                request = request.body(bytes);
            }
            request
                .send()
                .await
                .map_err(|e| storage_io(&format!("s3 {method_name} {key}"), e))
        }

        async fn get_object(&self, key: &str) -> Result<Option<Vec<u8>>> {
            let response = self.request("GET", key, "", None).await?;
            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if status != reqwest::StatusCode::OK {
                return Err(CamoufoxError::Storage(format!(
                    "s3 GET {key} failed: {status}"
                )));
            }
            response
                .bytes()
                .await
                .map(|b| Some(b.to_vec()))
                .map_err(|e| storage_io("s3 read body", e))
        }

        async fn put_object(&self, key: &str, bytes: Vec<u8>) -> Result<()> {
            let response = self.request("PUT", key, "", Some(bytes)).await?;
            let status = response.status();
            if !status.is_success() {
                return Err(CamoufoxError::Storage(format!(
                    "s3 PUT {key} failed: {status}"
                )));
            }
            Ok(())
        }

        async fn delete_object(&self, key: &str) -> Result<bool> {
            let response = self.request("DELETE", key, "", None).await?;
            match response.status() {
                reqwest::StatusCode::NO_CONTENT | reqwest::StatusCode::OK => Ok(true),
                reqwest::StatusCode::NOT_FOUND => Ok(false),
                status => Err(CamoufoxError::Storage(format!(
                    "s3 DELETE {key} failed: {status}"
                ))),
            }
        }
    }

    /// AWS URI encoding. `encode_slash = false` for canonical paths.
    fn uri_encode(value: &str, encode_slash: bool) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(byte as char)
                }
                b'/' if !encode_slash => out.push('/'),
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }

    /// ISO8601 basic format (no chrono dependency).
    fn iso8601_now() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let (year, month, day) = civil_from_days((now / 86400) as i64);
        let secs_of_day = now % 86400;
        format!(
            "{year:04}{month:02}{day:02}T{h:02}{m:02}{s:02}Z",
            h = secs_of_day / 3600,
            m = (secs_of_day % 3600) / 60,
            s = secs_of_day % 60
        )
    }

    /// Howard Hinnant's civil-from-days algorithm.
    fn civil_from_days(z: i64) -> (i64, u32, u32) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if m <= 2 { y + 1 } else { y }, m, d)
    }

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    fn hmac_chain(secret: &str, date: &str, region: &str) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(format!("AWS4{secret}").as_bytes())
            .expect("hmac accepts any key length");
        mac.update(date.as_bytes());
        let date_key = mac.finalize().into_bytes();

        let mut mac = HmacSha256::new_from_slice(&date_key).unwrap();
        mac.update(region.as_bytes());
        let region_key = mac.finalize().into_bytes();

        let mut mac = HmacSha256::new_from_slice(&region_key).unwrap();
        mac.update(b"s3");
        let service_key = mac.finalize().into_bytes();

        let mut mac = HmacSha256::new_from_slice(&service_key).unwrap();
        mac.update(b"aws4_request");
        mac
    }

    #[async_trait]
    impl StorageProvider for S3Store {
        fn name(&self) -> &'static str {
            "s3"
        }

        async fn save_persona(&self, record: &PersonaRecord) -> Result<()> {
            let key = self.persona_key(&record.id);
            let bytes = serde_json::to_vec_pretty(record)?;
            self.put_object(&key, bytes).await
        }

        async fn load_persona(&self, id: &str) -> Result<Option<PersonaRecord>> {
            match self.get_object(&self.persona_key(id)).await? {
                Some(bytes) => Ok(Some(
                    serde_json::from_slice(&bytes)
                        .map_err(|e| storage_io("s3 parse persona", e))?,
                )),
                None => Ok(None),
            }
        }

        async fn delete_persona(&self, id: &str) -> Result<bool> {
            self.delete_object(&self.persona_key(id)).await
        }

        async fn list_personas(&self) -> Result<Vec<PersonaSummary>> {
            let prefix = if self.prefix.is_empty() {
                "personas/".to_string()
            } else {
                format!("{}/personas/", self.prefix)
            };
            let query = format!("list-type=2&prefix={}", uri_encode(&prefix, true));
            let response = self.request("GET", "", &query, None).await?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|e| storage_io("s3 list body", e))?;
            if !status.is_success() {
                return Err(CamoufoxError::Storage(format!("s3 LIST failed: {status}")));
            }
            let mut summaries = Vec::new();
            for segment in body.split("<Key>").skip(1) {
                let key = segment.split("</Key>").next().unwrap_or("");
                let id = key
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(".json");
                if id.is_empty() {
                    continue;
                }
                if let Ok(Some(record)) = self.load_persona(id).await {
                    summaries.push(PersonaSummary::from(&record));
                }
            }
            summaries.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(summaries)
        }

        async fn save_session(&self, snapshot: &SessionSnapshot) -> Result<()> {
            let key = self.session_key(&snapshot.persona_id);
            let bytes = serde_json::to_vec_pretty(snapshot)?;
            self.put_object(&key, bytes).await
        }

        async fn load_session(&self, persona_id: &str) -> Result<Option<SessionSnapshot>> {
            match self.get_object(&self.session_key(persona_id)).await? {
                Some(bytes) => Ok(Some(
                    serde_json::from_slice(&bytes)
                        .map_err(|e| storage_io("s3 parse session", e))?,
                )),
                None => Ok(None),
            }
        }

        async fn delete_session(&self, persona_id: &str) -> Result<bool> {
            self.delete_object(&self.session_key(persona_id)).await
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn civil_from_days_matches_known_dates() {
            // 19723 days = 2024-01-01, 20697 days = 2026-09-01.
            assert_eq!(civil_from_days(19723), (2024, 1, 1));
            assert_eq!(civil_from_days(20697), (2026, 9, 1));
            assert_eq!(civil_from_days(0), (1970, 1, 1));
        }

        #[test]
        fn uri_encoding() {
            assert_eq!(uri_encode("a b/c", false), "a%20b/c");
            assert_eq!(uri_encode("a b/c", true), "a%20b%2Fc");
            assert_eq!(uri_encode("safe-AZ_09.~-", true), "safe-AZ_09.~-");
        }

        #[test]
        fn iso8601_shape() {
            let now = iso8601_now();
            assert_eq!(now.len(), 16);
            assert!(now.ends_with('Z'));
            assert!(now.contains('T'));
            assert!(now[..8].chars().all(|c| c.is_ascii_digit()));
        }
    }
}

#[cfg(feature = "s3")]
pub use s3_impl::S3Store;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_specs() {
        assert!(matches!(
            ProviderSpec::parse("memory").unwrap(),
            ProviderSpec::Memory
        ));
        assert!(matches!(
            ProviderSpec::parse("file:/tmp/personas").unwrap(),
            ProviderSpec::File(p) if p == Path::new("/tmp/personas")
        ));
        assert!(matches!(
            ProviderSpec::parse("sqlite:/tmp/p.sqlite").unwrap(),
            ProviderSpec::Sqlite(p) if p == Path::new("/tmp/p.sqlite")
        ));
        assert!(matches!(
            ProviderSpec::parse("mysql://u:p@h/db").unwrap(),
            ProviderSpec::Mysql(d) if d == "mysql://u:p@h/db"
        ));
        match ProviderSpec::parse("s3://bucket/pfx?region=sa-east-1").unwrap() {
            ProviderSpec::S3 {
                bucket,
                prefix,
                region,
                ..
            } => {
                assert_eq!(bucket, "bucket");
                assert_eq!(prefix, "pfx");
                assert_eq!(region.as_deref(), Some("sa-east-1"));
            }
            _ => panic!("expected s3"),
        }
        assert!(ProviderSpec::parse("weird").is_err());
        assert!(ProviderSpec::parse("mysql:").is_err());
    }
}

// ---------------------------------------------------------------------------
// Profile blobs (virtual profiles)
// ---------------------------------------------------------------------------

use crate::profileblob::{ensure_relative, sanitize_owner, ProfileBlobStore};
use camoufox_core::profile_snapshot::ProfileFile;

/// Builds a profile-blob store from a spec.
///
/// Same spec formats as [`open`]; blobs are unsupported on the memory and
/// S3 providers (use file/sqlite/mysql).
pub async fn open_blob_store(spec: &str) -> Result<Box<dyn ProfileBlobStore>> {
    match ProviderSpec::parse(spec)? {
        ProviderSpec::File(dir) => Ok(Box::new(FileStore::new(dir))),
        #[cfg(feature = "sqlite")]
        ProviderSpec::Sqlite(path) => Ok(Box::new(SqliteStore::open(&path)?)),
        #[cfg(not(feature = "sqlite"))]
        ProviderSpec::Sqlite(_) => Err(CamoufoxError::Storage(
            "sqlite blob store requires the `sqlite` feature".into(),
        )),
        #[cfg(feature = "mysql")]
        ProviderSpec::Mysql(dsn) => {
            let store = MySqlStore::connect(&dsn).await?;
            Ok(Box::new(store))
        }
        #[cfg(not(feature = "mysql"))]
        ProviderSpec::Mysql(_) => Err(CamoufoxError::Storage(
            "mysql blob store requires the `mysql` feature".into(),
        )),
        ProviderSpec::Memory => Err(CamoufoxError::Storage(
            "profile blobs are not supported by the memory provider".into(),
        )),
        ProviderSpec::S3 { .. } => Err(CamoufoxError::Storage(
            "profile blobs are not supported by the S3 provider yet".into(),
        )),
        ProviderSpec::Custom(_) => Err(CamoufoxError::Storage(
            "profile blobs are not supported by custom providers".into(),
        )),
    }
}

#[async_trait]
impl ProfileBlobStore for FileStore {
    fn name(&self) -> &'static str {
        "file"
    }

    async fn save_profile(&self, owner: &str, files: &[ProfileFile]) -> Result<()> {
        let dir = self.profiles_dir.join(sanitize_owner(owner));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| storage_io("create profile dir", e))?;
        for file in files {
            ensure_relative(&file.path)?;
            let dest = dir.join(&file.path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| storage_io("create profile subdir", e))?;
            }
            std::fs::write(&dest, &file.data).map_err(|e| storage_io("write profile file", e))?;
        }
        Ok(())
    }

    async fn load_profile(&self, owner: &str) -> Result<Vec<ProfileFile>> {
        let dir = self.profiles_dir.join(sanitize_owner(owner));
        let mut files = Vec::new();
        collect_profile_files(&dir, &dir, &mut files)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }

    async fn delete_profile(&self, owner: &str) -> Result<bool> {
        let dir = self.profiles_dir.join(sanitize_owner(owner));
        if !dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir).map_err(|e| storage_io("remove profile dir", e))?;
        Ok(true)
    }
}

/// Recursively collects files under `root` with `/`-separated relative paths.
fn collect_profile_files(root: &Path, dir: &Path, files: &mut Vec<ProfileFile>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_profile_files(root, &path, files)?;
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if let Ok(data) = std::fs::read(&path) {
            files.push(ProfileFile { path: rel, data });
        }
    }
    Ok(())
}

#[cfg(feature = "sqlite")]
mod sqlite_blobs {
    use super::*;
    use crate::profileblob::ProfileBlobStore;

    #[async_trait]
    impl ProfileBlobStore for SqliteStore {
        fn name(&self) -> &'static str {
            "sqlite"
        }

        async fn save_profile(&self, owner: &str, files: &[ProfileFile]) -> Result<()> {
            self.with_conn("delete profile files", |conn| {
                conn.execute(
                    "DELETE FROM profile_files WHERE owner = ?1",
                    rusqlite::params![owner],
                )
                .map(|_| ())
            })?;
            for file in files {
                self.with_conn("insert profile file", |conn| {
                    conn.execute(
                        "INSERT INTO profile_files (owner, path, data) VALUES (?1, ?2, ?3)",
                        rusqlite::params![owner, file.path, file.data],
                    )
                    .map(|_| ())
                })?;
            }
            Ok(())
        }

        async fn load_profile(&self, owner: &str) -> Result<Vec<ProfileFile>> {
            self.with_conn("load profile files", |conn| {
                let mut statement = conn.prepare(
                    "SELECT path, data FROM profile_files WHERE owner = ?1 ORDER BY path",
                )?;
                let rows = statement.query_map(rusqlite::params![owner], |row| {
                    Ok(ProfileFile {
                        path: row.get(0)?,
                        data: row.get(1)?,
                    })
                })?;
                rows.collect()
            })
        }

        async fn delete_profile(&self, owner: &str) -> Result<bool> {
            self.with_conn("delete profile files", |conn| {
                conn.execute(
                    "DELETE FROM profile_files WHERE owner = ?1",
                    rusqlite::params![owner],
                )
                .map(|deleted| deleted > 0)
            })
        }
    }
}

#[cfg(feature = "mysql")]
mod mysql_blobs {
    use super::*;
    use crate::profileblob::ProfileBlobStore;

    #[async_trait]
    impl ProfileBlobStore for MySqlStore {
        fn name(&self) -> &'static str {
            "mysql"
        }

        async fn save_profile(&self, owner: &str, files: &[ProfileFile]) -> Result<()> {
            self.ensure_schema().await?;
            sqlx::query("DELETE FROM profile_files WHERE owner = ?")
                .bind(owner)
                .execute(&self.pool)
                .await
                .map_err(|e| storage_io("delete profile files (mysql)", e))?;
            for file in files {
                sqlx::query("INSERT INTO profile_files (owner, path, data) VALUES (?, ?, ?)")
                    .bind(owner)
                    .bind(&file.path)
                    .bind(&file.data)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| storage_io("insert profile file (mysql)", e))?;
            }
            Ok(())
        }

        async fn load_profile(&self, owner: &str) -> Result<Vec<ProfileFile>> {
            self.ensure_schema().await?;
            let rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
                "SELECT path, data FROM profile_files WHERE owner = ? ORDER BY path",
            )
            .bind(owner)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| storage_io("load profile files (mysql)", e))?;
            Ok(rows
                .into_iter()
                .map(|(path, data)| ProfileFile { path, data })
                .collect())
        }

        async fn delete_profile(&self, owner: &str) -> Result<bool> {
            self.ensure_schema().await?;
            let result = sqlx::query("DELETE FROM profile_files WHERE owner = ?")
                .bind(owner)
                .execute(&self.pool)
                .await
                .map_err(|e| storage_io("delete profile files (mysql)", e))?;
            Ok(result.rows_affected() > 0)
        }
    }
}
