//! Адаптер `IndexStore`: SQLite у WAL-режимі.
//!
//! Реалізація: T-010 (підключення), T-011 (схема v1), T-012 (міграції),
//! T-013 (батчевий запис), T-014 (віконні запити), T-078 (журнал Quarantine).

use rusqlite::{Connection, OptionalExtension};
use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    time::Duration,
};

const DATABASE_FILE_NAME: &str = "index.sqlite3";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub type Result<T> = std::result::Result<T, IndexSqliteError>;

#[derive(Debug)]
pub enum IndexSqliteError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
}

impl fmt::Display for IndexSqliteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "sqlite index I/O error: {error}"),
            Self::Sqlite(error) => write!(f, "sqlite index error: {error}"),
        }
    }
}

impl Error for IndexSqliteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
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

impl IndexDatabase {
    pub fn open_profile(profile_dir: impl AsRef<Path>) -> Result<Self> {
        Self::open(profile_dir.as_ref().join(DATABASE_FILE_NAME))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let connection = Connection::open(&path)?;
        configure_connection(&connection)?;
        ensure_bootstrap_schema(&connection)?;

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
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.busy_timeout(BUSY_TIMEOUT)?;

    Ok(())
}

fn ensure_bootstrap_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS __trashradar_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::TransactionBehavior;
    use std::sync::atomic::{AtomicU64, Ordering};

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
