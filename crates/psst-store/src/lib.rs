//! `SQLite` connection policy and forward-only schema migrations for Psst.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

mod authenticated;
mod inbox;
mod instance;
mod message;
mod repository;

pub use authenticated::{
    AuthenticatedSession, InboxPage, JoinAndClaim, JoinAndClaimOutcome, LeaveOutcome, MessageView,
    SendByName, SendOutcome, SessionContext, TranscriptByName, TranscriptQuery,
};
pub use inbox::{
    AcknowledgeMessages, InboxQuery, MAX_ACK_MESSAGES, MAX_INBOX_MESSAGES, MAX_INBOX_OUTPUT_BYTES,
};
pub use instance::{
    ClaimInstance, ClaimOutcome, DEFAULT_HEARTBEAT_INTERVAL, DEFAULT_LEASE_DURATION,
    HeartbeatInstance, InstanceRecord, LeasePolicy, ResumeInstance,
};
pub use message::{MessageRecord, SendMessage};
pub use repository::{
    CreateSquad, JoinMembership, MembershipRecord, RepositoryError, RosterMember, SquadRecord,
    TransportPresence,
};

const APPLICATION_ID: i32 = 0x5053_5354; // "PSST"
const BUSY_TIMEOUT: Duration = Duration::from_millis(2_000);
const JOURNAL_RETRY_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
struct Migration {
    version: i64,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/001_initial.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/002_indexes.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("../migrations/003_instance_owner.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("../migrations/004_inbox_order.sql"),
    },
];

/// Returns the schema version embedded in this build.
///
/// # Panics
/// Panics only when the compile-time migration table contains a version outside `u32`.
#[must_use]
pub fn current_schema_version() -> u32 {
    u32::try_from(MIGRATIONS.last().map_or(0, |migration| migration.version))
        .expect("validated migration versions fit u32")
}

/// Errors that can prevent a store from opening safely.
#[derive(Debug)]
pub enum StoreError {
    Database(rusqlite::Error),
    UnexpectedApplicationId { actual: i32 },
    FutureSchema { database: i64, supported: i64 },
    MigrationChecksumMismatch { version: i64 },
    InvalidMigrationLedger { expected: i64, actual: i64 },
    InvalidMigrationPlan(&'static str),
    WorkerUnavailable,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(_) => formatter.write_str("SQLite store operation failed"),
            Self::UnexpectedApplicationId { actual } => {
                write!(
                    formatter,
                    "database belongs to another application ({actual})"
                )
            }
            Self::FutureSchema {
                database,
                supported,
            } => write!(
                formatter,
                "database schema version {database} is newer than supported version {supported}"
            ),
            Self::MigrationChecksumMismatch { version } => {
                write!(formatter, "migration {version} checksum does not match")
            }
            Self::InvalidMigrationLedger { expected, actual } => write!(
                formatter,
                "migration ledger expected version {expected} but found {actual}"
            ),
            Self::InvalidMigrationPlan(message) => formatter.write_str(message),
            Self::WorkerUnavailable => formatter.write_str("store worker could not start"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

/// One configured `SQLite` connection. Higher layers should serialize access through
/// a bounded worker rather than share this connection globally.
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens a real `SQLite` file, applies the connection policy, and migrates it.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be opened or configured, when migration
    /// integrity checks fail, or when a migration cannot be committed atomically.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with_migrations(path.as_ref(), MIGRATIONS)
    }

    fn open_with_migrations(path: &Path, migrations: &[Migration]) -> Result<Self, StoreError> {
        validate_migration_plan(migrations)?;
        let mut connection = Connection::open(path)?;
        verify_application_id(&connection)?;
        configure_connection(&connection)?;
        migrate(&mut connection, migrations)?;
        Ok(Self { connection })
    }

    /// Returns the latest migration version recorded in the database.
    ///
    /// # Errors
    ///
    /// Returns an error when the migration metadata cannot be read.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        Ok(current_version(&self.connection)?.unwrap_or(0))
    }

