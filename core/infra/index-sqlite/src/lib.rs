//! Адаптер `IndexStore`: SQLite у WAL-режимі.
//!
//! Реалізація: T-010 (підключення), T-011 (схема v1), T-012 (міграції),
//! T-013 (батчевий запис), T-014 (віконні запити), T-029 (курсор USN),
//! T-078 (журнал Quarantine).

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};
use trashradar_domain::{
    candidate::{
        ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
        FileRecordSort, FsTimestamp, SafetyLevel,
    },
    category::CategoryId,
    duplicates::{normalize_hash_cache_path, ContentHash, FileHashCacheEntry, PartialHash},
    scan::UsnCursor,
};

const DATABASE_FILE_NAME: &str = "index.sqlite3";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SCHEMA_VERSION_V1: i64 = 1;
const SCHEMA_VERSION_V2: i64 = 2;
const SCHEMA_VERSION_V3: i64 = 3;
const SCHEMA_VERSION_V4: i64 = 4;
const SCHEMA_VERSION_V5: i64 = 5;
const LATEST_SCHEMA_VERSION: i64 = SCHEMA_VERSION_V5;
const WRITER_QUEUE_CHANNEL_CLOSED: &str = "sqlite index writer queue is closed";

/// Migration v1 creates the persistent file-record layer used by scanner output and
/// MVP detectors. The migration runner executes this script for databases created
/// by T-010; later tasks add the batched writer (T-013) and paged read API (T-014).
const SCHEMA_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS __trashradar_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS file_records (
    candidate_id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    created_at_filetime INTEGER,
    modified_at_filetime INTEGER,
    accessed_at_filetime INTEGER,
    file_kind TEXT NOT NULL CHECK (
        file_kind IN ('video', 'image', 'audio', 'archive', 'installer', 'disk_image', 'document', 'other')
    ),
    candidate_unit TEXT NOT NULL CHECK (candidate_unit IN ('file', 'folder')),
    category TEXT NOT NULL CHECK (
        category IN (
            'large_files',
            'old_files',
            'forgotten_videos',
            'duplicates',
            'archives',
            'installers',
            'temp_files',
            'app_caches',
            'dev_artifacts'
        )
    ),
    safety TEXT NOT NULL CHECK (safety IN ('safe_to_bulk', 'review_recommended')),
    decision TEXT NOT NULL DEFAULT 'undecided' CHECK (decision IN ('undecided', 'keep', 'marked')),
    detector_id TEXT NOT NULL,
    explanation TEXT NOT NULL DEFAULT '',
    attributes INTEGER NOT NULL DEFAULT 0 CHECK (attributes >= 0),
    is_readonly INTEGER NOT NULL DEFAULT 0 CHECK (is_readonly IN (0, 1)),
    is_hidden INTEGER NOT NULL DEFAULT 0 CHECK (is_hidden IN (0, 1)),
    is_system INTEGER NOT NULL DEFAULT 0 CHECK (is_system IN (0, 1)),
    is_temporary INTEGER NOT NULL DEFAULT 0 CHECK (is_temporary IN (0, 1)),
    UNIQUE (category, path)
);

CREATE INDEX IF NOT EXISTS idx_file_records_category_size
    ON file_records (category, size_bytes DESC, candidate_id);
CREATE INDEX IF NOT EXISTS idx_file_records_category_accessed
    ON file_records (category, accessed_at_filetime, candidate_id);
CREATE INDEX IF NOT EXISTS idx_file_records_kind
    ON file_records (file_kind, candidate_id);

PRAGMA user_version = 1;
"#;

/// T-029: позиція USN Journal на том (architecture.md §2.3 / §10).
const SCHEMA_V2_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS volume_usn_state (
    volume TEXT PRIMARY KEY NOT NULL,
    journal_id INTEGER NOT NULL,
    next_usn INTEGER NOT NULL,
    updated_at_unix INTEGER NOT NULL
);

PRAGMA user_version = 2;
"#;

/// T-062: кеш partial/full хешів (валідність size + mtime).
const SCHEMA_V3_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS file_hash_cache (
    path_key TEXT PRIMARY KEY NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    modified_at_filetime INTEGER NOT NULL DEFAULT 0,
    partial_hash BLOB,
    content_hash BLOB,
    CHECK (partial_hash IS NULL OR length(partial_hash) = 32),
    CHECK (content_hash IS NULL OR length(content_hash) = 32)
);

PRAGMA user_version = 3;
"#;

/// T-068: кеш preview_cache (посилання на прев'ю, валідність size + mtime).
const SCHEMA_V4_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS preview_cache (
    source_path TEXT NOT NULL,
    preview_kind TEXT NOT NULL CHECK (preview_kind IN ('thumbnail', 'scrub')),
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    modified_at_filetime INTEGER NOT NULL DEFAULT 0,
    preview_path TEXT NOT NULL,
    PRIMARY KEY (source_path, preview_kind)
);

PRAGMA user_version = 4;
"#;

/// T-073: вид превью `large` (повнорозмірне для великого показу).
/// SQLite не вміє розширити CHECK — таблиця перестворюється; втрата
/// кешу превью — не помилка, а холодний старт (architecture.md §10).
const SCHEMA_V5_SQL: &str = r#"
DROP TABLE IF EXISTS preview_cache;
CREATE TABLE preview_cache (
    source_path TEXT NOT NULL,
    preview_kind TEXT NOT NULL CHECK (preview_kind IN ('thumbnail', 'scrub', 'large')),
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    modified_at_filetime INTEGER NOT NULL DEFAULT 0,
    preview_path TEXT NOT NULL,
    PRIMARY KEY (source_path, preview_kind)
);

PRAGMA user_version = 5;
"#;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: SCHEMA_VERSION_V1,
        sql: SCHEMA_V1_SQL,
    },
    Migration {
        version: SCHEMA_VERSION_V2,
        sql: SCHEMA_V2_SQL,
    },
    Migration {
        version: SCHEMA_VERSION_V3,
        sql: SCHEMA_V3_SQL,
    },
    Migration {
        version: SCHEMA_VERSION_V4,
        sql: SCHEMA_V4_SQL,
    },
    Migration {
        version: SCHEMA_VERSION_V5,
        sql: SCHEMA_V5_SQL,
    },
];

const UPSERT_FILE_RECORD_SQL: &str = "INSERT INTO file_records (
        candidate_id,
        path,
        size_bytes,
        created_at_filetime,
        modified_at_filetime,
        accessed_at_filetime,
        file_kind,
        candidate_unit,
        category,
        safety,
        decision,
        detector_id,
        explanation,
        attributes,
        is_readonly,
        is_hidden,
        is_system,
        is_temporary
    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
    ON CONFLICT(candidate_id) DO UPDATE SET
        path = excluded.path,
        size_bytes = excluded.size_bytes,
        created_at_filetime = excluded.created_at_filetime,
        modified_at_filetime = excluded.modified_at_filetime,
        accessed_at_filetime = excluded.accessed_at_filetime,
        file_kind = excluded.file_kind,
        candidate_unit = excluded.candidate_unit,
        category = excluded.category,
        safety = excluded.safety,
        decision = excluded.decision,
        detector_id = excluded.detector_id,
        explanation = excluded.explanation,
        attributes = excluded.attributes,
        is_readonly = excluded.is_readonly,
        is_hidden = excluded.is_hidden,
        is_system = excluded.is_system,
        is_temporary = excluded.is_temporary";

pub type Result<T> = std::result::Result<T, IndexSqliteError>;

#[derive(Debug)]
pub enum IndexSqliteError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    UnsupportedSchemaVersion(i64),
    WriterQueueClosed,
    WriterThreadPanicked,
    SchemaMigrationFailed {
        from: i64,
        to: i64,
        source: rusqlite::Error,
    },
    InvalidUnsignedInteger {
        field: &'static str,
        value: u64,
    },
    InvalidEnumValue {
        field: &'static str,
        value: String,
    },
}

impl fmt::Display for IndexSqliteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "sqlite index I/O error: {error}"),
            Self::Sqlite(error) => write!(f, "sqlite index error: {error}"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(f, "unsupported sqlite index schema version: {version}")
            }
            Self::WriterQueueClosed => write!(f, "{WRITER_QUEUE_CHANNEL_CLOSED}"),
            Self::WriterThreadPanicked => write!(f, "sqlite index writer thread panicked"),
            Self::SchemaMigrationFailed { from, to, source } => {
                write!(
                    f,
                    "failed to migrate sqlite index schema from {from} to {to}: {source}"
                )
            }
            Self::InvalidUnsignedInteger { field, value } => {
                write!(
                    f,
                    "value {value} for {field} does not fit into SQLite INTEGER"
                )
            }
            Self::InvalidEnumValue { field, value } => {
                write!(f, "invalid value {value:?} for {field}")
            }
        }
    }
}

