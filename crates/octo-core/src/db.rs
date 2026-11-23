use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Database lock poisoned")]
    LockPoisoned,
    #[error("Invalid timestamp stored in database: {0}")]
    TimestampParse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerHealth {
    Ready,
    Busy,
    RateLimited,
    Dead,
}

impl PeerHealth {
    pub fn as_str(&self) -> &'static str {
        match self {
            PeerHealth::Ready => "ready",
            PeerHealth::Busy => "busy",
            PeerHealth::RateLimited => "rate_limited",
            PeerHealth::Dead => "dead",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "busy" => PeerHealth::Busy,
            "rate_limited" => PeerHealth::RateLimited,
            "dead" => PeerHealth::Dead,
            _ => PeerHealth::Ready,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerStateRow {
    pub peer_name: String,
    pub health: PeerHealth,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskHistoryRow {
    pub id: String,
    pub caller: String,
    pub target: String,
    pub resolved_peer: String,
    pub depth: u32,
    pub status: String,
    pub prompt_preview: String,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffRecord {
    pub id: i64,
    pub author: String,
    pub task_title: String,
    pub summary: String,
    pub next_steps: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRow {
    pub id: i64,
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub ref_task: Option<String>,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct Blackboard {
    conn: Arc<Mutex<Connection>>,
}

impl Blackboard {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        let board = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        board.init_schema()?;
        Ok(board)
    }

    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        let board = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        board.init_schema()?;
        Ok(board)
    }

    fn init_schema(&self) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS peer_health (
                peer_name TEXT PRIMARY KEY,
                health TEXT NOT NULL,
                cooldown_until TEXT,
                last_error TEXT,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS task_history (
                attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT NOT NULL,
                caller TEXT NOT NULL,
                target TEXT NOT NULL,
                resolved_peer TEXT NOT NULL,
                depth INTEGER NOT NULL,
                status TEXT NOT NULL,
                prompt_preview TEXT NOT NULL,
                error_message TEXT,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS handoffs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                author TEXT NOT NULL,
                task_title TEXT NOT NULL,
                summary TEXT NOT NULL,
                next_steps TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sender TEXT NOT NULL,
                recipient TEXT NOT NULL,
                body TEXT NOT NULL,
                ref_task TEXT,
                created_at TEXT NOT NULL,
                read_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_messages_recipient
                ON messages (recipient, read_at);
            ",
        )?;
        Ok(())
    }

    pub fn ensure_peers(&self, peer_names: &[String]) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let now = Utc::now().to_rfc3339();
        for name in peer_names {
            conn.execute(
                "INSERT OR IGNORE INTO peer_health (peer_name, health, cooldown_until, last_error, updated_at)
                 VALUES (?1, 'ready', NULL, NULL, ?2)",
                params![name, now],
            )?;
        }
        Ok(())
    }

    /// Returns true if the peer is available right now (automatically clears expired cooldowns).
    pub fn is_peer_available(&self, peer_name: &str) -> Result<bool, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let now = Utc::now();
        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT health, cooldown_until FROM peer_health WHERE peer_name = ?1",
                params![peer_name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        match row {
            None => Ok(true),
            Some((health_str, cooldown_opt)) => {
                let health = PeerHealth::from_db(&health_str);
                if let Some(cd_str) = cooldown_opt {
                    let cd_time = DateTime::parse_from_rfc3339(&cd_str)
                        .map_err(|_| DbError::TimestampParse(cd_str.clone()))?
                        .with_timezone(&Utc);
                    if cd_time > now {
                        return Ok(false);
                    }
                    // Cooldown expired: promote peer back to ready
                    conn.execute(
                        "UPDATE peer_health
                         SET health = 'ready', cooldown_until = NULL, updated_at = ?2
                         WHERE peer_name = ?1",
                        params![peer_name, now.to_rfc3339()],
                    )?;
                    return Ok(true);
                }
                Ok(matches!(health, PeerHealth::Ready))
            }
        }
    }

    pub fn set_peer_health(
        &self,
        peer_name: &str,
        health: PeerHealth,
        cooldown_secs: Option<u64>,
        last_error: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let now = Utc::now();
        let cooldown_until =
            cooldown_secs.map(|secs| (now + Duration::seconds(secs as i64)).to_rfc3339());
        conn.execute(
            "INSERT INTO peer_health (peer_name, health, cooldown_until, last_error, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(peer_name) DO UPDATE SET
               health = excluded.health,
               cooldown_until = excluded.cooldown_until,
               last_error = excluded.last_error,
               updated_at = excluded.updated_at",
            params![
                peer_name,
                health.as_str(),
                cooldown_until,
                last_error,
                now.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn reset_peer_health(&self, peer_name: Option<&str>) -> Result<usize, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let now = Utc::now().to_rfc3339();
        let affected = match peer_name {
            Some(name) => conn.execute(
                "UPDATE peer_health
                 SET health = 'ready', cooldown_until = NULL, last_error = NULL, updated_at = ?2
                 WHERE peer_name = ?1",
                params![name, now],
            )?,
            None => conn.execute(
                "UPDATE peer_health
                 SET health = 'ready', cooldown_until = NULL, last_error = NULL, updated_at = ?1",
                params![now],
            )?,
        };
        Ok(affected)
    }

    pub fn list_peer_states(&self) -> Result<Vec<PeerStateRow>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let now = Utc::now();
        let mut stmt = conn.prepare(
            "SELECT peer_name, health, cooldown_until, last_error, updated_at
             FROM peer_health ORDER BY peer_name ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let peer_name: String = r.get(0)?;
            let health_str: String = r.get(1)?;
            let cd_opt: Option<String> = r.get(2)?;
            let last_error: Option<String> = r.get(3)?;
            let updated_str: String = r.get(4)?;
            Ok((peer_name, health_str, cd_opt, last_error, updated_str))
        })?;

        let mut out = Vec::new();
        for item in rows {
            let (peer_name, health_str, cd_opt, last_error, updated_str) = item?;
            let mut health = PeerHealth::from_db(&health_str);
            let mut cooldown_until = None;
            if let Some(cd_str) = cd_opt {
                if let Ok(parsed) = DateTime::parse_from_rfc3339(&cd_str) {
                    let utc_dt = parsed.with_timezone(&Utc);
                    if utc_dt > now {
                        cooldown_until = Some(utc_dt);
                    } else {
                        health = PeerHealth::Ready;
                    }
                }
            }
            let updated_at = DateTime::parse_from_rfc3339(&updated_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or(now);

            out.push(PeerStateRow {
                peer_name,
                health,
                cooldown_until,
                last_error,
                updated_at,
            });
        }
        Ok(out)
    }

    pub fn record_task(
        &self,
        id: &str,
        caller: &str,
        target: &str,
        resolved_peer: &str,
        depth: u32,
        status: &str,
        prompt: &str,
        error_message: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let preview: String = prompt.chars().take(160).collect();
        conn.execute(
            "INSERT INTO task_history
             (id, caller, target, resolved_peer, depth, status, prompt_preview, error_message, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                caller,
                target,
                resolved_peer,
                depth,
                status,
                preview,
                error_message,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn record_handoff(
        &self,
        author: &str,
        task_title: &str,
        summary: &str,
        next_steps: &str,
    ) -> Result<HandoffRecord, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let now = Utc::now();
        conn.execute(
            "INSERT INTO handoffs (author, task_title, summary, next_steps, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![author, task_title, summary, next_steps, now.to_rfc3339()],
        )?;
        let id = conn.last_insert_rowid();
        Ok(HandoffRecord {
            id,
            author: author.to_string(),
            task_title: task_title.to_string(),
            summary: summary.to_string(),
            next_steps: next_steps.to_string(),
            created_at: now,
        })
    }

    pub fn recent_handoffs(&self, limit: usize) -> Result<Vec<HandoffRecord>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut stmt = conn.prepare(
            "SELECT id, author, task_title, summary, next_steps, created_at
             FROM handoffs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            let created_str: String = r.get(5)?;
            let created_at = DateTime::parse_from_rfc3339(&created_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(HandoffRecord {
                id: r.get(0)?,
                author: r.get(1)?,
                task_title: r.get(2)?,
                summary: r.get(3)?,
                next_steps: r.get(4)?,
                created_at,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    pub fn send_message(
        &self,
        sender: &str,
        recipient: &str,
        body: &str,
        ref_task: Option<&str>,
    ) -> Result<MessageRow, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let now = Utc::now();
        conn.execute(
            "INSERT INTO messages (sender, recipient, body, ref_task, created_at, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![sender, recipient, body, ref_task, now.to_rfc3339()],
        )?;
        let id = conn.last_insert_rowid();
        Ok(MessageRow {
            id,
            sender: sender.to_string(),
            recipient: recipient.to_string(),
            body: body.to_string(),
            ref_task: ref_task.map(|s| s.to_string()),
            created_at: now,
            read_at: None,
        })
    }

    /// Messages addressed to `for_peer` (plus broadcasts to 'all'), newest first.
    pub fn list_messages(
        &self,
        for_peer: &str,
        unread_only: bool,
        limit: usize,
    ) -> Result<Vec<MessageRow>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let sql = if unread_only {
            "SELECT id, sender, recipient, body, ref_task, created_at, read_at
             FROM messages
             WHERE (recipient = ?1 OR recipient = 'all') AND read_at IS NULL
             ORDER BY id DESC LIMIT ?2"
        } else {
            "SELECT id, sender, recipient, body, ref_task, created_at, read_at
             FROM messages
             WHERE (recipient = ?1 OR recipient = 'all')
             ORDER BY id DESC LIMIT ?2"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![for_peer, limit as i64], |r| {
            let created_str: String = r.get(5)?;
            let created_at = DateTime::parse_from_rfc3339(&created_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let read_str: Option<String> = r.get(6)?;
            let read_at = read_str
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|d| d.with_timezone(&Utc));
            Ok(MessageRow {
                id: r.get(0)?,
                sender: r.get(1)?,
                recipient: r.get(2)?,
                body: r.get(3)?,
                ref_task: r.get(4)?,
                created_at,
                read_at,
            })
        })?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    pub fn mark_messages_read(&self, for_peer: &str) -> Result<usize, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let now = Utc::now().to_rfc3339();
        let affected = conn.execute(
            "UPDATE messages SET read_at = ?2
             WHERE (recipient = ?1 OR recipient = 'all') AND read_at IS NULL",
            params![for_peer, now],
        )?;
        Ok(affected)
    }

    pub fn unread_count(&self, for_peer: &str) -> Result<i64, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE (recipient = ?1 OR recipient = 'all') AND read_at IS NULL",
            params![for_peer],
            |r| r.get(0),
        )?;
        Ok(count)
    }
}
