// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! SQLite persistence layer for threads and runs.
//!
//! Provides durable storage so that thread state and run history survive
//! server restarts. Uses a single SQLite file in the Foundry Local data
//! directory.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::acp_types::{Message, ThreadStatus};

/// Thread-safe SQLite connection wrapper.
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Open (or create) the database at the given path.
    pub fn open(path: &PathBuf) -> Result<Arc<Self>, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let db = Arc::new(Self {
            conn: Mutex::new(conn),
        });
        tokio::task::block_in_place(|| {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(db.migrate())
        })?;
        Ok(db)
    }

    /// Open an in-memory database (for testing).
    pub fn open_memory() -> Result<Arc<Self>, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        let db = Arc::new(Self {
            conn: Mutex::new(conn),
        });
        tokio::task::block_in_place(|| {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(db.migrate())
        })?;
        Ok(db)
    }

    /// Create tables if they don't exist.
    async fn migrate(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().await;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS threads (
                thread_id TEXT PRIMARY KEY,
                status TEXT NOT NULL DEFAULT 'idle',
                metadata TEXT DEFAULT '{}',
                messages TEXT DEFAULT '[]',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runs (
                run_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                thread_id TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                input TEXT,
                output TEXT,
                config TEXT,
                metadata TEXT DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_runs_thread ON runs(thread_id);
            CREATE INDEX IF NOT EXISTS idx_runs_agent ON runs(agent_id);
            CREATE INDEX IF NOT EXISTS idx_runs_status ON runs(status);
            ",
        )?;
        Ok(())
    }

    // ── Thread operations ────────────────────────────────────────────────────

    /// Insert a new thread. Returns false if it already exists.
    pub async fn insert_thread(
        &self,
        thread_id: &str,
        metadata: &Value,
        now: &DateTime<Utc>,
    ) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().await;
        let ts = now.to_rfc3339();
        let meta_str = serde_json::to_string(metadata).unwrap_or_default();
        let result = conn.execute(
            "INSERT OR IGNORE INTO threads (thread_id, status, metadata, messages, created_at, updated_at) VALUES (?1, 'idle', ?2, '[]', ?3, ?3)",
            params![thread_id, meta_str, ts],
        )?;
        Ok(result > 0)
    }

    /// Get a thread by ID.
    pub async fn get_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<StoredThread>, rusqlite::Error> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT thread_id, status, metadata, messages, created_at, updated_at FROM threads WHERE thread_id = ?1",
            params![thread_id],
            |row| {
                Ok(StoredThread {
                    thread_id: row.get(0)?,
                    status: row.get(1)?,
                    metadata: row.get::<_, String>(2)?,
                    messages: row.get::<_, String>(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )
        .optional()
    }

    /// Update thread metadata (merge with existing).
    pub async fn update_thread_metadata(
        &self,
        thread_id: &str,
        metadata: &Value,
        now: &DateTime<Utc>,
    ) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().await;
        let ts = now.to_rfc3339();

        // Read existing metadata and merge
        let existing: Option<String> = conn
            .query_row(
                "SELECT metadata FROM threads WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .optional()?;

        let Some(existing_str) = existing else {
            return Ok(false);
        };

        let mut merged: Value = serde_json::from_str(&existing_str).unwrap_or(Value::Object(Default::default()));
        if let (Some(base), Some(patch)) = (merged.as_object_mut(), metadata.as_object()) {
            for (k, v) in patch {
                base.insert(k.clone(), v.clone());
            }
        }

        let merged_str = serde_json::to_string(&merged).unwrap_or_default();
        conn.execute(
            "UPDATE threads SET metadata = ?1, updated_at = ?2 WHERE thread_id = ?3",
            params![merged_str, ts, thread_id],
        )?;
        Ok(true)
    }

    /// Update thread status.
    pub async fn update_thread_status(
        &self,
        thread_id: &str,
        status: ThreadStatus,
        now: &DateTime<Utc>,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().await;
        let ts = now.to_rfc3339();
        let status_str = serde_json::to_string(&status)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        conn.execute(
            "UPDATE threads SET status = ?1, updated_at = ?2 WHERE thread_id = ?3",
            params![status_str, ts, thread_id],
        )?;
        Ok(())
    }

    /// Append messages to a thread.
    pub async fn append_messages(
        &self,
        thread_id: &str,
        new_messages: &[Message],
        now: &DateTime<Utc>,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().await;
        let ts = now.to_rfc3339();

        let existing: String = conn
            .query_row(
                "SELECT messages FROM threads WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| "[]".to_string());

        let mut msgs: Vec<Message> =
            serde_json::from_str(&existing).unwrap_or_default();
        msgs.extend(new_messages.iter().cloned());

        let msgs_str = serde_json::to_string(&msgs).unwrap_or_default();
        conn.execute(
            "UPDATE threads SET messages = ?1, updated_at = ?2 WHERE thread_id = ?3",
            params![msgs_str, ts, thread_id],
        )?;
        Ok(())
    }

    /// Delete a thread.
    pub async fn delete_thread(&self, thread_id: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().await;
        let deleted = conn.execute(
            "DELETE FROM threads WHERE thread_id = ?1",
            params![thread_id],
        )?;
        // Also delete associated runs
        conn.execute(
            "DELETE FROM runs WHERE thread_id = ?1",
            params![thread_id],
        )?;
        Ok(deleted > 0)
    }

    /// Search threads by status.
    pub async fn search_threads(
        &self,
        status: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredThread>, rusqlite::Error> {
        let conn = self.conn.lock().await;
        let mut results = Vec::new();

        if let Some(status) = status {
            let mut stmt = conn.prepare(
                "SELECT thread_id, status, metadata, messages, created_at, updated_at FROM threads WHERE status = ?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3",
            )?;
            let rows = stmt.query_map(params![status, limit, offset], |row| {
                Ok(StoredThread {
                    thread_id: row.get(0)?,
                    status: row.get(1)?,
                    metadata: row.get(2)?,
                    messages: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?;
            for row in rows {
                results.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT thread_id, status, metadata, messages, created_at, updated_at FROM threads ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
            )?;
            let rows = stmt.query_map(params![limit, offset], |row| {
                Ok(StoredThread {
                    thread_id: row.get(0)?,
                    status: row.get(1)?,
                    metadata: row.get(2)?,
                    messages: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?;
            for row in rows {
                results.push(row?);
            }
        }

        Ok(results)
    }

    // ── Run operations ───────────────────────────────────────────────────────

    /// Insert a new run.
    pub async fn insert_run(
        &self,
        run_id: &str,
        agent_id: &str,
        thread_id: Option<&str>,
        input: Option<&Value>,
        config: Option<&Value>,
        now: &DateTime<Utc>,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().await;
        let ts = now.to_rfc3339();
        let input_str = input.map(|v| serde_json::to_string(v).unwrap_or_default());
        let config_str = config.map(|v| serde_json::to_string(v).unwrap_or_default());
        conn.execute(
            "INSERT INTO runs (run_id, agent_id, thread_id, status, input, config, created_at, updated_at) VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6, ?6)",
            params![run_id, agent_id, thread_id, input_str, config_str, ts],
        )?;
        Ok(())
    }

    /// Update run status and optionally set output.
    pub async fn update_run(
        &self,
        run_id: &str,
        status: &str,
        output: Option<&Value>,
        now: &DateTime<Utc>,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().await;
        let ts = now.to_rfc3339();
        let output_str = output.map(|v| serde_json::to_string(v).unwrap_or_default());
        conn.execute(
            "UPDATE runs SET status = ?1, output = ?2, updated_at = ?3 WHERE run_id = ?4",
            params![status, output_str, ts, run_id],
        )?;
        Ok(())
    }

    /// Delete a run.
    pub async fn delete_run(&self, run_id: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().await;
        let deleted = conn.execute("DELETE FROM runs WHERE run_id = ?1", params![run_id])?;
        Ok(deleted > 0)
    }

    /// Get the default database path.
    pub fn default_path() -> PathBuf {
        let base = dirs_next::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("FoundryLocal").join("foundry-acp.db")
    }
}

/// A thread as stored in SQLite.
#[derive(Debug, Clone)]
pub struct StoredThread {
    pub thread_id: String,
    pub status: String,
    pub metadata: String,
    pub messages: String,
    pub created_at: String,
    pub updated_at: String,
}
