//! # DeepOmni State
//!
//! SQLite-backed durable persistence for threads, turns, events, tool calls,
//! and plugin state. Includes a migration system.
//!
//! Adapted from DeepSeek-TUI state patterns.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
// Re-export protocol types used in stored records.
use deepomni_journal::{JournalEntry, JournalError, JournalRecord, JournalSubscriber, TurnJournal};
use deepomni_protocol::{EventFrame, TurnItem};

/// Default state database path: `~/.deepomni/state.db`.
pub fn default_state_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".deepomni")
        .join("state.db")
}

/// A stored thread record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadRecord {
    pub id: String,
    pub preview: String,
    pub ephemeral: bool,
    pub model_provider: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub status: String,
    pub path: Option<PathBuf>,
    pub cwd: String,
    pub cli_version: String,
    pub source: String,
    pub name: Option<String>,
    pub sandbox_policy: Option<String>,
    pub approval_mode: Option<String>,
    pub archived: bool,
    pub archived_at: Option<i64>,
    pub parent_thread_id: Option<String>,
}

/// A stored turn record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub id: String,
    pub thread_id: String,
    pub status: String,
    pub user_input: String,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub parent_turn_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub subagent_id: Option<String>,
}

/// A stored event record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: i64,
    pub thread_id: String,
    pub turn_id: String,
    pub seq: i64,
    pub event_json: String,
    pub created_at: i64,
}

/// A stored tool call record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub tool_name: String,
    pub payload_json: String,
    pub output_json: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

/// A durable pending approval record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApprovalRecord {
    pub approval_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub model: String,
    pub workspace: String,
    pub config_json: String,
    pub status: String,
    pub created_at: i64,
}

/// Plugin state blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStateRecord {
    pub plugin_id: String,
    pub state_json: String,
    pub updated_at: i64,
}

/// The SQLite-backed state store.
#[derive(Debug, Clone)]
pub struct StateStore {
    db_path: PathBuf,
    journal_senders: Arc<Mutex<HashMap<String, tokio::sync::broadcast::Sender<JournalRecord>>>>,
}