impl Error for IndexSqliteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::SchemaMigrationFailed { source, .. } => Some(source),
            Self::UnsupportedSchemaVersion(_)
            | Self::WriterQueueClosed
            | Self::WriterThreadPanicked
            | Self::InvalidUnsignedInteger { .. }
            | Self::InvalidEnumValue { .. } => None,
        }
    }
}

impl From<std::io::Error> for IndexSqliteError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for IndexSqliteError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Debug)]
pub struct IndexDatabase {
    path: PathBuf,
    connection: Connection,
}

#[derive(Debug, Clone, Copy)]
struct Migration {
    version: i64,
    sql: &'static str,
}

// FileAttributes and FileRecord are imported from trashradar_domain::candidate

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchWriteReport {
    pub records_written: usize,
}

#[derive(Debug)]
pub struct IndexWriterQueue {
    sender: mpsc::Sender<WriterCommand>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone)]
pub struct IndexWriter {
    sender: mpsc::Sender<WriterCommand>,
}

#[derive(Debug)]
enum WriterCommand {
    UpsertFileRecords {
        records: Vec<FileRecord>,
        respond_to: mpsc::Sender<Result<BatchWriteReport>>,
    },
    Shutdown,
}

impl IndexDatabase {
    pub fn open_profile(profile_dir: impl AsRef<Path>) -> Result<Self> {
        Self::open(profile_dir.as_ref().join(DATABASE_FILE_NAME))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut connection = open_configured_connection(&path)?;
        if let Err(error) = ensure_schema(&mut connection) {
            if !requires_cold_start_reset(&error) {
                return Err(error);
            }

            drop(connection);
            reset_database_files(&path)?;
            connection = open_configured_connection(&path)?;
            ensure_schema(&mut connection)?;
        }

        Ok(Self { path, connection })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn journal_mode(&self) -> Result<String> {
        self.connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn foreign_keys_enabled(&self) -> Result<bool> {
        let enabled: i64 = self
            .connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        Ok(enabled != 0)
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn read_meta(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT value FROM __trashradar_meta WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn write_meta(&mut self, key: &str, value: &str) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO __trashradar_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value),
        )?;
        transaction.commit()?;

        Ok(())
    }

    pub fn upsert_file_record(&mut self, record: &FileRecord) -> Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut statement = transaction.prepare_cached(UPSERT_FILE_RECORD_SQL)?;
            execute_upsert_file_record(&mut statement, record)?;
        }
        transaction.commit()?;

