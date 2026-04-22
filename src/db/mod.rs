// src/db/mod.rs
//
// Thin SQLite wrapper dùng rusqlite (bundled).
// Wrap tất cả calls trong tokio::task::spawn_blocking để không block async runtime.
//
// Tables:
//   sessions   — bridge session state (replaces in-memory HashMap)
//   jobs       — background job registry
//   events     — audit log (optional, default off)

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

pub type DbPool = Arc<Db>;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Open hoặc create database tại `path`.
    /// Chạy migrations ngay khi mở.
    pub fn open(path: &str) -> anyhow::Result<DbPool> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            // Tạo thư mục nếu chưa có
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(parent)?;
            }
            Connection::open(path)?
        };

        // WAL mode: cải thiện concurrent read/write
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        let db = Arc::new(Self {
            conn: Mutex::new(conn),
        });
        db.migrate()?;
        log::info!("[db] SQLite opened at '{}'", path);
        Ok(db)
    }

    fn migrate(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(include_str!("schema.sql"))?;
        Ok(())
    }
}

// ─── Session operations ───────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub thread_id: Option<String>,
    pub mode: String,
    pub surface: Option<String>,
    pub account_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub metadata_json: Option<String>,
}

impl Db {
    pub fn upsert_session(&self, s: &SessionRecord) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, thread_id, mode, surface, account_id, created_at, updated_at, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
               thread_id   = excluded.thread_id,
               mode        = excluded.mode,
               surface     = excluded.surface,
               account_id  = excluded.account_id,
               updated_at  = excluded.updated_at,
               metadata_json = excluded.metadata_json",
            params![
                s.id, s.thread_id, s.mode, s.surface,
                s.account_id, s.created_at, s.updated_at, s.metadata_json
            ],
        )?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> anyhow::Result<Option<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, thread_id, mode, surface, account_id, created_at, updated_at, metadata_json
             FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SessionRecord {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                mode: row.get(2)?,
                surface: row.get(3)?,
                account_id: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                metadata_json: row.get(7)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn list_sessions(&self, limit: i64) -> anyhow::Result<Vec<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, thread_id, mode, surface, account_id, created_at, updated_at, metadata_json
             FROM sessions ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                mode: row.get(2)?,
                surface: row.get(3)?,
                account_id: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                metadata_json: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Xóa sessions cũ hơn `older_than_secs` giây (eviction)
    pub fn evict_old_sessions(&self, older_than_secs: i64) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = chrono::Utc::now().timestamp() - older_than_secs;
        let n = conn.execute(
            "DELETE FROM sessions WHERE updated_at < ?1",
            params![cutoff],
        )?;
        Ok(n)
    }
}

// ─── Job operations ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JobRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub status: String, // "pending" | "running" | "done" | "failed"
    pub surface: Option<String>,
    pub account_id: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

impl Db {
    pub fn upsert_job(&self, j: &JobRecord) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO jobs (id, session_id, status, surface, account_id, created_at, finished_at, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
               status      = excluded.status,
               account_id  = excluded.account_id,
               finished_at = excluded.finished_at,
               error       = excluded.error",
            params![
                j.id, j.session_id, j.status, j.surface,
                j.account_id, j.created_at, j.finished_at, j.error
            ],
        )?;
        Ok(())
    }

    pub fn list_jobs(&self, limit: i64) -> anyhow::Result<Vec<JobRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, status, surface, account_id, created_at, finished_at, error
             FROM jobs ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(JobRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                status: row.get(2)?,
                surface: row.get(3)?,
                account_id: row.get(4)?,
                created_at: row.get(5)?,
                finished_at: row.get(6)?,
                error: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn session_count(&self) -> i64 {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn job_count_by_status(&self) -> Vec<(String, i64)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT status, COUNT(*) FROM jobs GROUP BY status")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .flatten()
            .collect()
    }
}

// ─── Async wrappers ───────────────────────────────────────────────────────────

/// Wrap sync DB call vào spawn_blocking để không block tokio runtime
#[macro_export]
macro_rules! db_blocking {
    ($db:expr, $f:expr) => {{
        let db = $db.clone();
        tokio::task::spawn_blocking(move || $f(&db))
            .await
            .map_err(|e| anyhow::anyhow!("db task join error: {}", e))?
    }};
}