impl StateStore {
    /// Open (or create) the state database.
    pub fn open(path: Option<PathBuf>) -> Result<Self, StateError> {
        let db_path = path.unwrap_or_else(default_state_db_path);
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|e| StateError::OpenError {
                path: db_path.clone(),
                message: e.to_string(),
            })?;
        }
        let store = Self {
            db_path,
            journal_senders: Arc::new(Mutex::new(HashMap::new())),
        };
        store.init_schema()?;
        store.run_migrations()?;
        Ok(store)
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn conn(&self) -> Result<Connection, StateError> {
        Connection::open(&self.db_path).map_err(|e| StateError::OpenError {
            path: self.db_path.clone(),
            message: e.to_string(),
        })
    }

    /// Create the base schema.
    fn init_schema(&self) -> Result<(), StateError> {
        let conn = self.conn()?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS threads (
                id TEXT PRIMARY KEY,
                preview TEXT NOT NULL DEFAULT '',
                ephemeral INTEGER NOT NULL DEFAULT 0,
                model_provider TEXT NOT NULL DEFAULT 'deepseek',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'idle',
                path TEXT,
                cwd TEXT NOT NULL DEFAULT '.',
                cli_version TEXT NOT NULL DEFAULT '0.1.0',
                source TEXT NOT NULL DEFAULT 'interactive',
                name TEXT,
                sandbox_policy TEXT,
                approval_mode TEXT,
                archived INTEGER NOT NULL DEFAULT 0,
                archived_at INTEGER,
                parent_thread_id TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_threads_updated_at ON threads(updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_threads_archived ON threads(archived, updated_at DESC);

            CREATE TABLE IF NOT EXISTS turns (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'started',
                user_input TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                completed_at INTEGER,
                model TEXT,
                model_provider TEXT,
                parent_turn_id TEXT,
                parent_thread_id TEXT,
                subagent_id TEXT,
                FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_turns_thread ON turns(thread_id, created_at ASC);

            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                event_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_events_thread_seq ON events(thread_id, seq ASC);
            CREATE INDEX IF NOT EXISTS idx_events_turn ON events(turn_id, seq ASC);

            CREATE TABLE IF NOT EXISTS tool_calls (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                output_json TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at INTEGER NOT NULL,
                completed_at INTEGER,
                FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_tool_calls_turn ON tool_calls(turn_id, created_at ASC);

            CREATE TABLE IF NOT EXISTS turn_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                item_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_turn_items_turn ON turn_items(turn_id, seq ASC);

            CREATE TABLE IF NOT EXISTS journal_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                entry_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE,
                UNIQUE(thread_id, seq)
            );
            CREATE INDEX IF NOT EXISTS idx_journal_entries_thread_seq ON journal_entries(thread_id, seq ASC);
            CREATE INDEX IF NOT EXISTS idx_journal_entries_turn ON journal_entries(turn_id, seq ASC);

            CREATE TABLE IF NOT EXISTS pending_approvals (
                approval_id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                call_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                arguments_json TEXT NOT NULL,
                model TEXT NOT NULL,
                workspace TEXT NOT NULL,
                config_json TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at INTEGER NOT NULL,
                FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS plugin_state (
                plugin_id TEXT PRIMARY KEY,
                state_json TEXT NOT NULL DEFAULT '{}',
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );
            "#,
        )
        .map_err(|e| StateError::SchemaError {
            message: format!("failed to initialize schema: {e}"),
        })?;
        Ok(())
    }

    /// Run pending migrations.
    fn run_migrations(&self) -> Result<(), StateError> {
        let conn = self.conn()?;
        let current: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Migration 1: initial version marker
        if current < 1 {
            conn.execute(
                "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
                params![1, current_timestamp()],
            )
            .map_err(|e| StateError::MigrationError {
                version: 1,
                source: e,
            })?;
        }

        Ok(())
    }

    // ── Thread operations ──

    pub fn upsert_thread(&self, thread: &ThreadRecord) -> Result<(), StateError> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            INSERT INTO threads (
                id, preview, ephemeral, model_provider, created_at, updated_at, status, path, cwd,
                cli_version, source, name, sandbox_policy, approval_mode, archived, archived_at,
                parent_thread_id
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17
            )
            ON CONFLICT(id) DO UPDATE SET
                preview=excluded.preview,
                ephemeral=excluded.ephemeral,
                model_provider=excluded.model_provider,
                updated_at=excluded.updated_at,
                status=excluded.status,
                path=excluded.path,
                cwd=excluded.cwd,
                name=excluded.name,
                sandbox_policy=excluded.sandbox_policy,
                approval_mode=excluded.approval_mode,
                archived=excluded.archived,
                archived_at=excluded.archived_at,
                parent_thread_id=excluded.parent_thread_id
            "#,
            params![
                thread.id,
                thread.preview,
                bool_to_i64(thread.ephemeral),
                thread.model_provider,
                thread.created_at,
                thread.updated_at,
                thread.status,
                thread.path.as_ref().map(|p| p.display().to_string()),
                thread.cwd,
                thread.cli_version,
                thread.source,
                thread.name,
                thread.sandbox_policy,
                thread.approval_mode,
                bool_to_i64(thread.archived),
                thread.archived_at,
                thread.parent_thread_id,
            ],
        )
        .map_err(|e| StateError::QueryError {
            message: format!("failed to upsert thread: {e}"),
        })?;
        Ok(())
    }

    pub fn get_thread(&self, thread_id: &str) -> Result<Option<ThreadRecord>, StateError> {
        let conn = self.conn()?;
        conn.query_row(
            r#"
            SELECT id, preview, ephemeral, model_provider, created_at, updated_at, status, path, cwd,
                   cli_version, source, name, sandbox_policy, approval_mode, archived, archived_at,
                   parent_thread_id
            FROM threads WHERE id = ?1
            "#,
            params![thread_id],
            |row| {
                Ok(ThreadRecord {
                    id: row.get(0)?,
                    preview: row.get(1)?,
                    ephemeral: i64_to_bool(row.get(2)?),
                    model_provider: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                    status: row.get(6)?,
                    path: row
                        .get::<_, Option<String>>(7)?
                        .map(PathBuf::from),
                    cwd: row.get(8)?,
                    cli_version: row.get(9)?,
                    source: row.get(10)?,
                    name: row.get(11)?,
                    sandbox_policy: row.get(12)?,
                    approval_mode: row.get(13)?,
                    archived: i64_to_bool(row.get(14)?),
                    archived_at: row.get(15)?,
                    parent_thread_id: row.get(16)?,
                })
            },
        )
        .optional()
        .map_err(|e| StateError::QueryError {
            message: format!("failed to get thread: {e}"),
        })
    }

    pub fn list_threads(
        &self,
        include_archived: bool,
        limit: usize,
    ) -> Result<Vec<ThreadRecord>, StateError> {
        let conn = self.conn()?;
        let where_clause = if !include_archived {
            "WHERE archived = 0"
        } else {
            ""
        };
        let sql = format!(
            r#"
            SELECT id, preview, ephemeral, model_provider, created_at, updated_at, status, path, cwd,
                   cli_version, source, name, sandbox_policy, approval_mode, archived, archived_at,
                   parent_thread_id
            FROM threads {where_clause}
            ORDER BY updated_at DESC
            LIMIT ?1
            "#
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| StateError::QueryError {
            message: format!("failed to prepare list_threads: {e}"),
        })?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(ThreadRecord {
                    id: row.get(0)?,
                    preview: row.get(1)?,
                    ephemeral: i64_to_bool(row.get(2)?),
                    model_provider: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                    status: row.get(6)?,
                    path: row.get::<_, Option<String>>(7)?.map(PathBuf::from),
                    cwd: row.get(8)?,
                    cli_version: row.get(9)?,
                    source: row.get(10)?,
                    name: row.get(11)?,
                    sandbox_policy: row.get(12)?,
                    approval_mode: row.get(13)?,
                    archived: i64_to_bool(row.get(14)?),
                    archived_at: row.get(15)?,
                    parent_thread_id: row.get(16)?,
                })
            })
            .map_err(|e| StateError::QueryError {
                message: format!("failed to list threads: {e}"),
            })?;
        let mut threads = Vec::new();
        for row in rows {
            threads.push(row.map_err(|e| StateError::QueryError {
                message: format!("failed to read thread row: {e}"),
            })?);
        }
        Ok(threads)
    }

    // ── Turn operations ──

    pub fn insert_turn(&self, turn: &TurnRecord) -> Result<(), StateError> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            INSERT INTO turns (id, thread_id, status, user_input, created_at, completed_at,
                              model, model_provider, parent_turn_id, parent_thread_id, subagent_id)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                turn.id,
                turn.thread_id,
                turn.status,
                turn.user_input,
                turn.created_at,
                turn.completed_at,
                turn.model,
                turn.model_provider,
                turn.parent_turn_id,
                turn.parent_thread_id,
                turn.subagent_id,
            ],
        )
        .map_err(|e| StateError::QueryError {
            message: format!("failed to insert turn: {e}"),
        })?;
        Ok(())
    }

    pub fn update_turn_status(
        &self,
        turn_id: &str,
        status: &str,
        completed_at: Option<i64>,
    ) -> Result<(), StateError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE turns SET status = ?1, completed_at = ?2 WHERE id = ?3",
            params![status, completed_at, turn_id],
        )
        .map_err(|e| StateError::QueryError {
            message: format!("failed to update turn status: {e}"),
        })?;
        Ok(())
    }

    // ── Event operations ──

    pub fn insert_event(
        &self,
        thread_id: &str,
        turn_id: &str,
        seq: i64,
        event: &EventFrame,
    ) -> Result<(), StateError> {
        let conn = self.conn()?;
        let event_json = serde_json::to_string(event).unwrap_or_default();
        conn.execute(
            r#"
            INSERT INTO events (thread_id, turn_id, seq, event_json, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![thread_id, turn_id, seq, event_json, current_timestamp()],
        )
        .map_err(|e| StateError::QueryError {
            message: format!("failed to insert event: {e}"),
        })?;
        Ok(())
    }

    pub fn get_events_since(
        &self,
        thread_id: &str,
        since_seq: i64,
    ) -> Result<Vec<(i64, EventFrame)>, StateError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT seq, event_json FROM events WHERE thread_id = ?1 AND seq > ?2 ORDER BY seq ASC",
            )
            .map_err(|e| StateError::QueryError {
                message: format!("failed to prepare get_events_since: {e}"),
            })?;
        let rows = stmt
            .query_map(params![thread_id, since_seq], |row| {
                let seq: i64 = row.get(0)?;
                let json: String = row.get(1)?;
                Ok((seq, json))
            })
            .map_err(|e| StateError::QueryError {
                message: format!("failed to get events: {e}"),
            })?;
        let mut events = Vec::new();
        for row in rows {
            let (seq, json) = row.map_err(|e| StateError::QueryError {
                message: format!("failed to read event row: {e}"),
            })?;
            if let Ok(event) = serde_json::from_str::<EventFrame>(&json) {
                events.push((seq, event));
            }
        }
        Ok(events)
    }

    // ── Turn item operations ──

    /// Persist a structured turn item for transcript reconstruction.
    pub fn insert_turn_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        item: &TurnItem,
    ) -> Result<i64, StateError> {
        let conn = self.conn()?;
        let item_json = serde_json::to_string(item).unwrap_or_default();
        let max_seq: Option<i64> = conn
            .query_row(
                "SELECT MAX(seq) FROM turn_items WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StateError::QueryError {
                message: format!("failed to get max seq: {e}"),
            })?
            .flatten();
        let seq = max_seq.unwrap_or(0) + 1;

        conn.execute(
            "INSERT INTO turn_items (thread_id, turn_id, seq, item_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![thread_id, turn_id, seq, item_json, current_timestamp()],
        )
        .map_err(|e| StateError::QueryError {
            message: format!("failed to insert turn item: {e}"),
        })?;

        Ok(seq)
    }

    /// Reconstruct the full turn transcript from persisted items.
    pub fn get_turn_items(&self, turn_id: &str) -> Result<Vec<(i64, TurnItem)>, StateError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT seq, item_json FROM turn_items WHERE turn_id = ?1 ORDER BY seq ASC")
            .map_err(|e| StateError::QueryError {
                message: format!("failed to prepare get_turn_items: {e}"),
            })?;
        let rows = stmt
            .query_map(params![turn_id], |row| {
                let seq: i64 = row.get(0)?;
                let json: String = row.get(1)?;
                Ok((seq, json))
            })
            .map_err(|e| StateError::QueryError {
                message: format!("failed to get turn items: {e}"),
            })?;
        let mut items = Vec::new();
        for row in rows {
            let (seq, json) = row.map_err(|e| StateError::QueryError {
                message: format!("failed to read turn item row: {e}"),
            })?;
            if let Ok(item) = serde_json::from_str::<TurnItem>(&json) {
                items.push((seq, item));
            }
        }
        Ok(items)
    }

    /// Replay journal records for a single turn. This is used for approval
    /// resume transcript reconstruction while the public journal trait remains
    /// thread-oriented for SSE replay.
    pub fn get_journal_records_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<Vec<JournalRecord>, StateError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT seq, thread_id, turn_id, entry_json, created_at FROM journal_entries WHERE turn_id = ?1 ORDER BY seq ASC",
            )
            .map_err(|e| StateError::QueryError {
                message: format!("failed to prepare get_journal_records_for_turn: {e}"),
            })?;
        let rows = stmt
            .query_map(params![turn_id], |row| {
                let seq: i64 = row.get(0)?;
                let thread_id: String = row.get(1)?;
                let turn_id: String = row.get(2)?;
                let entry_json: String = row.get(3)?;
                let created_at: i64 = row.get(4)?;
                Ok((seq, thread_id, turn_id, entry_json, created_at))
            })
            .map_err(|e| StateError::QueryError {
                message: format!("failed to get journal records for turn: {e}"),
            })?;

        let mut records = Vec::new();
        for row in rows {
            let (seq, thread_id, turn_id, entry_json, created_at) =
                row.map_err(|e| StateError::QueryError {
                    message: format!("failed to read journal turn row: {e}"),
                })?;
            let entry = serde_json::from_str::<JournalEntry>(&entry_json).map_err(|e| {
                StateError::QueryError {
                    message: format!("failed to deserialize journal entry: {e}"),
                }
            })?;
            records.push(JournalRecord {
                seq,
                thread_id: deepomni_protocol::ThreadId::from_string(&thread_id),
                turn_id: deepomni_protocol::TurnId::from_string(&turn_id),
                entry,
                created_at,
            });
        }
        Ok(records)
    }

    // ── Pending approval operations ──

    /// Persist a pending approval record for later resume.
    pub fn insert_pending_approval(&self, rec: &PendingApprovalRecord) -> Result<(), StateError> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO pending_approvals (approval_id, thread_id, turn_id, call_id, tool_name, arguments_json, model, workspace, config_json, status, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![rec.approval_id, rec.thread_id, rec.turn_id, rec.call_id, rec.tool_name, rec.arguments_json, rec.model, rec.workspace, rec.config_json, rec.status, rec.created_at],
        ).map_err(|e| StateError::QueryError { message: format!("insert pending approval: {e}") })?;
        Ok(())
    }

    /// Update the status of a pending approval.
    pub fn update_pending_approval_status(
        &self,
        approval_id: &str,
        status: &str,
    ) -> Result<(), StateError> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE pending_approvals SET status = ?1 WHERE approval_id = ?2",
            params![status, approval_id],
        )
        .map_err(|e| StateError::QueryError {
            message: format!("update pending approval: {e}"),
        })?;
        Ok(())
    }

    /// Get a pending approval by turn_id.
    pub fn get_pending_approval_by_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Option<PendingApprovalRecord>, StateError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT approval_id, thread_id, turn_id, call_id, tool_name, arguments_json, model, workspace, config_json, status, created_at FROM pending_approvals WHERE thread_id = ?1 AND turn_id = ?2 AND status = 'pending'",
            params![thread_id, turn_id],
            |row| Ok(PendingApprovalRecord {
                approval_id: row.get(0)?,
                thread_id: row.get(1)?,
                turn_id: row.get(2)?,
                call_id: row.get(3)?,
                tool_name: row.get(4)?,
                arguments_json: row.get(5)?,
                model: row.get(6)?,
                workspace: row.get(7)?,
                config_json: row.get(8)?,
                status: row.get(9)?,
                created_at: row.get(10)?,
            }),
        ).optional().map_err(|e| StateError::QueryError { message: format!("get pending approval: {e}") })
    }

    pub fn next_event_seq(&self, thread_id: &str) -> Result<i64, StateError> {
        let conn = self.conn()?;
        let max_seq: Option<i64> = conn
            .query_row(
                "SELECT MAX(seq) FROM events WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StateError::QueryError {
                message: format!("failed to get max seq: {e}"),
            })?
            .flatten();
        Ok(max_seq.unwrap_or(0) + 1)
    }

    /// Atomically compute the next sequence number and insert the event
    /// within a single transaction. Returns the assigned sequence number.
    /// The transaction prevents concurrent appenders from assigning duplicate
    /// sequence numbers.
    pub fn append_event(
        &self,
        thread_id: &str,
        turn_id: &str,
        event: &EventFrame,
    ) -> Result<i64, StateError> {
        let conn = self.conn()?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| StateError::QueryError {
                message: format!("begin tx: {e}"),
            })?;

        let result = (|| {
            let max_seq: Option<i64> = conn
                .query_row(
                    "SELECT MAX(seq) FROM events WHERE thread_id = ?1",
                    params![thread_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| StateError::QueryError {
                    message: format!("failed to get max seq: {e}"),
                })?
                .flatten();
            let seq = max_seq.unwrap_or(0) + 1;

            let event_json = serde_json::to_string(event).unwrap_or_default();
            conn.execute(
                r#"
                INSERT INTO events (thread_id, turn_id, seq, event_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![thread_id, turn_id, seq, event_json, current_timestamp()],
            )
            .map_err(|e| StateError::QueryError {
                message: format!("failed to append event: {e}"),
            })?;

            Ok(seq)
        })();

        match result {
            Ok(seq) => {
                conn.execute("COMMIT", [])
                    .map_err(|e| StateError::QueryError {
                        message: format!("commit tx: {e}"),
                    })?;
                Ok(seq)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }
}

impl TurnJournal for StateStore {
    fn append(
        &self,
        thread_id: &deepomni_protocol::ThreadId,
        turn_id: &deepomni_protocol::TurnId,
        entry: JournalEntry,
    ) -> Result<JournalRecord, JournalError> {
        let conn = self
            .conn()
            .map_err(|e| JournalError::Storage(format!("{e}")))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| JournalError::Storage(format!("begin tx: {e}")))?;

        let result = (|| {
            let max_seq: Option<i64> = conn
                .query_row(
                    "SELECT MAX(seq) FROM journal_entries WHERE thread_id = ?1",
                    params![thread_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| JournalError::Storage(format!("failed to get journal max seq: {e}")))?
                .flatten();
            let seq = max_seq.unwrap_or(0) + 1;
            let created_at = current_timestamp();
            let entry_json = serde_json::to_string(&entry)
                .map_err(|e| JournalError::Serialization(format!("{e}")))?;

            conn.execute(
                r#"
                INSERT INTO journal_entries (thread_id, turn_id, seq, entry_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![
                    thread_id.as_str(),
                    turn_id.as_str(),
                    seq,
                    entry_json,
                    created_at
                ],
            )
            .map_err(|e| JournalError::Storage(format!("failed to append journal entry: {e}")))?;

            match &entry {
                JournalEntry::ApprovalPending {
                    call_id,
                    approval_id,
                    tool_name,
                    arguments,
                    model,
                    workspace,
                    config_json,
                    ..
                } => {
                    let arguments_json = serde_json::to_string(arguments)
                        .map_err(|e| JournalError::Serialization(format!("{e}")))?;
                    conn.execute(
                        "INSERT OR REPLACE INTO pending_approvals (approval_id, thread_id, turn_id, call_id, tool_name, arguments_json, model, workspace, config_json, status, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'pending',?10)",
                        params![
                            approval_id,
                            thread_id.as_str(),
                            turn_id.as_str(),
                            call_id.as_str(),
                            tool_name,
                            arguments_json,
                            model,
                            workspace,
                            config_json,
                            created_at,
                        ],
                    )
                    .map_err(|e| JournalError::Storage(format!("project pending approval: {e}")))?;
                }
                JournalEntry::ApprovalResolved {
                    approval_id,
                    approved,
                } => {
                    let status = if *approved { "approved" } else { "rejected" };
                    conn.execute(
                        "UPDATE pending_approvals SET status = ?1 WHERE approval_id = ?2",
                        params![status, approval_id],
                    )
                    .map_err(|e| {
                        JournalError::Storage(format!("project approval resolution: {e}"))
                    })?;
                }
                _ => {}
            }

            Ok(JournalRecord {
                seq,
                thread_id: thread_id.clone(),
                turn_id: turn_id.clone(),
                entry,
                created_at,
            })
        })();

        match result {
            Ok(record) => {
                conn.execute("COMMIT", [])
                    .map_err(|e| JournalError::Storage(format!("commit tx: {e}")))?;
                if let Ok(senders) = self.journal_senders.lock()
                    && let Some(sender) = senders.get(thread_id.as_str())
                {
                    let _ = sender.send(record.clone());
                }
                Ok(record)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    fn replay(
        &self,
        thread_id: &deepomni_protocol::ThreadId,
        since_seq: i64,
    ) -> Result<Vec<JournalRecord>, JournalError> {
        let conn = self
            .conn()
            .map_err(|e| JournalError::Storage(format!("{e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT seq, turn_id, entry_json, created_at FROM journal_entries WHERE thread_id = ?1 AND seq > ?2 ORDER BY seq ASC",
            )
            .map_err(|e| JournalError::Storage(format!("prepare journal replay: {e}")))?;
        let rows = stmt
            .query_map(params![thread_id.as_str(), since_seq], |row| {
                let seq: i64 = row.get(0)?;
                let turn_id: String = row.get(1)?;
                let entry_json: String = row.get(2)?;
                let created_at: i64 = row.get(3)?;
                Ok((seq, turn_id, entry_json, created_at))
            })
            .map_err(|e| JournalError::Storage(format!("query journal replay: {e}")))?;

        let mut records = Vec::new();
        for row in rows {
            let (seq, turn_id, entry_json, created_at) =
                row.map_err(|e| JournalError::Storage(format!("read journal row: {e}")))?;
            let entry = serde_json::from_str::<JournalEntry>(&entry_json)
                .map_err(|e| JournalError::Serialization(format!("{e}")))?;
            records.push(JournalRecord {
                seq,
                thread_id: thread_id.clone(),
                turn_id: deepomni_protocol::TurnId::from_string(&turn_id),
                entry,
                created_at,
            });
        }
        Ok(records)
    }

    fn subscribe(&self, thread_id: deepomni_protocol::ThreadId) -> JournalSubscriber {
        let sender = {
            let mut senders = self
                .journal_senders
                .lock()
                .expect("journal sender mutex poisoned");
            senders
                .entry(thread_id.to_string())
                .or_insert_with(|| tokio::sync::broadcast::channel(256).0)
                .clone()
        };
        deepomni_journal::subscriber(thread_id.clone(), sender.subscribe())
    }
}

// ── Helpers ──

fn bool_to_i64(b: bool) -> i64 {
    if b { 1 } else { 0 }
}

fn i64_to_bool(i: i64) -> bool {
    i != 0
}

fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ── Error types ──

#[derive(Debug)]
pub enum StateError {
    OpenError {
        path: PathBuf,
        message: String,
    },
    SchemaError {
        message: String,
    },
    MigrationError {
        version: i64,
        source: rusqlite::Error,
    },
    QueryError {
        message: String,
    },
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::OpenError { path, message } => {
                write!(
                    f,
                    "failed to open state db at {}: {message}",
                    path.display()
                )
            }
            StateError::SchemaError { message } => {
                write!(f, "schema error: {message}")
            }
            StateError::MigrationError { version, source } => {
                write!(f, "migration v{version} failed: {source}")
            }
            StateError::QueryError { message } => {
                write!(f, "query error: {message}")
            }
        }
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StateError::MigrationError { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> StateStore {
        let tmp = std::env::temp_dir().join(format!("deepomni-test-{}.db", uuid::Uuid::new_v4()));
        // Clean up after test.
        let path = Some(tmp.clone());
        StateStore::open(path).unwrap()
    }

    #[test]
    fn test_thread_upsert_and_get() {
        let store = test_store();
        let thread = ThreadRecord {
            id: "thread-test-1".into(),
            preview: "test".into(),
            ephemeral: false,
            model_provider: "deepseek".into(),
            created_at: current_timestamp(),
            updated_at: current_timestamp(),
            status: "idle".into(),
            path: None,
            cwd: "/tmp".into(),
            cli_version: "0.1.0".into(),
            source: "interactive".into(),
            name: Some("test thread".into()),
            sandbox_policy: None,
            approval_mode: None,
            archived: false,
            archived_at: None,
            parent_thread_id: None,
        };
        store.upsert_thread(&thread).unwrap();
        let retrieved = store.get_thread("thread-test-1").unwrap().unwrap();
        assert_eq!(retrieved.id, "thread-test-1");
        assert_eq!(retrieved.name.unwrap(), "test thread");
    }

    #[test]
    fn test_event_insert_and_replay() {
        let store = test_store();
        let thread_id = "thread-ev-1";
        let turn_id = "turn-ev-1";

        // Create parent records so FK constraints pass.
        store
            .upsert_thread(&ThreadRecord {
                id: thread_id.to_string(),
                preview: String::new(),
                ephemeral: false,
                model_provider: "deepseek".into(),
                created_at: current_timestamp(),
                updated_at: current_timestamp(),
                status: "idle".into(),
                path: None,
                cwd: ".".into(),
                cli_version: "0.1.0".into(),
                source: "interactive".into(),
                name: None,
                sandbox_policy: None,
                approval_mode: None,
                archived: false,
                archived_at: None,
                parent_thread_id: None,
            })
            .unwrap();
        store
            .insert_turn(&TurnRecord {
                id: turn_id.to_string(),
                thread_id: thread_id.to_string(),
                status: "started".into(),
                user_input: "hello".into(),
                created_at: current_timestamp(),
                completed_at: None,
                model: None,
                model_provider: None,
                parent_turn_id: None,
                parent_thread_id: None,
                subagent_id: None,
            })
            .unwrap();

        let event = EventFrame::TurnStarted {
            thread_id: deepomni_protocol::ThreadId::from_string(thread_id),
            turn_id: deepomni_protocol::TurnId::from_string(turn_id),
            user_input: "hello".into(),
        };

        store.insert_event(thread_id, turn_id, 1, &event).unwrap();

        let events = store.get_events_since(thread_id, 0).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, 1);
    }

    #[test]
    fn test_migration_sets_version() {
        let store = test_store();
        let conn = store.conn().unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(version >= 1);
    }
}