    /// Verifies that the connection is usable and the embedded schema is current.
    ///
    /// # Errors
    ///
    /// Returns a stable store error when the query fails or the schema is incompatible.
    pub fn readiness(&self) -> Result<(), StoreError> {
        let version = self.schema_version()?;
        let supported = MIGRATIONS.last().map_or(0, |migration| migration.version);
        if version != supported {
            return Err(StoreError::FutureSchema {
                database: version,
                supported,
            });
        }
        self.connection.query_row("SELECT 1", [], |_| Ok(()))?;
        Ok(())
    }

    /// Performs a bounded passive WAL checkpoint.
    ///
    /// # Errors
    ///
    /// Returns a stable store error when `SQLite` cannot complete the checkpoint.
    pub fn checkpoint(&self) -> Result<(), StoreError> {
        self.connection
            .pragma_update(None, "wal_checkpoint", "PASSIVE")?;
        Ok(())
    }

    /// Opens a second fully configured connection to the same database file.
    ///
    /// Callers should normally open a new [`Store`] with [`Store::open`]. This
    /// method intentionally remains absent: a store does not retain or reveal its
    /// filesystem path, preventing accidental creation of unconfigured connections.
    fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    enable_wal_with_bounded_retry(connection)?;
    // FULL is intentional: accepted writes must survive an OS crash, not merely a process crash.
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "wal_autocheckpoint", 1_000_i64)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    Ok(())
}

fn verify_application_id(connection: &Connection) -> Result<(), StoreError> {
    let application_id: i32 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if application_id != 0 && application_id != APPLICATION_ID {
        return Err(StoreError::UnexpectedApplicationId {
            actual: application_id,
        });
    }
    Ok(())
}

fn enable_wal_with_bounded_retry(connection: &Connection) -> Result<(), StoreError> {
    // SQLite can return BUSY immediately when two fresh connections race to change
    // journal mode, even with a busy handler. Retry only that transition and only
    // within the same documented two-second bound.
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        match connection.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => return Ok(()),
            Err(error) if is_busy(&error) && std::time::Instant::now() < deadline => {
                std::thread::sleep(JOURNAL_RETRY_INTERVAL);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if matches!(inner.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn validate_migration_plan(migrations: &[Migration]) -> Result<(), StoreError> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = i64::try_from(index).expect("migration count fits i64") + 1;
        if migration.version != expected {
            return Err(StoreError::InvalidMigrationPlan(
                "embedded migrations must be contiguous and start at version 1",
            ));
        }
    }
    Ok(())
}

fn migrate(connection: &mut Connection, migrations: &[Migration]) -> Result<(), StoreError> {
    // BEGIN EXCLUSIVE obtains SQLite's database write lock, serializing migrations
    // across processes. In WAL mode readers may continue, but no competing writer can.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
    verify_application_id(&transaction)?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL,
            checksum TEXT NOT NULL
        ) STRICT;",
    )?;

    let supported = migrations.last().map_or(0, |migration| migration.version);
    let database = verify_migration_ledger(&transaction, migrations, supported)?;
    for migration in migrations.iter().filter(|item| item.version > database) {
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at, checksum)
             VALUES (?1, unixepoch(), ?2)",
            params![migration.version, checksum(migration.sql)],
        )?;
    }
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.commit()?;
    Ok(())
}

fn current_version(connection: &Connection) -> Result<Option<i64>, rusqlite::Error> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_schema
            WHERE type = 'table' AND name = 'schema_migrations'
        )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    connection.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
        row.get(0)
    })
}

