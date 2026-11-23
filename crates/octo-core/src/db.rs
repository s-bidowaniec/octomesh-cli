use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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
    pub project: String,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
    pub ack_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceRow {
    pub peer: String,
    pub pid: i64,
    pub host: String,
    pub tty: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub meta: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: String,
    pub kind: String,
    pub from_peer: String,
    pub to_peer: String,
    pub to_window: Option<String>,
    pub project: String,
    pub prompt_preview: String,
    pub status: String,
    pub detail: Option<String>,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub lease_secs: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorCallRow {
    pub id: i64,
    pub connector: String,
    pub project: String,
    pub method: String,
    pub ok: bool,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectUnread {
    pub project: String,
    pub unread: i64,
}

#[derive(Debug, Serialize)]
pub struct InboxPage {
    pub messages: Vec<MessageRow>,
    pub unread: i64,
    pub marked_read: usize,
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
        let mut conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
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

            CREATE TABLE IF NOT EXISTS broadcast_reads (
                message_id INTEGER NOT NULL,
                peer TEXT NOT NULL,
                read_at TEXT NOT NULL,
                PRIMARY KEY (message_id, peer)
            );

            CREATE TABLE IF NOT EXISTS presence (
                peer TEXT PRIMARY KEY,
                pid INTEGER NOT NULL,
                host TEXT NOT NULL,
                tty TEXT,
                started_at TEXT NOT NULL,
                last_seen TEXT NOT NULL,
                meta TEXT
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                from_peer TEXT NOT NULL,
                to_peer TEXT NOT NULL,
                to_window TEXT,
                project TEXT NOT NULL DEFAULT 'default',
                prompt_preview TEXT NOT NULL,
                status TEXT NOT NULL,
                detail TEXT,
                claimed_by TEXT,
                claimed_at TEXT,
                lease_secs INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_tasks_to_peer_status
                ON tasks (to_peer, status, created_at);

            CREATE TABLE IF NOT EXISTS budget_usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT,
                peer TEXT NOT NULL,
                prompt_chars INTEGER NOT NULL,
                est_tokens INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_budget_usage_peer
                ON budget_usage (peer, created_at);

            CREATE TABLE IF NOT EXISTS connector_calls (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                connector TEXT NOT NULL,
                project TEXT NOT NULL,
                method TEXT NOT NULL,
                ok INTEGER NOT NULL,
                error TEXT,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_connector_calls_project
                ON connector_calls (project, created_at);
            ",
        )?;
        let schema_version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema_version == 0 {
            // Draft builds stored broadcasts as 'all' and shared one read flag.
            tx.execute(
                "UPDATE messages SET recipient = '*', read_at = NULL WHERE recipient = 'all'",
                [],
            )?;
            tx.pragma_update(None, "user_version", 1)?;
        }
        if schema_version < 2 {
            // AF-02: project scoping + per-recipient ack on the mailbox.
            for (col, ddl) in [
                ("project", "TEXT NOT NULL DEFAULT 'default'"),
                ("ack_at", "TEXT"),
            ] {
                let stmt = format!("ALTER TABLE messages ADD COLUMN {col} {ddl}");
                if let Err(e) = tx.execute(&stmt, []) {
                    let lower = e.to_string().to_lowercase();
                    if !lower.contains("duplicate column") {
                        return Err(e.into());
                    }
                }
            }
            tx.execute(
                "CREATE INDEX IF NOT EXISTS idx_messages_project_to
                    ON messages (project, recipient, created_at)",
                [],
            )?;
            tx.pragma_update(None, "user_version", 2)?;
        }
        tx.commit()?;
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

    pub fn send_messages(
        &self,
        sender: &str,
        recipients: &[String],
        body: &str,
        ref_task: Option<&str>,
    ) -> Result<Vec<i64>, DbError> {
        self.send_messages_project(sender, recipients, body, ref_task, "default")
    }

    /// Insert one mailbox row per recipient, stamped with `project` (AF-02 isolation).
    pub fn send_messages_project(
        &self,
        sender: &str,
        recipients: &[String],
        body: &str,
        ref_task: Option<&str>,
        project: &str,
    ) -> Result<Vec<i64>, DbError> {
        let mut conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let tx = conn.transaction()?;
        let now = Utc::now().to_rfc3339();
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for recipient in recipients
            .iter()
            .filter(|recipient| seen.insert(*recipient))
        {
            tx.execute(
                "INSERT INTO messages (sender, recipient, body, ref_task, project, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![sender, recipient, body, ref_task, project, now],
            )?;
            ids.push(tx.last_insert_rowid());
        }
        tx.commit()?;
        Ok(ids)
    }

    /// Set `ack_at` (and coalesce `read_at`) on this peer's own rows for the given message ids.
    pub fn ack_messages(&self, peer: &str, ids: &[i64]) -> Result<usize, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let now = Utc::now().to_rfc3339();
        let mut acked = 0usize;
        for id in ids {
            let n = conn.execute(
                "UPDATE messages SET ack_at = ?1, read_at = COALESCE(read_at, ?1)
                 WHERE id = ?2 AND (recipient = ?3 OR recipient = '*')",
                params![now, id, peer],
            )?;
            acked += n;
        }
        Ok(acked)
    }

    /// Unread count for one peer within one project (AF-02).
    pub fn unread_count_project(&self, peer: &str, project: &str) -> Result<i64, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages m
             WHERE m.project = ?2
               AND (m.recipient = ?1 AND m.recipient <> '*' AND m.read_at IS NULL)
                OR (m.project = ?2 AND m.recipient = '*' AND NOT EXISTS (
                    SELECT 1 FROM broadcast_reads b WHERE b.message_id = m.id AND b.peer = ?1
                ))",
            params![peer, project],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Per-project unread counts for a peer (for `octo projects` / mesh_status).
    pub fn list_projects_with_unread(&self, peer: &str) -> Result<Vec<ProjectUnread>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut stmt = conn.prepare(
            "SELECT m.project, COUNT(*) FROM messages m
             WHERE (m.recipient = ?1 AND m.recipient <> '*' AND m.read_at IS NULL)
                OR (m.recipient = '*' AND NOT EXISTS (
                    SELECT 1 FROM broadcast_reads b WHERE b.message_id = m.id AND b.peer = ?1
                ))
             GROUP BY m.project ORDER BY m.project ASC",
        )?;
        let rows = stmt.query_map(params![peer], |r| {
            Ok(ProjectUnread {
                project: r.get(0)?,
                unread: r.get(1)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Fetch the newest messages and optionally mark only this page as read.
    pub fn read_inbox(
        &self,
        for_peer: &str,
        unread_only: bool,
        limit: usize,
        mark_read: bool,
    ) -> Result<InboxPage, DbError> {
        self.read_inbox_project(for_peer, "default", false, unread_only, limit, mark_read)
    }

    /// Project-scoped inbox (AF-02): `all_projects=true` opts into cross-project view.
    pub fn read_inbox_project(
        &self,
        for_peer: &str,
        project: &str,
        all_projects: bool,
        unread_only: bool,
        limit: usize,
        mark_read: bool,
    ) -> Result<InboxPage, DbError> {
        let mut conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let behavior = if mark_read {
            TransactionBehavior::Immediate
        } else {
            TransactionBehavior::Deferred
        };
        let tx = conn.transaction_with_behavior(behavior)?;
        let project_clause = if all_projects {
            ""
        } else {
            "AND m.project = ?4"
        };
        let mut messages = {
            let sql = format!(
                "WITH inbox AS (
                    SELECT m.id, m.sender, m.recipient, m.body, m.ref_task, m.project, m.created_at, m.ack_at,
                           CASE WHEN m.recipient = '*' THEN b.read_at ELSE m.read_at END AS read_at
                    FROM messages m
                    LEFT JOIN broadcast_reads b ON b.message_id = m.id AND b.peer = ?1
                    WHERE m.recipient IN (?1, '*') {project_clause}
                )
                SELECT id, sender, recipient, body, ref_task, project, created_at, ack_at, read_at FROM inbox
                WHERE NOT ?2 OR read_at IS NULL ORDER BY id DESC LIMIT ?3"
            );
            let mut stmt = tx.prepare(&sql)?;
            let limit_i = i64::try_from(limit).unwrap_or(i64::MAX);
            if all_projects {
                stmt.query_map(params![for_peer, unread_only, limit_i], parse_message_row)?
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                stmt.query_map(
                    params![for_peer, unread_only, limit_i, project],
                    parse_message_row,
                )?
                .collect::<Result<Vec<_>, _>>()?
            }
        };
        let mut marked_read = 0;
        if mark_read {
            let now = Utc::now();
            let timestamp = now.to_rfc3339();
            for message in messages
                .iter_mut()
                .filter(|message| message.read_at.is_none())
            {
                if message.recipient == "*" {
                    tx.execute(
                        "INSERT INTO broadcast_reads (message_id, peer, read_at) VALUES (?1, ?2, ?3)",
                        params![message.id, for_peer, timestamp],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE messages SET read_at = ?2 WHERE id = ?1",
                        params![message.id, timestamp],
                    )?;
                }
                message.read_at = Some(now);
                marked_read += 1;
            }
        }
        let unread = if all_projects {
            tx.query_row(
                "SELECT COUNT(*) FROM messages m
                 WHERE (m.recipient = ?1 AND m.recipient <> '*' AND m.read_at IS NULL)
                    OR (m.recipient = '*' AND NOT EXISTS (
                        SELECT 1 FROM broadcast_reads b WHERE b.message_id = m.id AND b.peer = ?1
                    ))",
                params![for_peer],
                |row| row.get(0),
            )?
        } else {
            tx.query_row(
                "SELECT COUNT(*) FROM messages m
                 WHERE m.project = ?2
                   AND ((m.recipient = ?1 AND m.recipient <> '*' AND m.read_at IS NULL)
                    OR (m.recipient = '*' AND NOT EXISTS (
                        SELECT 1 FROM broadcast_reads b WHERE b.message_id = m.id AND b.peer = ?1
                    )))",
                params![for_peer, project],
                |row| row.get(0),
            )?
        };
        tx.commit()?;
        Ok(InboxPage {
            messages,
            unread,
            marked_read,
        })
    }
}

fn parse_message_row(row: &rusqlite::Row) -> rusqlite::Result<MessageRow> {
    let parse_dt = |s: String| {
        DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now())
    };
    let created_str: String = row.get(6)?;
    let ack_str: Option<String> = row.get(7)?;
    let read_str: Option<String> = row.get(8)?;
    Ok(MessageRow {
        id: row.get(0)?,
        sender: row.get(1)?,
        recipient: row.get(2)?,
        body: row.get(3)?,
        ref_task: row.get(4)?,
        project: row.get(5)?,
        created_at: parse_dt(created_str),
        read_at: read_str.map(|s| parse_dt(s)),
        ack_at: ack_str.map(|s| parse_dt(s)),
    })
}

fn parse_task_row(row: &rusqlite::Row) -> rusqlite::Result<TaskRow> {
    let parse_dt = |s: String| {
        DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now())
    };
    let claimed_str: Option<String> = row.get(10)?;
    Ok(TaskRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        from_peer: row.get(2)?,
        to_peer: row.get(3)?,
        to_window: row.get(4)?,
        project: row.get(5)?,
        prompt_preview: row.get(6)?,
        status: row.get(7)?,
        detail: row.get(8)?,
        claimed_by: row.get(9)?,
        claimed_at: claimed_str.map(|s| parse_dt(s)),
        lease_secs: row.get(11)?,
        created_at: parse_dt(row.get(12)?),
        updated_at: parse_dt(row.get(13)?),
    })
}

impl Blackboard {
    /// Mark a peer busy with an automatic recovery TTL (AF-05): the cooldown doubles
    /// as the busy lease, so a crashed caller cannot wedge the peer forever.
    pub fn set_peer_busy(
        &self,
        peer_name: &str,
        busy_ttl_secs: u64,
        last_error: Option<&str>,
    ) -> Result<(), DbError> {
        let ttl = busy_ttl_secs.max(1);
        self.set_peer_health(peer_name, PeerHealth::Busy, Some(ttl), last_error)
    }

    // --- AF-03 presence ---

    pub fn upsert_presence(&self, row: &PresenceRow) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        conn.execute(
            "INSERT INTO presence (peer, pid, host, tty, started_at, last_seen, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(peer) DO UPDATE SET
               pid = excluded.pid,
               host = excluded.host,
               tty = COALESCE(excluded.tty, presence.tty),
               last_seen = excluded.last_seen,
               meta = COALESCE(excluded.meta, presence.meta)",
            params![
                row.peer,
                row.pid,
                row.host,
                row.tty,
                row.started_at.to_rfc3339(),
                row.last_seen.to_rfc3339(),
                row.meta,
            ],
        )?;
        Ok(())
    }

    pub fn list_presence(&self) -> Result<Vec<PresenceRow>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut stmt = conn.prepare(
            "SELECT peer, pid, host, tty, started_at, last_seen, meta
             FROM presence ORDER BY peer ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let parse_dt = |idx: usize| -> rusqlite::Result<DateTime<Utc>> {
                let s: String = r.get(idx)?;
                Ok(DateTime::parse_from_rfc3339(&s)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()))
            };
            Ok(PresenceRow {
                peer: r.get(0)?,
                pid: r.get(1)?,
                host: r.get(2)?,
                tty: r.get(3)?,
                started_at: parse_dt(4)?,
                last_seen: parse_dt(5)?,
                meta: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Delete presence rows not seen since `cutoff`; returns the pruned count.
    pub fn prune_presence_before(&self, cutoff: DateTime<Utc>) -> Result<usize, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let n = conn.execute(
            "DELETE FROM presence WHERE last_seen < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        Ok(n)
    }

    // --- AF-04 / AF-05 tasks ---

    pub fn create_task(&self, task: &TaskRow) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        conn.execute(
            "INSERT INTO tasks
             (id, kind, from_peer, to_peer, to_window, project, prompt_preview, status, detail,
              claimed_by, claimed_at, lease_secs, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                task.id,
                task.kind,
                task.from_peer,
                task.to_peer,
                task.to_window,
                task.project,
                task.prompt_preview,
                task.status,
                task.detail,
                task.claimed_by,
                task.claimed_at.map(|d| d.to_rfc3339()),
                task.lease_secs,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn set_task_status(
        &self,
        task_id: &str,
        status: &str,
        detail: Option<&str>,
    ) -> Result<bool, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let n = conn.execute(
            "UPDATE tasks SET status = ?1, detail = COALESCE(?2, detail), updated_at = ?3
             WHERE id = ?4",
            params![status, detail, Utc::now().to_rfc3339(), task_id],
        )?;
        Ok(n > 0)
    }

    /// Receiver acknowledgement: `delivered`/`queued`/`claimed` -> `accepted`.
    pub fn ack_task(&self, task_id: &str, note: Option<&str>) -> Result<bool, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let n = conn.execute(
            "UPDATE tasks SET status = 'accepted', detail = COALESCE(?1, detail), updated_at = ?2
             WHERE id = ?3 AND status IN ('delivered', 'queued', 'claimed')",
            params![note, Utc::now().to_rfc3339(), task_id],
        )?;
        Ok(n > 0)
    }

    /// Atomically claim the oldest queued task for `peer` in `project` (AF-05).
    /// Stale claims whose lease expired are requeued first.
    pub fn claim_next_task(
        &self,
        peer: &str,
        project: &str,
        lease_secs: i64,
    ) -> Result<Option<TaskRow>, DbError> {
        let claimed_id: Option<String> = {
            let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
            let now = Utc::now();
            let lease_cutoff = (now - Duration::seconds(lease_secs)).to_rfc3339();
            conn.execute(
                "UPDATE tasks
                 SET status = 'queued', claimed_by = NULL, claimed_at = NULL, lease_secs = NULL,
                     updated_at = ?1
                 WHERE status = 'claimed' AND claimed_at IS NOT NULL AND claimed_at < ?2",
                params![now.to_rfc3339(), lease_cutoff],
            )?;
            conn.query_row(
                "UPDATE tasks
                 SET status = 'claimed', claimed_by = ?1, claimed_at = ?2, lease_secs = ?3,
                     updated_at = ?2
                 WHERE id = (
                     SELECT id FROM tasks
                     WHERE status = 'queued' AND project = ?4
                       AND (to_peer = ?1 OR to_peer = 'all' OR to_peer = '*')
                     ORDER BY created_at ASC, id ASC
                     LIMIT 1
                 )
                 RETURNING id",
                params![peer, now.to_rfc3339(), lease_secs, project],
                |r| r.get(0),
            )
            .optional()?
        };
        match claimed_id {
            Some(id) => self.get_task(&id),
            None => Ok(None),
        }
    }

    pub fn get_task(&self, task_id: &str) -> Result<Option<TaskRow>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        conn.query_row(
            "SELECT id, kind, from_peer, to_peer, to_window, project, prompt_preview, status,
                    detail, claimed_by, claimed_at, lease_secs, created_at, updated_at
             FROM tasks WHERE id = ?1",
            params![task_id],
            parse_task_row,
        )
        .optional()
        .map_err(DbError::from)
    }

    pub fn list_tasks(
        &self,
        peer: Option<&str>,
        status: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskRow>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut sql = String::from(
            "SELECT id, kind, from_peer, to_peer, to_window, project, prompt_preview, status,
                    detail, claimed_by, claimed_at, lease_secs, created_at, updated_at
             FROM tasks WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(p) = peer {
            if !p.trim().is_empty() {
                sql.push_str(" AND (to_peer = ? OR from_peer = ?)");
                params.push(Box::new(p.trim().to_string()));
                params.push(Box::new(p.trim().to_string()));
            }
        }
        if let Some(s) = status {
            if !s.trim().is_empty() {
                sql.push_str(" AND status = ?");
                params.push(Box::new(s.trim().to_string()));
            }
        }
        sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");
        params.push(Box::new(i64::try_from(limit).unwrap_or(i64::MAX)));
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(refs.as_slice(), parse_task_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn count_open_tasks(&self) -> Result<i64, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status IN ('queued', 'delivered', 'accepted', 'claimed')",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    // --- AF-05 budget usage ---

    /// Record prompt usage; returns the heuristic token estimate (chars / 4).
    pub fn record_budget_usage(
        &self,
        task_id: Option<&str>,
        peer: &str,
        prompt_chars: usize,
    ) -> Result<usize, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let est_tokens = prompt_chars / 4;
        conn.execute(
            "INSERT INTO budget_usage (task_id, peer, prompt_chars, est_tokens, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                task_id,
                peer,
                prompt_chars as i64,
                est_tokens as i64,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(est_tokens)
    }

    pub fn total_used_chars_for_peer(&self, peer: &str) -> Result<(i64, i64), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let (chars, calls): (i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(prompt_chars), 0), COUNT(*) FROM budget_usage WHERE peer = ?1",
            params![peer],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((chars.max(0), calls.max(0)))
    }

    // --- AF-06 connector audit ---

    pub fn record_connector_call(
        &self,
        connector: &str,
        project: &str,
        method: &str,
        ok: bool,
        error: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        conn.execute(
            "INSERT INTO connector_calls (connector, project, method, ok, error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                connector,
                project,
                method,
                if ok { 1 } else { 0 },
                error,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_connector_calls(&self, limit: usize) -> Result<Vec<ConnectorCallRow>, DbError> {
        let conn = self.conn.lock().map_err(|_| DbError::LockPoisoned)?;
        let mut stmt = conn.prepare(
            "SELECT id, connector, project, method, ok, error, created_at
             FROM connector_calls ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
            let ok_int: i64 = r.get(4)?;
            let created_str: String = r.get(6)?;
            let created_at = DateTime::parse_from_rfc3339(&created_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            Ok(ConnectorCallRow {
                id: r.get(0)?,
                connector: r.get(1)?,
                project: r.get(2)?,
                method: r.get(3)?,
                ok: ok_int != 0,
                error: r.get(5)?,
                created_at,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod af_tests {
    use super::*;

    fn task(id: &str, to_peer: &str, project: &str, created_offset_secs: i64) -> TaskRow {
        let now = Utc::now();
        TaskRow {
            id: id.to_string(),
            kind: "ask".to_string(),
            from_peer: "vibe".to_string(),
            to_peer: to_peer.to_string(),
            to_window: None,
            project: project.to_string(),
            prompt_preview: "preview".to_string(),
            status: "queued".to_string(),
            detail: None,
            claimed_by: None,
            claimed_at: None,
            lease_secs: None,
            created_at: now - Duration::seconds(created_offset_secs),
            updated_at: now,
        }
    }

    #[test]
    fn mailbox_project_isolation_and_ack() {
        let board = Blackboard::open_in_memory().unwrap();
        board
            .send_messages_project("vibe", &["codex".into()], "msg A", None, "proj_a")
            .unwrap();
        board
            .send_messages_project("vibe", &["codex".into()], "msg B", None, "proj_b")
            .unwrap();
        board
            .send_messages_project("vibe", &["*".into()], "bcast", None, "proj_a")
            .unwrap();

        let page_a = board
            .read_inbox_project("codex", "proj_a", false, false, 50, false)
            .unwrap();
        assert_eq!(page_a.messages.len(), 2); // direct + broadcast of proj_a only
        assert!(page_a.messages.iter().all(|m| m.project == "proj_a"));
        assert_eq!(board.unread_count_project("codex", "proj_a").unwrap(), 2);
        assert_eq!(board.unread_count_project("codex", "proj_b").unwrap(), 1);

        let all = board
            .read_inbox_project("codex", "proj_a", true, false, 50, false)
            .unwrap();
        assert_eq!(all.messages.len(), 3);

        let id = page_a
            .messages
            .iter()
            .find(|m| m.body == "msg A")
            .unwrap()
            .id;
        assert_eq!(board.ack_messages("codex", &[id]).unwrap(), 1);
        let page = board
            .read_inbox_project("codex", "proj_a", false, false, 50, false)
            .unwrap();
        let acked = page.messages.iter().find(|m| m.id == id).unwrap();
        assert!(acked.ack_at.is_some());
        // acking again does not double-count
        assert_eq!(board.ack_messages("codex", &[id]).unwrap(), 1);
        // other peer cannot ack
        assert_eq!(board.ack_messages("agy", &[id]).unwrap(), 0);

        let projects = board.list_projects_with_unread("codex").unwrap();
        let map: std::collections::HashMap<_, _> = projects
            .iter()
            .map(|p| (p.project.as_str(), p.unread))
            .collect();
        assert_eq!(map.get("proj_a"), Some(&1)); // ack also marks read
        assert_eq!(map.get("proj_b"), Some(&1));
    }

    #[test]
    fn messages_migration_adds_project_and_ack_columns() {
        // Simulate a pre-AF-02 database: messages without project/ack_at, user_version=1.
        let dir = std::env::temp_dir().join(format!("octo-mig-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    sender TEXT NOT NULL, recipient TEXT NOT NULL, body TEXT NOT NULL,
                    ref_task TEXT, created_at TEXT NOT NULL, read_at TEXT
                 );
                 INSERT INTO messages (sender, recipient, body, created_at) VALUES ('vibe','codex','hi','2026-01-01T00:00:00+00:00');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let board = Blackboard::open(&path).unwrap();
        let page = board.read_inbox("codex", false, 10, false).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].project, "default");
        assert!(page.messages[0].ack_at.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presence_upsert_prune_and_ttl() {
        let board = Blackboard::open_in_memory().unwrap();
        let now = Utc::now();
        let row = PresenceRow {
            peer: "codex".into(),
            pid: 42,
            host: "fedora".into(),
            tty: Some("/dev/pts/2".into()),
            started_at: now,
            last_seen: now,
            meta: None,
        };
        board.upsert_presence(&row).unwrap();
        board
            .upsert_presence(&PresenceRow {
                last_seen: now,
                ..row.clone()
            })
            .unwrap();
        assert_eq!(board.list_presence().unwrap().len(), 1); // upsert, not duplicate

        let stale = PresenceRow {
            peer: "agy".into(),
            pid: 43,
            host: "fedora".into(),
            tty: None,
            started_at: now - Duration::seconds(3600),
            last_seen: now - Duration::seconds(3600),
            meta: None,
        };
        board.upsert_presence(&stale).unwrap();
        let pruned = board
            .prune_presence_before(now - Duration::seconds(120))
            .unwrap();
        assert_eq!(pruned, 1);
        assert_eq!(board.list_presence().unwrap().len(), 1);
    }

    #[test]
    fn claim_next_task_is_atomic_and_requeues_expired_leases() {
        let board = Blackboard::open_in_memory().unwrap();
        board
            .create_task(&task("t1", "codex", "proj_a", 10))
            .unwrap();
        board
            .create_task(&task("t2", "codex", "proj_a", 5))
            .unwrap();
        board
            .create_task(&task("t3", "codex", "proj_b", 1))
            .unwrap(); // other project
        board.create_task(&task("t4", "agy", "proj_a", 1)).unwrap(); // other peer

        let first = board.claim_next_task("codex", "proj_a", 300).unwrap();
        assert_eq!(first.unwrap().id, "t1"); // oldest first
                                             // second claimer gets the next one, never the same
        let second = board.claim_next_task("codex", "proj_a", 300).unwrap();
        assert_eq!(second.unwrap().id, "t2");
        assert!(board
            .claim_next_task("codex", "proj_a", 300)
            .unwrap()
            .is_none());
        // other project / other peer untouched
        assert!(board
            .claim_next_task("codex", "proj_b", 300)
            .unwrap()
            .is_some());
        assert!(board
            .claim_next_task("agy", "proj_a", 300)
            .unwrap()
            .is_some());

        // expired lease requeues the claim
        let board2 = Blackboard::open_in_memory().unwrap();
        board2
            .create_task(&task("x1", "codex", "proj_a", 0))
            .unwrap();
        let claimed = board2
            .claim_next_task("codex", "proj_a", 1)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.status, "claimed");
        assert_eq!(claimed.claimed_by.as_deref(), Some("codex"));
        // force the claim into the past, then a short-lease claim must requeue it
        {
            let conn = board2.conn.lock().unwrap();
            conn.execute(
                "UPDATE tasks SET claimed_at = ?1 WHERE id = 'x1'",
                params![(Utc::now() - Duration::seconds(3600)).to_rfc3339()],
            )
            .unwrap();
        }
        let reclaimed = board2.claim_next_task("codex", "proj_a", 300).unwrap();
        let reclaimed = reclaimed.unwrap();
        assert_eq!(reclaimed.claimed_by.as_deref(), Some("codex"));
        assert_eq!(reclaimed.status, "claimed");
    }

    #[test]
    fn ack_task_only_from_deliverable_states() {
        let board = Blackboard::open_in_memory().unwrap();
        board
            .create_task(&task("a1", "codex", "proj_a", 0))
            .unwrap();
        assert!(board.ack_task("a1", Some("starting")).unwrap());
        assert!(!board.ack_task("a1", None).unwrap()); // already accepted
        board.set_task_status("a1", "done", None).unwrap();
        assert!(!board.ack_task("a1", None).unwrap()); // done cannot be acked
        assert_eq!(board.get_task("a1").unwrap().unwrap().status, "done");
        assert!(board.count_open_tasks().unwrap() == 0);
    }

    #[test]
    fn budget_usage_and_connector_audit() {
        let board = Blackboard::open_in_memory().unwrap();
        assert_eq!(
            board.record_budget_usage(Some("t1"), "vibe", 400).unwrap(),
            100
        );
        board.record_budget_usage(None, "vibe", 40).unwrap();
        let (chars, calls) = board.total_used_chars_for_peer("vibe").unwrap();
        assert_eq!(chars, 440);
        assert_eq!(calls, 2);

        board
            .record_connector_call("github", "proj_a", "tools/list", true, None)
            .unwrap();
        board
            .record_connector_call("drive", "proj_a", "tools/call", false, Some("boom"))
            .unwrap();
        let rows = board.list_connector_calls(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.connector == "github" && r.ok));
        assert!(rows.iter().any(|r| r.connector == "drive" && !r.ok));
    }

    #[test]
    fn busy_peer_recovers_after_ttl() {
        let board = Blackboard::open_in_memory().unwrap();
        board.ensure_peers(&["codex".to_string()]).unwrap();
        board.set_peer_busy("codex", 1, None).unwrap();
        assert!(!board.is_peer_available("codex").unwrap());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(board.is_peer_available("codex").unwrap()); // cooldown expired -> ready
    }
}