        Ok(())
    }

    pub fn upsert_file_records_batch(
        &mut self,
        records: &[FileRecord],
    ) -> Result<BatchWriteReport> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut statement = transaction.prepare_cached(UPSERT_FILE_RECORD_SQL)?;
            for record in records {
                execute_upsert_file_record(&mut statement, record)?;
            }
        }
        transaction.commit()?;

        Ok(BatchWriteReport {
            records_written: records.len(),
        })
    }

    pub fn read_file_record(&self, candidate_id: CandidateId) -> Result<Option<FileRecord>> {
        let candidate_id = sqlite_integer("candidate_id", candidate_id.0)?;
        let row = self
            .connection
            .query_row(
                "SELECT
                    candidate_id,
                    path,
                    size_bytes,
                    created_at_filetime,
                    modified_at_filetime,
                    accessed_at_filetime,
                    file_kind,
                    candidate_unit,
                    category,
                    safety,
                    decision,
                    detector_id,
                    explanation,
                    attributes,
                    is_readonly,
                    is_hidden,
                    is_system,
                    is_temporary
                FROM file_records
                WHERE candidate_id = ?1",
                [candidate_id],
                |row| {
                    Ok(StoredFileRecord {
                        candidate_id: row.get(0)?,
                        path: row.get(1)?,
                        size_bytes: row.get(2)?,
                        created_at: row.get(3)?,
                        modified_at: row.get(4)?,
                        accessed_at: row.get(5)?,
                        file_kind: row.get(6)?,
                        unit: row.get(7)?,
                        category: row.get(8)?,
                        safety: row.get(9)?,
                        decision: row.get(10)?,
                        detector_id: row.get(11)?,
                        explanation: row.get(12)?,
                        attributes: row.get(13)?,
                        is_readonly: row.get(14)?,
                        is_hidden: row.get(15)?,
                        is_system: row.get(16)?,
                        is_temporary: row.get(17)?,
                    })
                },
            )
            .optional()?;

        row.map(FileRecord::try_from).transpose()
    }

    pub fn read_file_records_window(
        &self,
        category: CategoryId,
        sort: FileRecordSort,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<FileRecord>> {
        let category_str = category_name(category);
        let limit_val = sqlite_integer("limit", limit)?;
        let offset_val = sqlite_integer("offset", offset)?;

        let sql = match sort {
            FileRecordSort::SizeDesc => {
                "SELECT
                    f.candidate_id,
                    f.path,
                    f.size_bytes,
                    f.created_at_filetime,
                    f.modified_at_filetime,
                    f.accessed_at_filetime,
                    f.file_kind,
                    f.candidate_unit,
                    f.category,
                    f.safety,
                    f.decision,
                    f.detector_id,
                    f.explanation,
                    f.attributes,
                    f.is_readonly,
                    f.is_hidden,
                    f.is_system,
                    f.is_temporary
                FROM file_records f
                JOIN (
                    SELECT candidate_id
                    FROM file_records
                    WHERE category = ?1
                    ORDER BY size_bytes DESC, candidate_id ASC
                    LIMIT ?2 OFFSET ?3
                ) sub ON f.candidate_id = sub.candidate_id
                ORDER BY f.size_bytes DESC, f.candidate_id ASC"
            }
            FileRecordSort::AccessedAsc => {
                "SELECT
                    f.candidate_id,
                    f.path,
                    f.size_bytes,
                    f.created_at_filetime,
                    f.modified_at_filetime,
                    f.accessed_at_filetime,
                    f.file_kind,
                    f.candidate_unit,
                    f.category,
                    f.safety,
                    f.decision,
                    f.detector_id,
                    f.explanation,
                    f.attributes,
                    f.is_readonly,
                    f.is_hidden,
                    f.is_system,
                    f.is_temporary
                FROM file_records f
                JOIN (
                    SELECT candidate_id
                    FROM file_records
                    WHERE category = ?1
                    ORDER BY accessed_at_filetime ASC, candidate_id ASC
                    LIMIT ?2 OFFSET ?3
                ) sub ON f.candidate_id = sub.candidate_id
                ORDER BY f.accessed_at_filetime ASC, f.candidate_id ASC"
            }
            FileRecordSort::AccessedDesc => {
                "SELECT
                    f.candidate_id,
                    f.path,
                    f.size_bytes,
                    f.created_at_filetime,
                    f.modified_at_filetime,
                    f.accessed_at_filetime,
                    f.file_kind,
                    f.candidate_unit,
                    f.category,
                    f.safety,
                    f.decision,
                    f.detector_id,
                    f.explanation,
                    f.attributes,
                    f.is_readonly,
                    f.is_hidden,
                    f.is_system,
                    f.is_temporary
                FROM file_records f
                JOIN (
                    SELECT candidate_id
                    FROM file_records
                    WHERE category = ?1
                    ORDER BY accessed_at_filetime DESC, candidate_id DESC
                    LIMIT ?2 OFFSET ?3
                ) sub ON f.candidate_id = sub.candidate_id
                ORDER BY f.accessed_at_filetime DESC, f.candidate_id DESC"
            }
        };

        let mut statement = self.connection.prepare_cached(sql)?;
        let rows = statement.query_map(params![category_str, limit_val, offset_val], |row| {
            Ok(StoredFileRecord {
                candidate_id: row.get(0)?,
                path: row.get(1)?,
                size_bytes: row.get(2)?,
                created_at: row.get(3)?,
                modified_at: row.get(4)?,
                accessed_at: row.get(5)?,
                file_kind: row.get(6)?,
                unit: row.get(7)?,
                category: row.get(8)?,
                safety: row.get(9)?,
                decision: row.get(10)?,
                detector_id: row.get(11)?,
                explanation: row.get(12)?,
                attributes: row.get(13)?,
                is_readonly: row.get(14)?,
                is_hidden: row.get(15)?,
                is_system: row.get(16)?,
                is_temporary: row.get(17)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            let stored = row?;
            records.push(FileRecord::try_from(stored)?);
        }

        Ok(records)
    }

    /// Збережений курсор USN для тому (літера, регістр нормалізується).
    pub fn get_usn_cursor(&self, volume: char) -> Result<Option<UsnCursor>> {
        let key = volume_key(volume)?;
        let row: Option<(i64, i64)> = self
            .connection
            .query_row(
                "SELECT journal_id, next_usn FROM volume_usn_state WHERE volume = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(journal_id, next_usn)| UsnCursor {
            journal_id: journal_id as u64,
            next_usn,
        }))
    }

    /// Зафіксувати позицію USN Journal після скану / дельти (T-029).
    pub fn set_usn_cursor(&self, volume: char, cursor: UsnCursor) -> Result<()> {
        let key = volume_key(volume)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.connection.execute(
            "INSERT INTO volume_usn_state (volume, journal_id, next_usn, updated_at_unix)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(volume) DO UPDATE SET
                journal_id = excluded.journal_id,
                next_usn = excluded.next_usn,
                updated_at_unix = excluded.updated_at_unix",
            params![key, cursor.journal_id as i64, cursor.next_usn, now],
        )?;
        Ok(())
    }

    /// Видалити всі записи з даним шляхом (T-030; шлях порівнюється exact COLLATE NOCASE).
    pub fn delete_file_records_by_path(&self, path: &str) -> Result<u64> {
        let changed = self.connection.execute(
            "DELETE FROM file_records WHERE path = ?1 COLLATE NOCASE",
            params![path],
        )?;
        Ok(changed as u64)
    }

    // --- T-062: file_hash_cache ------------------------------------------------

    /// Прочитати запис кешу хешів (без перевірки size+mtime — це app-шар).
    pub fn get_hash_cache_entry(&self, path: &str) -> Result<Option<FileHashCacheEntry>> {
        let key = normalize_hash_cache_path(path);
        type HashCacheRow = (i64, i64, Option<Vec<u8>>, Option<Vec<u8>>);
        let row: Option<HashCacheRow> = self
            .connection
            .query_row(
                "SELECT size_bytes, modified_at_filetime, partial_hash, content_hash
                 FROM file_hash_cache WHERE path_key = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        Ok(row.map(|(size, mtime, partial, content)| {
            let modified_at = if mtime == 0 {
                None
            } else {
                Some(FsTimestamp(mtime))
            };
            FileHashCacheEntry {
                path_key: key,
                size: ByteSize(size as u64),
                modified_at,
                partial: blob_to_partial(partial),
                content: blob_to_content(content),
            }
        }))
    }

    /// Upsert кешу хешів (merge: NULL blob не затирає існуючий).
    pub fn put_hash_cache_entry(&self, entry: &FileHashCacheEntry) -> Result<()> {
        let mtime = entry.modified_at.map(|t| t.0).unwrap_or(0);
        let partial = entry.partial.map(|h| h.0.to_vec());
        let content = entry.content.map(|h| h.0.to_vec());
        self.connection.execute(
            "INSERT INTO file_hash_cache (path_key, size_bytes, modified_at_filetime, partial_hash, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path_key) DO UPDATE SET
                size_bytes = excluded.size_bytes,
                modified_at_filetime = excluded.modified_at_filetime,
                partial_hash = COALESCE(excluded.partial_hash, file_hash_cache.partial_hash),
                content_hash = COALESCE(excluded.content_hash, file_hash_cache.content_hash)",
            params![
                entry.path_key,
                entry.size.0 as i64,
                mtime,
                partial,
                content
            ],
        )?;
        Ok(())
    }

    /// Скинути USN-курсор тому (T-031).
    pub fn clear_usn_cursor(&self, volume: char) -> Result<()> {
        let key = volume_key(volume)?;
        self.connection.execute(
            "DELETE FROM volume_usn_state WHERE volume = ?1",
            params![key],
        )?;
        Ok(())
    }

    // --- T-068: preview_cache ------------------------------------------------

    pub fn get_preview_cache_entry(
        &self,
        source_path: &str,
        kind: trashradar_app::ports::PreviewKind,
    ) -> Result<Option<trashradar_app::ports::PreviewCacheEntry>> {
        let kind_str = preview_kind_name(kind);
        type PreviewCacheRow = (i64, i64, String);
        let row: Option<PreviewCacheRow> = self
            .connection
            .query_row(
                "SELECT size_bytes, modified_at_filetime, preview_path
                 FROM preview_cache
                 WHERE source_path = ?1 AND preview_kind = ?2",
                params![source_path, kind_str],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        match row {
            Some((size, mtime, preview_path)) => {
                let modified_at = if mtime == 0 {
                    None
                } else {
                    Some(FsTimestamp(mtime))
                };
                Ok(Some(trashradar_app::ports::PreviewCacheEntry {
                    source_path: source_path.to_string(),
                    preview_kind: kind,
                    size: ByteSize(size as u64),
                    modified_at,
                    preview_path,
                }))
            }
            None => Ok(None),
        }
    }

    pub fn put_preview_cache_entry(
        &self,
        entry: &trashradar_app::ports::PreviewCacheEntry,
    ) -> Result<()> {
        let kind_str = preview_kind_name(entry.preview_kind);
        let mtime = entry.modified_at.map(|t| t.0).unwrap_or(0);
        self.connection.execute(
            "INSERT INTO preview_cache (source_path, preview_kind, size_bytes, modified_at_filetime, preview_path)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source_path, preview_kind) DO UPDATE SET
                size_bytes = excluded.size_bytes,
                modified_at_filetime = excluded.modified_at_filetime,
                preview_path = excluded.preview_path",
            params![
                entry.source_path,
                kind_str,
                entry.size.0 as i64,
                mtime,
                entry.preview_path
            ],
        )?;
        Ok(())
    }

    pub fn delete_preview_cache_entry(
        &self,
        source_path: &str,
        kind: trashradar_app::ports::PreviewKind,
    ) -> Result<()> {
        let kind_str = preview_kind_name(kind);
        self.connection.execute(
            "DELETE FROM preview_cache WHERE source_path = ?1 AND preview_kind = ?2",
            params![source_path, kind_str],
        )?;
        Ok(())
    }

    pub fn delete_all_preview_cache_entries_for_path(&self, source_path: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM preview_cache WHERE source_path = ?1",
            params![source_path],
        )?;
        Ok(())
    }

    pub fn read_all_file_records(&self) -> Result<Vec<FileRecord>> {
        let sql = "SELECT
            candidate_id,
            path,
            size_bytes,
            created_at_filetime,
            modified_at_filetime,
            accessed_at_filetime,
            file_kind,
            candidate_unit,
            category,
            safety,
            decision,
            detector_id,
            explanation,
            attributes,
            is_readonly,
            is_hidden,
            is_system,
            is_temporary
        FROM file_records
        ORDER BY candidate_id ASC";

        let mut statement = self.connection.prepare_cached(sql)?;
        let rows = statement.query_map([], |row| {
            Ok(StoredFileRecord {
                candidate_id: row.get(0)?,
                path: row.get(1)?,
                size_bytes: row.get(2)?,
                created_at: row.get(3)?,
                modified_at: row.get(4)?,
                accessed_at: row.get(5)?,
                file_kind: row.get(6)?,
                unit: row.get(7)?,
                category: row.get(8)?,
                safety: row.get(9)?,
                decision: row.get(10)?,
                detector_id: row.get(11)?,
                explanation: row.get(12)?,
                attributes: row.get(13)?,
                is_readonly: row.get(14)?,
                is_hidden: row.get(15)?,
                is_system: row.get(16)?,
                is_temporary: row.get(17)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            let stored = row?;
            records.push(FileRecord::try_from(stored)?);
        }

        Ok(records)
    }
}

impl trashradar_app::ports::IndexStore for IndexDatabase {
    fn read_file_records_window(
        &self,
        category: CategoryId,
        sort: FileRecordSort,
        offset: u64,
        limit: u64,
    ) -> std::result::Result<Vec<FileRecord>, trashradar_domain::error::CoreError> {
        self.read_file_records_window(category, sort, offset, limit)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }

    fn read_all_file_records(
        &self,
    ) -> std::result::Result<Vec<FileRecord>, trashradar_domain::error::CoreError> {
        self.read_all_file_records()
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }

    fn get_usn_cursor(
        &self,
        volume: char,
    ) -> std::result::Result<Option<UsnCursor>, trashradar_domain::error::CoreError> {
        self.get_usn_cursor(volume)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }

    fn set_usn_cursor(
        &self,
        volume: char,
        cursor: UsnCursor,
    ) -> std::result::Result<(), trashradar_domain::error::CoreError> {
        self.set_usn_cursor(volume, cursor)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }

    fn delete_file_records_by_path(
        &self,
        path: &str,
    ) -> std::result::Result<u64, trashradar_domain::error::CoreError> {
        self.delete_file_records_by_path(path)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }

    fn clear_usn_cursor(
        &self,
        volume: char,
    ) -> std::result::Result<(), trashradar_domain::error::CoreError> {
        self.clear_usn_cursor(volume)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }
}

/// Thread-safe обгортка `IndexDatabase` для [`HashCache`] (rusqlite Connection !Sync).
///
/// Один writer-lock на get/put — достатньо для T-062; ліміт I/O на том — T-063.
pub struct ThreadSafeHashCache {
    db: std::sync::Mutex<IndexDatabase>,
}

impl ThreadSafeHashCache {
    pub fn new(db: IndexDatabase) -> Self {
        Self {
            db: std::sync::Mutex::new(db),
        }
    }

    pub fn open_profile(profile_dir: impl AsRef<std::path::Path>) -> Result<Self> {
        Ok(Self::new(IndexDatabase::open_profile(profile_dir)?))
    }
}

impl trashradar_app::ports::HashCache for ThreadSafeHashCache {
    fn get_entry(
        &self,
        path: &str,
    ) -> std::result::Result<Option<FileHashCacheEntry>, trashradar_domain::error::CoreError> {
        self.db
            .lock()
            .map_err(|_| {
                trashradar_domain::error::CoreError::internal("hash cache mutex poisoned")
            })?
            .get_hash_cache_entry(path)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }

    fn put_entry(
        &self,
        entry: &FileHashCacheEntry,
    ) -> std::result::Result<(), trashradar_domain::error::CoreError> {
        self.db
            .lock()
            .map_err(|_| {
                trashradar_domain::error::CoreError::internal("hash cache mutex poisoned")
            })?
            .put_hash_cache_entry(entry)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }
}

/// Thread-safe обгортка `IndexDatabase` для [`PreviewCache`] (rusqlite Connection !Sync).
pub struct ThreadSafePreviewCache {
    db: std::sync::Mutex<IndexDatabase>,
}

impl ThreadSafePreviewCache {
    pub fn new(db: IndexDatabase) -> Self {
        Self {
            db: std::sync::Mutex::new(db),
        }
    }

    pub fn open_profile(profile_dir: impl AsRef<std::path::Path>) -> Result<Self> {
        Ok(Self::new(IndexDatabase::open_profile(profile_dir)?))
    }
}

impl trashradar_app::ports::PreviewCache for ThreadSafePreviewCache {
    fn get_preview_entry(
        &self,
        source_path: &str,
        kind: trashradar_app::ports::PreviewKind,
    ) -> std::result::Result<
        Option<trashradar_app::ports::PreviewCacheEntry>,
        trashradar_domain::error::CoreError,
    > {
        self.db
            .lock()
            .map_err(|_| {
                trashradar_domain::error::CoreError::internal("preview cache mutex poisoned")
            })?
            .get_preview_cache_entry(source_path, kind)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }

    fn put_preview_entry(
        &self,
        entry: &trashradar_app::ports::PreviewCacheEntry,
    ) -> std::result::Result<(), trashradar_domain::error::CoreError> {
        self.db
            .lock()
            .map_err(|_| {
                trashradar_domain::error::CoreError::internal("preview cache mutex poisoned")
            })?
            .put_preview_cache_entry(entry)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }

    fn delete_preview_entry(
        &self,
        source_path: &str,
        kind: trashradar_app::ports::PreviewKind,
    ) -> std::result::Result<(), trashradar_domain::error::CoreError> {
        self.db
            .lock()
            .map_err(|_| {
                trashradar_domain::error::CoreError::internal("preview cache mutex poisoned")
            })?
            .delete_preview_cache_entry(source_path, kind)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }

    fn delete_all_previews_for_path(
        &self,
        source_path: &str,
    ) -> std::result::Result<(), trashradar_domain::error::CoreError> {
        self.db
            .lock()
            .map_err(|_| {
                trashradar_domain::error::CoreError::internal("preview cache mutex poisoned")
            })?
            .delete_all_preview_cache_entries_for_path(source_path)
            .map_err(|err| trashradar_domain::error::CoreError::internal(err.to_string()))
    }
}

fn preview_kind_name(kind: trashradar_app::ports::PreviewKind) -> &'static str {
    match kind {
        trashradar_app::ports::PreviewKind::Thumbnail => "thumbnail",
        trashradar_app::ports::PreviewKind::Scrub => "scrub",
        trashradar_app::ports::PreviewKind::Large => "large",
    }
}

fn volume_key(volume: char) -> Result<String> {
    if !volume.is_ascii_alphabetic() {
        return Err(IndexSqliteError::InvalidEnumValue {
            field: "volume",
            value: volume.to_string(),
        });
    }
    Ok(volume.to_ascii_uppercase().to_string())
}

fn blob_to_partial(blob: Option<Vec<u8>>) -> Option<PartialHash> {
    blob.and_then(|b| {
        if b.len() == 32 {
            let mut a = [0u8; 32];
            a.copy_from_slice(&b);
            Some(PartialHash(a))
        } else {
            None
        }
    })
}

fn blob_to_content(blob: Option<Vec<u8>>) -> Option<ContentHash> {
    blob.and_then(|b| {
        if b.len() == 32 {
            let mut a = [0u8; 32];
            a.copy_from_slice(&b);
            Some(ContentHash(a))
        } else {
            None
        }
    })
}

impl IndexWriterQueue {
    pub fn open_profile(profile_dir: impl AsRef<Path>) -> Result<Self> {
        Self::open(profile_dir.as_ref().join(DATABASE_FILE_NAME))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let (sender, receiver) = mpsc::channel();
        let (startup_sender, startup_receiver) = mpsc::channel();

        let thread = thread::spawn(move || {
            let mut database = match IndexDatabase::open(path) {
                Ok(database) => {
                    let _ = startup_sender.send(Ok(()));
                    database
                }
                Err(error) => {
                    let _ = startup_sender.send(Err(error));
                    return;
                }
            };

            writer_loop(&mut database, receiver);
        });

        match startup_receiver
            .recv()
            .map_err(|_| IndexSqliteError::WriterQueueClosed)?
        {
            Ok(()) => Ok(Self {
                sender,
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }

    pub fn handle(&self) -> IndexWriter {
        IndexWriter {
            sender: self.sender.clone(),
        }
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> Result<()> {
        if let Some(thread) = self.thread.take() {
            let _ = self.sender.send(WriterCommand::Shutdown);
            thread
                .join()
                .map_err(|_| IndexSqliteError::WriterThreadPanicked)?;
        }

        Ok(())
    }
}

impl Drop for IndexWriterQueue {
    fn drop(&mut self) {
        let _ = self.shutdown_inner();
    }
}

impl IndexWriter {
    pub fn upsert_file_records(&self, records: Vec<FileRecord>) -> Result<BatchWriteReport> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(WriterCommand::UpsertFileRecords {
                records,
                respond_to,
            })
            .map_err(|_| IndexSqliteError::WriterQueueClosed)?;

        response
            .recv()
            .map_err(|_| IndexSqliteError::WriterQueueClosed)?
    }
}

fn writer_loop(database: &mut IndexDatabase, receiver: mpsc::Receiver<WriterCommand>) {
    while let Ok(command) = receiver.recv() {
        match command {
            WriterCommand::UpsertFileRecords {
                records,
                respond_to,
            } => {
                let result = database.upsert_file_records_batch(&records);
                let _ = respond_to.send(result);
            }
            WriterCommand::Shutdown => break,
        }
    }
}

fn execute_upsert_file_record(
    statement: &mut rusqlite::CachedStatement<'_>,
    record: &FileRecord,
) -> Result<()> {
    let candidate_id = sqlite_integer("candidate_id", record.candidate_id.0)?;
    let size_bytes = sqlite_integer("size_bytes", record.size.0)?;
    let attributes = sqlite_integer("attributes", u64::from(record.attributes.raw_bits))?;

    statement.execute(params![
        candidate_id,
        record.path.as_str(),
        size_bytes,
        record.created_at.map(|timestamp| timestamp.0),
        record.modified_at.map(|timestamp| timestamp.0),
        record.accessed_at.map(|timestamp| timestamp.0),
        file_kind_name(record.kind),
        candidate_unit_name(record.unit),
        category_name(record.category),
        safety_level_name(record.safety),
        decision_name(record.decision),
        record.detector_id.as_str(),
        record.explanation.as_str(),
        attributes,
        bool_integer(record.attributes.is_readonly),
        bool_integer(record.attributes.is_hidden),
        bool_integer(record.attributes.is_system),
        bool_integer(record.attributes.is_temporary),
    ])?;

    Ok(())
}

fn open_configured_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "cache_size", -262_144)?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "wal_autocheckpoint", 10_000)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;

    Ok(())
}

fn ensure_schema(connection: &mut Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if version > LATEST_SCHEMA_VERSION {
        return Err(IndexSqliteError::UnsupportedSchemaVersion(version));
    }

    let mut current_version = version;
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version > version)
    {
        run_migration(connection, current_version, migration)?;
        current_version = migration.version;
    }

    Ok(())
}

fn run_migration(connection: &mut Connection, from: i64, migration: &Migration) -> Result<()> {
    let transaction =
        connection
            .transaction()
            .map_err(|source| IndexSqliteError::SchemaMigrationFailed {
                from,
                to: migration.version,
                source,
            })?;

    transaction.execute_batch(migration.sql).map_err(|source| {
        IndexSqliteError::SchemaMigrationFailed {
            from,
            to: migration.version,
            source,
        }
    })?;

    transaction
        .commit()
        .map_err(|source| IndexSqliteError::SchemaMigrationFailed {
            from,
            to: migration.version,
            source,
        })?;

    Ok(())
}

fn requires_cold_start_reset(error: &IndexSqliteError) -> bool {
    matches!(error, IndexSqliteError::SchemaMigrationFailed { .. })
}

fn reset_database_files(path: &Path) -> Result<()> {
    for file_path in database_files(path) {
        match fs::remove_file(&file_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

fn database_files(path: &Path) -> [PathBuf; 3] {
    [
        path.to_path_buf(),
        path_with_suffix(path, "-wal"),
        path_with_suffix(path, "-shm"),
    ]
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[derive(Debug)]
struct StoredFileRecord {
    candidate_id: i64,
    path: String,
    size_bytes: i64,
    created_at: Option<i64>,
    modified_at: Option<i64>,
    accessed_at: Option<i64>,
    file_kind: String,
    unit: String,
    category: String,
    safety: String,
    decision: String,
    detector_id: String,
    explanation: String,
    attributes: i64,
    is_readonly: i64,
    is_hidden: i64,
    is_system: i64,
    is_temporary: i64,
}

impl TryFrom<StoredFileRecord> for FileRecord {
    type Error = IndexSqliteError;

    fn try_from(record: StoredFileRecord) -> Result<Self> {
        Ok(Self {
            candidate_id: CandidateId(unsigned_integer("candidate_id", record.candidate_id)?),
            path: record.path,
            size: ByteSize(unsigned_integer("size_bytes", record.size_bytes)?),
            created_at: record.created_at.map(FsTimestamp),
            modified_at: record.modified_at.map(FsTimestamp),
            accessed_at: record.accessed_at.map(FsTimestamp),
            kind: parse_file_kind(&record.file_kind)?,
            unit: parse_candidate_unit(&record.unit)?,
            category: parse_category(&record.category)?,
            safety: parse_safety_level(&record.safety)?,
            decision: parse_decision(&record.decision)?,
            detector_id: record.detector_id,
            explanation: record.explanation,
            attributes: FileAttributes {
                raw_bits: unsigned_integer("attributes", record.attributes)? as u32,
                is_readonly: record.is_readonly != 0,
                is_hidden: record.is_hidden != 0,
                is_system: record.is_system != 0,
                is_temporary: record.is_temporary != 0,
            },
        })
    }
}

fn sqlite_integer(field: &'static str, value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| IndexSqliteError::InvalidUnsignedInteger { field, value })
}

fn unsigned_integer(field: &'static str, value: i64) -> Result<u64> {
    if value < 0 {
        return Err(IndexSqliteError::InvalidEnumValue {
            field,
            value: value.to_string(),
        });
    }

    Ok(value as u64)
}

fn bool_integer(value: bool) -> i64 {
    i64::from(value)
}

fn file_kind_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Video => "video",
        FileKind::Image => "image",
        FileKind::Audio => "audio",
        FileKind::Archive => "archive",
        FileKind::Installer => "installer",
        FileKind::DiskImage => "disk_image",
        FileKind::Document => "document",
        FileKind::Other => "other",
    }
}

fn parse_file_kind(value: &str) -> Result<FileKind> {
    match value {
        "video" => Ok(FileKind::Video),
        "image" => Ok(FileKind::Image),
        "audio" => Ok(FileKind::Audio),
        "archive" => Ok(FileKind::Archive),
        "installer" => Ok(FileKind::Installer),
        "disk_image" => Ok(FileKind::DiskImage),
        "document" => Ok(FileKind::Document),
        "other" => Ok(FileKind::Other),
        value => invalid_enum("file_kind", value),
    }
}

fn candidate_unit_name(unit: CandidateUnit) -> &'static str {
    match unit {
        CandidateUnit::File => "file",
        CandidateUnit::Folder => "folder",
    }
}

fn parse_candidate_unit(value: &str) -> Result<CandidateUnit> {
    match value {
        "file" => Ok(CandidateUnit::File),
        "folder" => Ok(CandidateUnit::Folder),
        value => invalid_enum("candidate_unit", value),
    }
}

fn category_name(category: CategoryId) -> &'static str {
    match category {
        CategoryId::LargeFiles => "large_files",
        CategoryId::OldFiles => "old_files",
        CategoryId::ForgottenVideos => "forgotten_videos",
        CategoryId::Duplicates => "duplicates",
        CategoryId::Archives => "archives",
        CategoryId::Installers => "installers",
        CategoryId::TempFiles => "temp_files",
        CategoryId::AppCaches => "app_caches",
        CategoryId::DevArtifacts => "dev_artifacts",
        // Не входить у схему CHECK: uncategorized-записи живуть лише в гарячому
        // in-memory індексі. Спроба записати такий у SQLite впаде на CHECK —
        // це навмисний інваріант «persistent = лише категоризоване».
        CategoryId::Uncategorized => "uncategorized",
    }
}

fn parse_category(value: &str) -> Result<CategoryId> {
    match value {
        "large_files" => Ok(CategoryId::LargeFiles),
        "old_files" => Ok(CategoryId::OldFiles),
        "forgotten_videos" => Ok(CategoryId::ForgottenVideos),
        "duplicates" => Ok(CategoryId::Duplicates),
        "archives" => Ok(CategoryId::Archives),
        "installers" => Ok(CategoryId::Installers),
        "temp_files" => Ok(CategoryId::TempFiles),
        "app_caches" => Ok(CategoryId::AppCaches),
        "dev_artifacts" => Ok(CategoryId::DevArtifacts),
        "uncategorized" => Ok(CategoryId::Uncategorized),
        value => invalid_enum("category", value),
    }
}

fn safety_level_name(safety: SafetyLevel) -> &'static str {
    match safety {
        SafetyLevel::SafeToBulk => "safe_to_bulk",
        SafetyLevel::ReviewRecommended => "review_recommended",
    }
}

fn parse_safety_level(value: &str) -> Result<SafetyLevel> {
    match value {
        "safe_to_bulk" => Ok(SafetyLevel::SafeToBulk),
        "review_recommended" => Ok(SafetyLevel::ReviewRecommended),
        value => invalid_enum("safety", value),
    }
}

fn decision_name(decision: Decision) -> &'static str {
    match decision {
        Decision::Undecided => "undecided",
        Decision::Keep => "keep",
        Decision::Marked => "marked",
    }
}

fn parse_decision(value: &str) -> Result<Decision> {
    match value {
        "undecided" => Ok(Decision::Undecided),
        "keep" => Ok(Decision::Keep),
        "marked" => Ok(Decision::Marked),
        value => invalid_enum("decision", value),
    }
}

fn invalid_enum<T>(field: &'static str, value: &str) -> Result<T> {
    Err(IndexSqliteError::InvalidEnumValue {
        field,
        value: value.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::TransactionBehavior;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn creates_database_in_profile_with_wal_mode() {
        let profile_dir = temp_profile_dir("profile");

        let database = IndexDatabase::open_profile(&profile_dir).expect("open profile database");

        assert_eq!(database.path(), profile_dir.join(DATABASE_FILE_NAME));
        assert!(database.path().is_file());
        assert_eq!(database.journal_mode().expect("journal mode"), "wal");
        assert!(database.foreign_keys_enabled().expect("foreign keys"));

        cleanup(profile_dir);
    }

    #[test]
    fn reader_is_not_blocked_by_active_writer_transaction() {
        let profile_dir = temp_profile_dir("wal-reader");
        let database_path = profile_dir.join(DATABASE_FILE_NAME);
        let mut writer = IndexDatabase::open(&database_path).expect("open writer");
        let reader = IndexDatabase::open(&database_path).expect("open reader");

        writer
            .write_meta("probe", "before")
            .expect("write initial metadata");

        let transaction = writer
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("start writer transaction");
        transaction
            .execute(
                "UPDATE __trashradar_meta SET value = ?1 WHERE key = ?2",
                ("during", "probe"),
            )
            .expect("write uncommitted metadata");

        assert_eq!(
            reader.read_meta("probe").expect("read during write"),
            Some("before".to_string())
        );

        transaction.commit().expect("commit writer transaction");

        assert_eq!(
            reader.read_meta("probe").expect("read after commit"),
            Some("during".to_string())
        );

        cleanup(profile_dir);
    }

    #[test]
    fn hash_cache_roundtrip_and_merge_t062() {
        use trashradar_app::ports::HashCache;
        use trashradar_domain::duplicates::{ContentHash, FileHashCacheEntry, PartialHash};

        let profile_dir = temp_profile_dir("hash-cache-v3");
        let raw = IndexDatabase::open_profile(&profile_dir).expect("open");
        assert_eq!(raw.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let db = ThreadSafeHashCache::new(raw);

        let mut entry = FileHashCacheEntry::new(r"C:\T\x.bin", ByteSize(99), Some(FsTimestamp(7)));
        let mut ph = [0u8; 32];
        ph[0] = 0xAA;
        entry.partial = Some(PartialHash(ph));
        db.put_entry(&entry).unwrap();

        let got = db.get_entry(r"C:\t\X.BIN").unwrap().unwrap();
        assert_eq!(got.size.0, 99);
        assert_eq!(got.modified_at, Some(FsTimestamp(7)));
        assert_eq!(got.partial.unwrap().0[0], 0xAA);
        assert!(got.content.is_none());

        let mut full = FileHashCacheEntry::new(r"C:\T\x.bin", ByteSize(99), Some(FsTimestamp(7)));
        full.content = Some(ContentHash([0xBB; 32]));
        db.put_entry(&full).unwrap();
        let got2 = db.get_entry(r"C:\T\x.bin").unwrap().unwrap();
        assert_eq!(got2.partial.unwrap().0[0], 0xAA); // merge kept partial
        assert_eq!(got2.content.unwrap().0[0], 0xBB);

        cleanup(profile_dir);
    }

    /// T-073: схема v5 приймає вид `large`; слот незалежний від `thumbnail`.
    #[test]
    fn preview_cache_accepts_large_kind_t073() {
        use trashradar_app::ports::{PreviewCache, PreviewCacheEntry, PreviewKind};

        let profile_dir = temp_profile_dir("preview-cache-v5-large");
        let raw = IndexDatabase::open_profile(&profile_dir).expect("open");
        assert_eq!(raw.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let db = ThreadSafePreviewCache::new(raw);

        let large = PreviewCacheEntry {
            source_path: r"C:\T\photo.jpg".to_string(),
            preview_kind: PreviewKind::Large,
            size: ByteSize(4096),
            modified_at: Some(FsTimestamp(777)),
            preview_path: r"C:\cache\abc_large.png".to_string(),
        };
        db.put_preview_entry(&large).unwrap();

        let got = db
            .get_preview_entry(r"C:\T\photo.jpg", PreviewKind::Large)
            .unwrap()
            .unwrap();
        assert_eq!(got.preview_kind, PreviewKind::Large);
        assert_eq!(got.preview_path, r"C:\cache\abc_large.png");

        // Слот thumbnail того самого шляху — окремий і порожній.
        assert!(db
            .get_preview_entry(r"C:\T\photo.jpg", PreviewKind::Thumbnail)
            .unwrap()
            .is_none());

        cleanup(profile_dir);
    }

    #[test]
    fn preview_cache_roundtrip_t068() {
        use trashradar_app::ports::{PreviewCache, PreviewCacheEntry, PreviewKind};

        let profile_dir = temp_profile_dir("preview-cache-v4");
        let raw = IndexDatabase::open_profile(&profile_dir).expect("open");
        assert_eq!(raw.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let db = ThreadSafePreviewCache::new(raw);

        let entry1 = PreviewCacheEntry {
            source_path: r"C:\T\video.mp4".to_string(),
            preview_kind: PreviewKind::Thumbnail,
            size: ByteSize(1024),
            modified_at: Some(FsTimestamp(12345)),
            preview_path: r"C:\cache\123_thumb.jpg".to_string(),
        };

        db.put_preview_entry(&entry1).unwrap();

        let got = db
            .get_preview_entry(r"C:\T\video.mp4", PreviewKind::Thumbnail)
            .unwrap()
            .unwrap();
        assert_eq!(got.source_path, r"C:\T\video.mp4");
        assert_eq!(got.preview_kind, PreviewKind::Thumbnail);
        assert_eq!(got.size.0, 1024);
        assert_eq!(got.modified_at, Some(FsTimestamp(12345)));
        assert_eq!(got.preview_path, r"C:\cache\123_thumb.jpg");

        // Verify other kind is None
        assert!(db
            .get_preview_entry(r"C:\T\video.mp4", PreviewKind::Scrub)
            .unwrap()
            .is_none());

        // Update
        let entry2 = PreviewCacheEntry {
            source_path: r"C:\T\video.mp4".to_string(),
            preview_kind: PreviewKind::Thumbnail,
            size: ByteSize(2048),
            modified_at: Some(FsTimestamp(67890)),
            preview_path: r"C:\cache\123_thumb_updated.jpg".to_string(),
        };
        db.put_preview_entry(&entry2).unwrap();

        let got_updated = db
            .get_preview_entry(r"C:\T\video.mp4", PreviewKind::Thumbnail)
            .unwrap()
            .unwrap();
        assert_eq!(got_updated.size.0, 2048);
        assert_eq!(got_updated.modified_at, Some(FsTimestamp(67890)));
        assert_eq!(got_updated.preview_path, r"C:\cache\123_thumb_updated.jpg");

        // Delete specific
        db.delete_preview_entry(r"C:\T\video.mp4", PreviewKind::Thumbnail)
            .unwrap();
        assert!(db
            .get_preview_entry(r"C:\T\video.mp4", PreviewKind::Thumbnail)
            .unwrap()
            .is_none());

        // Test delete all for path
        db.put_preview_entry(&entry1).unwrap();
        let entry_scrub = PreviewCacheEntry {
            source_path: r"C:\T\video.mp4".to_string(),
            preview_kind: PreviewKind::Scrub,
            size: ByteSize(1024),
            modified_at: Some(FsTimestamp(12345)),
            preview_path: r"C:\cache\123_scrub.png".to_string(),
        };
        db.put_preview_entry(&entry_scrub).unwrap();

        assert!(db
            .get_preview_entry(r"C:\T\video.mp4", PreviewKind::Thumbnail)
            .unwrap()
            .is_some());
        assert!(db
            .get_preview_entry(r"C:\T\video.mp4", PreviewKind::Scrub)
            .unwrap()
            .is_some());

        db.delete_all_previews_for_path(r"C:\T\video.mp4").unwrap();
        assert!(db
            .get_preview_entry(r"C:\T\video.mp4", PreviewKind::Thumbnail)
            .unwrap()
            .is_none());
        assert!(db
            .get_preview_entry(r"C:\T\video.mp4", PreviewKind::Scrub)
            .unwrap()
            .is_none());

        cleanup(profile_dir);
    }

    #[test]
    fn creates_schema_v1_for_file_records() {
        let profile_dir = temp_profile_dir("schema-v1");
        let database = IndexDatabase::open_profile(&profile_dir).expect("open profile database");

        assert_eq!(
            database.schema_version().expect("schema version"),
            LATEST_SCHEMA_VERSION
        );

        let columns: Vec<String> = database
            .connection
            .prepare("PRAGMA table_info(file_records)")
            .expect("prepare table info")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query table info")
            .collect::<std::result::Result<_, _>>()
            .expect("read columns");

        for expected in [
            "candidate_id",
            "path",
            "size_bytes",
            "created_at_filetime",
            "modified_at_filetime",
            "accessed_at_filetime",
            "file_kind",
            "candidate_unit",
            "category",
            "safety",
            "decision",
            "detector_id",
            "explanation",
            "attributes",
            "is_readonly",
            "is_hidden",
            "is_system",
            "is_temporary",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "missing column {expected}"
            );
        }

        // T-029: volume_usn_state у схемі v2.
        let usn_table: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='volume_usn_state'",
                [],
                |row| row.get(0),
            )
            .expect("usn table");
        assert_eq!(usn_table, 1);

        cleanup(profile_dir);
    }

    #[test]
    fn delete_file_records_by_path_removes_row() {
        let profile_dir = temp_profile_dir("delete-by-path");
        let mut database = IndexDatabase::open_profile(&profile_dir).expect("open");
        let record = sample_file_record(1);
        let path = record.path.clone();
        database.upsert_file_record(&record).expect("upsert");
        assert!(database
            .read_file_record(CandidateId(1))
            .expect("read")
            .is_some());
        let n = database
            .delete_file_records_by_path(&path.to_ascii_lowercase())
            .expect("delete");
        assert_eq!(n, 1);
        assert!(database
            .read_file_record(CandidateId(1))
            .expect("read2")
            .is_none());
        cleanup(profile_dir);
    }

    #[test]
    fn usn_cursor_roundtrip_and_overwrite() {
        let profile_dir = temp_profile_dir("usn-cursor");
        let database = IndexDatabase::open_profile(&profile_dir).expect("open");

        assert_eq!(database.get_usn_cursor('C').expect("get empty"), None);

        let cursor = UsnCursor {
            journal_id: 0xAABB_CCDD_1122_3344,
            next_usn: 1_000_042,
        };
        database.set_usn_cursor('c', cursor).expect("set");
        assert_eq!(database.get_usn_cursor('C').expect("get"), Some(cursor));

        // Перезапис після «обробки дельти».
        let next = UsnCursor {
            journal_id: cursor.journal_id,
            next_usn: 1_000_100,
        };
        database.set_usn_cursor('C', next).expect("update");
        assert_eq!(database.get_usn_cursor('C').expect("get2"), Some(next));

        // Інший том — незалежний.
        assert_eq!(database.get_usn_cursor('D').expect("other"), None);

        // T-031: clear перед full rescan.
        database.clear_usn_cursor('C').expect("clear");
        assert_eq!(database.get_usn_cursor('C').expect("after clear"), None);

        cleanup(profile_dir);
    }

    #[test]
    fn migrates_t010_database_to_latest_schema() {
        let profile_dir = temp_profile_dir("migrate-t010");
        let database_path = profile_dir.join(DATABASE_FILE_NAME);
        fs::create_dir_all(&profile_dir).expect("create profile dir");

        {
            let connection = Connection::open(&database_path).expect("open legacy database");
            connection
                .execute_batch(
                    "CREATE TABLE __trashradar_meta (
                        key TEXT PRIMARY KEY,
                        value TEXT NOT NULL
                    );
                    INSERT INTO __trashradar_meta (key, value) VALUES ('legacy', 'kept');
                    PRAGMA user_version = 0;",
                )
                .expect("create legacy schema");
        }

        let database = IndexDatabase::open(&database_path).expect("migrate database");

        assert_eq!(
            database.schema_version().expect("schema version"),
            LATEST_SCHEMA_VERSION
        );
        assert_eq!(
            database.read_meta("legacy").expect("read migrated meta"),
            Some("kept".to_string())
        );

        let table_exists: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'file_records'",
                [],
                |row| row.get(0),
            )
            .expect("query file_records table");
        assert_eq!(table_exists, 1);

        let usn_exists: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'volume_usn_state'",
                [],
                |row| row.get(0),
            )
            .expect("query volume_usn_state");
        assert_eq!(usn_exists, 1);

        cleanup(profile_dir);
    }

    #[test]
    fn failed_schema_migration_reopens_with_clean_cold_start() {
        let profile_dir = temp_profile_dir("migration-cold-start");
        let database_path = profile_dir.join(DATABASE_FILE_NAME);
        fs::create_dir_all(&profile_dir).expect("create profile dir");

        {
            let connection = Connection::open(&database_path).expect("open legacy database");
            connection
                .execute_batch(
                    "CREATE TABLE __trashradar_meta (
                        key TEXT PRIMARY KEY,
                        value TEXT NOT NULL
                    );
                    INSERT INTO __trashradar_meta (key, value) VALUES ('legacy', 'discarded');
                    CREATE TABLE file_records (bad INTEGER NOT NULL);
                    PRAGMA user_version = 0;",
                )
                .expect("create broken legacy schema");
        }

        let database = IndexDatabase::open(&database_path).expect("cold-start database");

        assert_eq!(
            database.schema_version().expect("schema version"),
            LATEST_SCHEMA_VERSION
        );
        assert_eq!(
            database.read_meta("legacy").expect("read legacy meta"),
            None
        );

        let columns: Vec<String> = database
            .connection
            .prepare("PRAGMA table_info(file_records)")
            .expect("prepare table info")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query table info")
            .collect::<std::result::Result<_, _>>()
            .expect("read columns");

        assert!(columns.iter().any(|column| column == "candidate_id"));
        assert!(!columns.iter().any(|column| column == "bad"));

        cleanup(profile_dir);
    }

    #[test]
    fn upserts_and_reads_file_record_with_mvp_attributes() {
        let profile_dir = temp_profile_dir("file-record");
        let mut database =
            IndexDatabase::open_profile(&profile_dir).expect("open profile database");
        let record = sample_file_record(42);

        database
            .upsert_file_record(&record)
            .expect("upsert file record");

        let stored = database
            .read_file_record(CandidateId(42))
            .expect("read file record")
            .expect("stored record");

        assert_eq!(stored, record);

        cleanup(profile_dir);
    }

    #[test]
    fn keep_decision_survives_reopen() {
        // DoD T-057: рішення Keep переживає перезапуск (persistent SQLite).
        let profile_dir = temp_profile_dir("decision-keep-restart");
        {
            let mut database = IndexDatabase::open_profile(&profile_dir).expect("open");
            let mut record = sample_file_record(7);
            record.decision = Decision::Keep;
            database.upsert_file_record(&record).expect("upsert keep");
        }
        // reopen profile
        let database = IndexDatabase::open_profile(&profile_dir).expect("reopen");
        let stored = database
            .read_file_record(CandidateId(7))
            .expect("read")
            .expect("row");
        assert_eq!(stored.decision, Decision::Keep);
        cleanup(profile_dir);
    }

    #[test]
    fn upserts_file_records_in_single_batch_transaction() {
        let profile_dir = temp_profile_dir("file-record-batch");
        let mut database =
            IndexDatabase::open_profile(&profile_dir).expect("open profile database");
        let records: Vec<_> = (0..512).map(sample_file_record).collect();

        let report = database
            .upsert_file_records_batch(&records)
            .expect("upsert file record batch");

        assert_eq!(report.records_written, records.len());
        assert_eq!(
            database
                .read_file_record(CandidateId(0))
                .expect("read first record"),
            Some(records[0].clone())
        );
        assert_eq!(
            database
                .read_file_record(CandidateId(511))
                .expect("read last record"),
            Some(records[511].clone())
        );

        cleanup(profile_dir);
    }

    #[test]
    fn writer_queue_serializes_concurrent_batches_without_database_locks() {
        let profile_dir = temp_profile_dir("writer-queue");
        let database_path = profile_dir.join(DATABASE_FILE_NAME);
        let queue = IndexWriterQueue::open(&database_path).expect("open writer queue");
        let mut joins = Vec::new();

        for worker in 0..8_u64 {
            let writer = queue.handle();
            joins.push(std::thread::spawn(move || {
                let base = worker * 1_000;
                let records: Vec<_> = (base..base + 250).map(sample_file_record).collect();
                writer
                    .upsert_file_records(records)
                    .expect("upsert queued batch")
            }));
        }

        let mut written = 0;
        for join in joins {
            written += join.join().expect("join producer").records_written;
        }

        queue.shutdown().expect("shutdown writer queue");

        let database = IndexDatabase::open(&database_path).expect("open reader database");
        assert_eq!(count_file_records(&database), written as i64);

        cleanup(profile_dir);
    }

    #[test]
    #[ignore = "local T-013 perf gate; writes 1M rows and is intentionally not part of default unit tests"]
    fn writer_queue_inserts_one_million_records_in_batches() {
        const TOTAL_RECORDS: u64 = 1_000_000;
        const BATCH_SIZE: u64 = 100_000;
        const TARGET: Duration = Duration::from_secs(70);

        let profile_dir = temp_profile_dir("writer-queue-1m");
        let database_path = profile_dir.join(DATABASE_FILE_NAME);
        let queue = IndexWriterQueue::open(&database_path).expect("open writer queue");
        let writer = queue.handle();
        let mut elapsed = Duration::ZERO;
        let mut written = 0;

        for base in (0..TOTAL_RECORDS).step_by(BATCH_SIZE as usize) {
            let records: Vec<_> = (base..base + BATCH_SIZE).map(sample_file_record).collect();
            let started_at = Instant::now();
            written += writer
                .upsert_file_records(records)
                .expect("upsert queued perf batch")
                .records_written;
            elapsed += started_at.elapsed();
        }

        queue.shutdown().expect("shutdown writer queue");

        let database = IndexDatabase::open(&database_path).expect("open reader database");
        assert_eq!(written as u64, TOTAL_RECORDS);
        assert_eq!(count_file_records(&database), TOTAL_RECORDS as i64);
        assert!(
            elapsed <= TARGET,
            "1M batched inserts took {elapsed:?}, expected <= {TARGET:?}"
        );

        cleanup(profile_dir);
    }

    #[test]
    fn reads_file_records_window_with_sorting_and_paging() {
        let profile_dir = temp_profile_dir("window-queries");
        let mut database =
            IndexDatabase::open_profile(&profile_dir).expect("open profile database");

        // Explain query plan
        for (inner_sort, outer_sort) in [
            (
                "size_bytes DESC, candidate_id ASC",
                "f.size_bytes DESC, f.candidate_id ASC",
            ),
            (
                "accessed_at_filetime ASC, candidate_id ASC",
                "f.accessed_at_filetime ASC, f.candidate_id ASC",
            ),
            (
                "accessed_at_filetime DESC, candidate_id DESC",
                "f.accessed_at_filetime DESC, f.candidate_id DESC",
            ),
        ] {
            let sql = format!(
                "SELECT f.candidate_id FROM file_records f JOIN (
                    SELECT candidate_id FROM file_records WHERE category = 'forgotten_videos' ORDER BY {} LIMIT 200 OFFSET 500000
                ) sub ON f.candidate_id = sub.candidate_id ORDER BY {}",
                inner_sort, outer_sort
            );
            let mut stmt = database
                .connection
                .prepare(&format!("EXPLAIN QUERY PLAN {}", sql))
                .unwrap();
            let mut rows = stmt.query([]).unwrap();
            println!("--- QUERY PLAN FOR {} ---", inner_sort);
            while let Some(row) = rows.next().unwrap() {
                let detail: String = row.get(3).unwrap();
                println!("{}", detail);
            }
        }

        // Insert 5 records into ForgottenVideos category, and 2 into LargeFiles.
        // Vary sizes and accessed times:
        // Record 1: size=100, accessed=10
        // Record 2: size=200, accessed=30
        // Record 3: size=300, accessed=20
        // Record 4: size=400, accessed=50
        // Record 5: size=500, accessed=40
        let mut records = Vec::new();
        for i in 1..=5 {
            let mut record = sample_file_record(i);
            record.category = CategoryId::ForgottenVideos;
            record.size = ByteSize(i * 100);
            // Accessed times: 10, 30, 20, 50, 40
            let accessed_vals = [0, 10, 30, 20, 50, 40];
            record.accessed_at = Some(FsTimestamp(accessed_vals[i as usize]));
            records.push(record);
        }

        // Two records in LargeFiles
        for i in 6..=7 {
            let mut record = sample_file_record(i);
            record.category = CategoryId::LargeFiles;
            records.push(record);
        }

        database
            .upsert_file_records_batch(&records)
            .expect("upsert records");

        // Query ForgottenVideos:
        // SizeDesc sorting, limit 3, offset 1
        // Records sorted by size desc:
        // #5 (500), #4 (400), #3 (300), #2 (200), #1 (100)
        // With offset 1 and limit 3, we expect: #4 (400), #3 (300), #2 (200)
        let window = database
            .read_file_records_window(CategoryId::ForgottenVideos, FileRecordSort::SizeDesc, 1, 3)
            .expect("query window size desc");

        assert_eq!(window.len(), 3);
        assert_eq!(window[0].candidate_id, CandidateId(4));
        assert_eq!(window[1].candidate_id, CandidateId(3));
        assert_eq!(window[2].candidate_id, CandidateId(2));

        // AccessedAsc sorting, limit 3, offset 1
        // Records sorted by accessed asc:
        // #1 (10), #3 (20), #2 (30), #5 (40), #4 (50)
        // With offset 1 and limit 3, we expect: #3 (20), #2 (30), #5 (40)
        let window = database
            .read_file_records_window(
                CategoryId::ForgottenVideos,
                FileRecordSort::AccessedAsc,
                1,
                3,
            )
            .expect("query window accessed asc");

        assert_eq!(window.len(), 3);
        assert_eq!(window[0].candidate_id, CandidateId(3));
        assert_eq!(window[1].candidate_id, CandidateId(2));
        assert_eq!(window[2].candidate_id, CandidateId(5));

        // AccessedDesc sorting, limit 2, offset 0
        // Records sorted by accessed desc:
        // #4 (50), #5 (40), #2 (30), #3 (20), #1 (10)
        // With offset 0 and limit 2, we expect: #4 (50), #5 (40)
        let window = database
            .read_file_records_window(
                CategoryId::ForgottenVideos,
                FileRecordSort::AccessedDesc,
                0,
                2,
            )
            .expect("query window accessed desc");

        assert_eq!(window.len(), 2);
        assert_eq!(window[0].candidate_id, CandidateId(4));
        assert_eq!(window[1].candidate_id, CandidateId(5));

        // Query LargeFiles category: limit 10, offset 0 -> expect 2 records
        let window = database
            .read_file_records_window(CategoryId::LargeFiles, FileRecordSort::SizeDesc, 0, 10)
            .expect("query large files");
        assert_eq!(window.len(), 2);

        cleanup(profile_dir);
    }

    #[test]
    #[ignore = "local T-014 perf gate; queries 1M rows and is intentionally not part of default unit tests"]
    fn window_queries_on_one_million_records_are_under_fifty_milliseconds() {
        const TOTAL_RECORDS: u64 = 1_000_000;
        const BATCH_SIZE: u64 = 100_000;
        const LIMIT: u64 = 200;
        const TARGET: Duration = Duration::from_millis(50);

        let profile_dir = temp_profile_dir("window-queries-1m");
        let database_path = profile_dir.join(DATABASE_FILE_NAME);
        let queue = IndexWriterQueue::open(&database_path).expect("open writer queue");
        let writer = queue.handle();

        for base in (0..TOTAL_RECORDS).step_by(BATCH_SIZE as usize) {
            let records: Vec<_> = (base..base + BATCH_SIZE)
                .map(|id| {
                    let mut record = sample_file_record(id);
                    // Make size vary so index is fully populated
                    record.size = ByteSize(id * 73 % 999983);
                    record.accessed_at = Some(FsTimestamp((id * 31 % 999983) as i64));
                    record
                })
                .collect();
            writer
                .upsert_file_records(records)
                .expect("upsert queued perf batch");
        }

        queue.shutdown().expect("shutdown writer queue");

        let database = IndexDatabase::open(&database_path).expect("open reader database");
        assert_eq!(count_file_records(&database), TOTAL_RECORDS as i64);

        // Warm up cache for SizeDesc
        let _ = database
            .read_file_records_window(
                CategoryId::ForgottenVideos,
                FileRecordSort::SizeDesc,
                500_000,
                LIMIT,
            )
            .expect("query window size desc warm");

        // Perform paged query and measure time
        let start = Instant::now();
        let records = database
            .read_file_records_window(
                CategoryId::ForgottenVideos,
                FileRecordSort::SizeDesc,
                500_000,
                LIMIT,
            )
            .expect("query window size desc");
        let elapsed = start.elapsed();

        assert_eq!(records.len(), LIMIT as usize);
        assert!(
            elapsed <= TARGET,
            "Querying 200/1M records took {elapsed:?}, expected <= {TARGET:?}"
        );

        // Warm up cache for AccessedAsc
        let _ = database
            .read_file_records_window(
                CategoryId::ForgottenVideos,
                FileRecordSort::AccessedAsc,
                500_000,
                LIMIT,
            )
            .expect("query window accessed asc warm");

        // Try accessed sort
        let start = Instant::now();
        let records = database
            .read_file_records_window(
                CategoryId::ForgottenVideos,
                FileRecordSort::AccessedAsc,
                500_000,
                LIMIT,
            )
            .expect("query window accessed asc");
        let elapsed = start.elapsed();

        assert_eq!(records.len(), LIMIT as usize);
        assert!(
            elapsed <= TARGET,
            "Querying 200/1M records by accessed asc took {elapsed:?}, expected <= {TARGET:?}"
        );

        cleanup(profile_dir);
    }

    fn sample_file_record(id: u64) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: format!(r"C:\Users\Ada\Videos\raw-{id}.mov"),
            size: ByteSize(4_294_967_296 + id),
            created_at: Some(FsTimestamp(132_537_600_000_000_000 + id as i64)),
            modified_at: Some(FsTimestamp(132_624_000_000_000_000 + id as i64)),
            accessed_at: Some(FsTimestamp(132_710_400_000_000_000 + id as i64)),
            kind: FileKind::Video,
            unit: CandidateUnit::File,
            category: CategoryId::ForgottenVideos,
            safety: SafetyLevel::ReviewRecommended,
            decision: Decision::Undecided,
            detector_id: "forgotten_videos.v1".to_string(),
            explanation: "large video not opened recently".to_string(),
            attributes: FileAttributes {
                raw_bits: 0x23,
                is_readonly: true,
                is_hidden: true,
                is_system: false,
                is_temporary: false,
            },
        }
    }

    fn count_file_records(database: &IndexDatabase) -> i64 {
        database
            .connection
            .query_row("SELECT COUNT(*) FROM file_records", [], |row| row.get(0))
            .expect("count file records")
    }

    fn temp_profile_dir(name: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "trashradar-index-sqlite-{}-{id}-{name}",
            std::process::id()
        ))
    }

    fn cleanup(path: PathBuf) {
        let _ = fs::remove_dir_all(path);
    }
}