fn verify_migration_ledger(
    transaction: &Transaction<'_>,
    migrations: &[Migration],
    supported: i64,
) -> Result<i64, StoreError> {
    let mut statement = transaction
        .prepare("SELECT version, checksum FROM schema_migrations ORDER BY version ASC")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut applied = 0;
    for row in rows {
        let (version, recorded_checksum) = row?;
        let expected = applied + 1;
        if version > supported {
            return Err(StoreError::FutureSchema {
                database: version,
                supported,
            });
        }
        if version != expected {
            return Err(StoreError::InvalidMigrationLedger {
                expected,
                actual: version,
            });
        }
        let migration = &migrations[usize::try_from(version - 1).map_err(|_| {
            StoreError::InvalidMigrationLedger {
                expected,
                actual: version,
            }
        })?];
        if recorded_checksum != checksum(migration.sql) {
            return Err(StoreError::MigrationChecksumMismatch { version });
        }
        applied = version;
    }
    Ok(applied)
}

fn checksum(sql: &str) -> String {
    format!("{:x}", Sha256::digest(sql.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OpenFlags;
    use tempfile::TempDir;

    const LEGACY_V1_SQL: &str = include_str!("../fixtures/legacy_v1.sql");
    const LEGACY_V1_CHECKSUM: &str =
        "65535f435735bd104f52a936deb89f30aa1f741a9198349963a734d676982abd";

    fn database_path(directory: &TempDir) -> std::path::PathBuf {
        directory.path().join("psst.sqlite3")
    }

    #[test]
    fn new_file_is_configured_and_migrated() {
        let directory = TempDir::new().unwrap();
        let store = Store::open(database_path(&directory)).unwrap();

        assert_eq!(store.schema_version().unwrap(), 4);
        assert_eq!(pragma_i64(&store.connection, "foreign_keys"), 1);
        assert_eq!(pragma_string(&store.connection, "journal_mode"), "wal");
        assert_eq!(pragma_i64(&store.connection, "synchronous"), 2);
        assert_eq!(pragma_i64(&store.connection, "busy_timeout"), 2_000);
        assert_eq!(pragma_i64(&store.connection, "wal_autocheckpoint"), 1_000);
        assert_eq!(pragma_i64(&store.connection, "trusted_schema"), 0);
        assert_eq!(
            pragma_i64(&store.connection, "application_id"),
            i64::from(APPLICATION_ID)
        );

        let strict_tables: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_list
                 WHERE name IN ('squads', 'agents', 'memberships', 'instances', 'messages', 'schema_migrations')
                   AND strict = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(strict_tables, 6);
    }

    #[test]
    fn upgrades_a_real_historical_file() {
        let directory = TempDir::new().unwrap();
        let path = database_path(&directory);
        assert_eq!(checksum(LEGACY_V1_SQL), LEGACY_V1_CHECKSUM);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "{LEGACY_V1_SQL}
                 CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY,
                     applied_at INTEGER NOT NULL,
                     checksum TEXT NOT NULL
                 ) STRICT;
                 INSERT INTO schema_migrations(version, applied_at, checksum)
                     VALUES (1, 1, '{LEGACY_V1_CHECKSUM}');
                 PRAGMA application_id = {APPLICATION_ID};"
            ))
            .unwrap();
        drop(connection);

        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 4);
        let index_exists: bool = store
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = 'messages_inbox')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(index_exists);
    }

    #[test]
    fn refuses_a_tampered_checksum() {
        let directory = TempDir::new().unwrap();
        let path = database_path(&directory);
        let store = Store::open(&path).unwrap();
        store
            .connection
            .execute(
                "UPDATE schema_migrations SET checksum = 'tampered' WHERE version = 1",
                [],
            )
            .unwrap();
        drop(store);

        assert!(matches!(
            Store::open(&path),
            Err(StoreError::MigrationChecksumMismatch { version: 1 })
        ));
    }

    #[test]
    fn refuses_a_future_schema() {
        let directory = TempDir::new().unwrap();
        let path = database_path(&directory);
        let store = Store::open(&path).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO schema_migrations(version, applied_at, checksum) VALUES (99, unixepoch(), 'future')",
                [],
            )
            .unwrap();
        drop(store);

        assert!(matches!(
            Store::open(&path),
            Err(StoreError::FutureSchema {
                database: 99,
                supported: 4
            })
        ));
    }

    #[test]
    fn refuses_nonpositive_and_gapped_migration_ledgers() {
        for unexpected in [-1_i64, 0] {
            let directory = TempDir::new().unwrap();
            let path = database_path(&directory);
            let store = Store::open(&path).unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO schema_migrations(version, applied_at, checksum)
                     VALUES (?1, 1, 'unexpected')",
                    [unexpected],
                )
                .unwrap();
            drop(store);

            assert!(matches!(
                Store::open(&path),
                Err(StoreError::InvalidMigrationLedger {
                    expected: 1,
                    actual
                }) if actual == unexpected
            ));
        }

        let directory = TempDir::new().unwrap();
        let path = database_path(&directory);
        let store = Store::open(&path).unwrap();
        store
            .connection
            .execute("DELETE FROM schema_migrations WHERE version = 1", [])
            .unwrap();
        drop(store);
        assert!(matches!(
            Store::open(&path),
            Err(StoreError::InvalidMigrationLedger {
                expected: 1,
                actual: 2
            })
        ));
    }

    #[test]
    fn refuses_a_database_owned_by_another_application_without_mutating_it() {
        let directory = TempDir::new().unwrap();
        let path = database_path(&directory);
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "application_id", 123_456_i64)
            .unwrap();
        drop(connection);

        assert!(matches!(
            Store::open(&path),
            Err(StoreError::UnexpectedApplicationId { actual: 123_456 })
        ));
        let connection = Connection::open(&path).unwrap();
        assert_eq!(pragma_string(&connection, "journal_mode"), "delete");
        let has_migration_table: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'schema_migrations')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!has_migration_table);
    }

    #[test]
    fn failed_migration_rolls_back_all_its_changes() {
        let directory = TempDir::new().unwrap();
        let path = database_path(&directory);
        let broken = [
            Migration {
                version: 1,
                sql: "CREATE TABLE stable(id INTEGER PRIMARY KEY) STRICT;",
            },
            Migration {
                version: 2,
                sql: "CREATE TABLE partial(id INTEGER PRIMARY KEY) STRICT; SELECT * FROM missing;",
            },
        ];

        assert!(Store::open_with_migrations(&path, &broken).is_err());
        let connection =
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let created: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('stable', 'partial', 'schema_migrations')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(created, 0);
    }

    #[test]
    fn rows_survive_close_and_reopen() {
        let directory = TempDir::new().unwrap();
        let path = database_path(&directory);
        let store = Store::open(&path).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO agents(id, created_at) VALUES ('agent-1', 100)",
                [],
            )
            .unwrap();
        drop(store);

        let store = Store::open(&path).unwrap();
        let created_at: i64 = store
            .connection
            .query_row(
                "SELECT created_at FROM agents WHERE id = 'agent-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(created_at, 100);
    }

    #[test]
    fn foreign_keys_and_partial_uniqueness_are_enforced() {
        let directory = TempDir::new().unwrap();
        let store = Store::open(database_path(&directory)).unwrap();
        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO memberships(
                    id, squad_id, agent_id, name, normalized_name, role, joined_at
                 ) VALUES ('member-1', 'missing', 'missing', 'alice', 'alice', 'worker', 1)",
                    [],
                )
                .is_err()
        );

        store
            .connection
            .execute_batch(
                "INSERT INTO squads(id, name, mission, state, created_at)
                 VALUES ('squad-1', 'squad', 'mission', 'active', 1);
             INSERT INTO agents(id, created_at) VALUES ('agent-1', 1), ('agent-2', 1);
             INSERT INTO memberships(id, squad_id, agent_id, name, normalized_name, role, joined_at)
                 VALUES ('member-1', 'squad-1', 'agent-1', 'alice', 'alice', 'worker', 1);",
            )
            .unwrap();
        assert!(store
            .connection
            .execute(
                "INSERT INTO memberships(id, squad_id, agent_id, name, normalized_name, role, joined_at)
                 VALUES ('member-2', 'squad-1', 'agent-2', 'alice', 'alice', 'reviewer', 1)",
                [],
            )
            .is_err());
    }

    #[test]
    fn cross_squad_sender_recipient_and_reply_references_are_rejected() {
        let directory = TempDir::new().unwrap();
        let store = Store::open(database_path(&directory)).unwrap();
        seed_two_squads(&store.connection);
        insert_message(
            &store.connection,
            "parent-2",
            "squad-2",
            "member-3",
            "member-4",
            None,
            None,
        )
        .unwrap();

        assert!(
            insert_message(
                &store.connection,
                "bad-sender",
                "squad-1",
                "member-3",
                "member-2",
                None,
                None,
            )
            .is_err()
        );
        assert!(
            insert_message(
                &store.connection,
                "bad-recipient",
                "squad-1",
                "member-1",
                "member-4",
                None,
                None,
            )
            .is_err()
        );
        assert!(
            insert_message(
                &store.connection,
                "bad-reply",
                "squad-1",
                "member-1",
                "member-2",
                Some("parent-2"),
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn schema_check_constraints_reject_invalid_values() {
        let directory = TempDir::new().unwrap();
        let store = Store::open(database_path(&directory)).unwrap();
        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO squads(id, name, mission, state, created_at)
                 VALUES ('empty-mission', 'empty-mission', '', 'active', 1)",
                    [],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO squads(id, name, mission, state, created_at)
                 VALUES ('bad-state', 'bad-state', 'mission', 'unknown', 1)",
                    [],
                )
                .is_err()
        );
        seed_two_squads(&store.connection);

        for (id, body, priority) in [
            ("empty-body", String::new(), "normal"),
            ("large-body", "x".repeat(65_537), "normal"),
            ("bad-priority", "body".to_owned(), "urgent"),
        ] {
            assert!(
                store
                    .connection
                    .execute(
                        "INSERT INTO messages(
                         id, squad_id, sender_membership_id, recipient_membership_id,
                         body, body_hash, priority, created_at
                     ) VALUES (?1, 'squad-1', 'member-1', 'member-2', ?2, zeroblob(32), ?3, 1)",
                        params![id, body, priority],
                    )
                    .is_err()
            );
        }

        for statement in [
            "INSERT INTO instances(
                 id, membership_id, mode, client_kind, resume_token_hash, availability,
                 availability_source, availability_observed_at, lease_expires_at,
                 last_seen_at, created_at
             ) VALUES ('bad-mode', 'member-1', 'prompt', 'test', zeroblob(32),
                 'unknown', 'adapter', 1, 2, 1, 1)",
            "INSERT INTO instances(
                 id, membership_id, mode, client_kind, resume_token_hash, availability,
                 availability_source, availability_observed_at, lease_expires_at,
                 last_seen_at, created_at
             ) VALUES ('bad-token', 'member-1', 'cooperative', 'test', zeroblob(31),
                 'unknown', 'adapter', 1, 2, 1, 1)",
            "INSERT INTO instances(
                 id, membership_id, mode, client_kind, resume_token_hash, availability,
                 availability_source, availability_observed_at, lease_expires_at,
                 last_seen_at, created_at
             ) VALUES ('bad-availability', 'member-1', 'cooperative', 'test', zeroblob(32),
                 'ready', 'adapter', 1, 2, 1, 1)",
        ] {
            assert!(store.connection.execute(statement, []).is_err());
        }
    }

    #[test]
    fn message_dedupe_is_partial_and_scoped_to_squad_and_sender() {
        let directory = TempDir::new().unwrap();
        let store = Store::open(database_path(&directory)).unwrap();
        seed_two_squads(&store.connection);
        insert_message(
            &store.connection,
            "message-1",
            "squad-1",
            "member-1",
            "member-2",
            None,
            Some("retry-key"),
        )
        .unwrap();
        assert!(
            insert_message(
                &store.connection,
                "message-2",
                "squad-1",
                "member-1",
                "member-2",
                None,
                Some("retry-key"),
            )
            .is_err()
        );

        insert_message(
            &store.connection,
            "message-3",
            "squad-1",
            "member-1",
            "member-2",
            None,
            None,
        )
        .unwrap();
        insert_message(
            &store.connection,
            "message-4",
            "squad-1",
            "member-1",
            "member-2",
            None,
            None,
        )
        .unwrap();
        insert_message(
            &store.connection,
            "message-5",
            "squad-1",
            "member-2",
            "member-1",
            None,
            Some("retry-key"),
        )
        .unwrap();
    }

    #[test]
    fn required_named_index_inventory_is_exact() {
        let directory = TempDir::new().unwrap();
        let store = Store::open(database_path(&directory)).unwrap();
        let mut statement = store
            .connection
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'index' AND name NOT LIKE 'sqlite_autoindex_%'
                 ORDER BY name",
            )
            .unwrap();
        let names: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            names,
            [
                "instances_lease_expiry",
                "instances_unclosed_owner",
                "memberships_active_name",
                "memberships_roster",
                "messages_dedupe",
                "messages_inbox",
                "messages_inbox_order",
            ]
        );
    }

    #[test]
    fn concurrent_openers_serialize_migration_through_sqlite() {
        for round in 0..4 {
            let directory = TempDir::new().unwrap();
            let path = database_path(&directory);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let openers: Vec<_> = (0..8)
                .map(|_| {
                    let path = path.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        Store::open(path).and_then(|store| store.schema_version())
                    })
                })
                .collect();
            for opener in openers {
                assert_eq!(opener.join().unwrap().unwrap(), 4, "round {round}");
            }
        }
    }

    fn seed_two_squads(connection: &Connection) {
        connection
            .execute_batch(
                "INSERT INTO squads(id, name, mission, state, created_at) VALUES
                     ('squad-1', 'squad-1', 'first', 'active', 1),
                     ('squad-2', 'squad-2', 'second', 'active', 1);
                 INSERT INTO agents(id, created_at) VALUES
                     ('agent-1', 1), ('agent-2', 1), ('agent-3', 1), ('agent-4', 1);
                 INSERT INTO memberships(
                     id, squad_id, agent_id, name, normalized_name, role, joined_at
                 ) VALUES
                     ('member-1', 'squad-1', 'agent-1', 'one', 'one', 'worker', 1),
                     ('member-2', 'squad-1', 'agent-2', 'two', 'two', 'worker', 1),
                     ('member-3', 'squad-2', 'agent-3', 'three', 'three', 'worker', 1),
                     ('member-4', 'squad-2', 'agent-4', 'four', 'four', 'worker', 1);",
            )
            .unwrap();
    }

    fn insert_message(
        connection: &Connection,
        id: &str,
        squad_id: &str,
        sender: &str,
        recipient: &str,
        reply_to: Option<&str>,
        dedupe_key: Option<&str>,
    ) -> Result<usize, rusqlite::Error> {
        connection.execute(
            "INSERT INTO messages(
                 id, squad_id, sender_membership_id, recipient_membership_id, body,
                 body_hash, priority, reply_to, dedupe_key, created_at
             ) VALUES (?1, ?2, ?3, ?4, 'body', zeroblob(32), 'normal', ?5, ?6, 1)",
            params![id, squad_id, sender, recipient, reply_to, dedupe_key],
        )
    }

    fn pragma_i64(connection: &Connection, name: &str) -> i64 {
        connection
            .pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }

    fn pragma_string(connection: &Connection, name: &str) -> String {
        connection
            .pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }
}
