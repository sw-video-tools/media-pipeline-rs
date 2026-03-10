//! SQLite-backed job queue for pipeline task dispatch.
//!
//! Jobs are enqueued with a stage and dequeued in FIFO order. A job
//! is marked "claimed" on dequeue so concurrent workers won't receive
//! the same item.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use tracing::info;

/// A queue entry referencing a pipeline job and its target stage.
#[derive(Debug, Clone)]
pub struct QueueEntry {
    pub entry_id: i64,
    pub job_id: String,
    pub stage: String,
    pub enqueued_at: String,
}

/// Persists a FIFO work queue in SQLite.
#[derive(Debug)]
pub struct JobQueue {
    conn: Mutex<Connection>,
}

impl JobQueue {
    /// Open (or create) a queue database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open queue db at {}", path.display()))?;
        let queue = Self {
            conn: Mutex::new(conn),
        };
        queue.migrate()?;
        info!("job-queue opened at {}", path.display());
        Ok(queue)
    }

    /// Create an in-memory queue (useful for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let queue = Self {
            conn: Mutex::new(conn),
        };
        queue.migrate()?;
        Ok(queue)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().expect("lock poisoned");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS queue (
                entry_id    INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id      TEXT NOT NULL,
                stage       TEXT NOT NULL,
                enqueued_at TEXT NOT NULL,
                claimed     INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(())
    }

    /// Add a job to the queue for a given stage.
    pub async fn enqueue(&self, job_id: &str, stage: &str) -> Result<()> {
        let conn = self.conn.lock().expect("lock poisoned");
        conn.execute(
            "INSERT INTO queue (job_id, stage, enqueued_at) VALUES (?1, ?2, ?3)",
            (job_id, stage, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Claim the oldest unclaimed entry, returning it if available.
    pub async fn dequeue(&self) -> Result<Option<QueueEntry>> {
        let conn = self.conn.lock().expect("lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT entry_id, job_id, stage, enqueued_at
             FROM queue WHERE claimed = 0
             ORDER BY entry_id ASC LIMIT 1",
        )?;

        let entry = stmt
            .query_map([], |row| {
                Ok(QueueEntry {
                    entry_id: row.get(0)?,
                    job_id: row.get(1)?,
                    stage: row.get(2)?,
                    enqueued_at: row.get(3)?,
                })
            })?
            .next()
            .transpose()?;

        if let Some(ref e) = entry {
            conn.execute(
                "UPDATE queue SET claimed = 1 WHERE entry_id = ?1",
                [e.entry_id],
            )?;
        }

        Ok(entry)
    }

    /// Remove a claimed entry after successful processing.
    pub async fn acknowledge(&self, entry_id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("lock poisoned");
        conn.execute("DELETE FROM queue WHERE entry_id = ?1", [entry_id])?;
        Ok(())
    }

    /// Return a claimed entry to the queue so it can be retried later.
    pub async fn nack(&self, entry_id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("lock poisoned");
        conn.execute(
            "UPDATE queue SET claimed = 0 WHERE entry_id = ?1",
            [entry_id],
        )?;
        Ok(())
    }

    /// Return the number of unclaimed entries.
    pub async fn pending_count(&self) -> Result<usize> {
        let conn = self.conn.lock().expect("lock poisoned");
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM queue WHERE claimed = 0", [], |row| {
                row.get(0)
            })?;
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enqueue_and_dequeue() {
        let q = JobQueue::open_in_memory().unwrap();
        q.enqueue("job-1", "Planning").await.unwrap();

        let entry = q.dequeue().await.unwrap().expect("expected an entry");
        assert_eq!(entry.job_id, "job-1");
        assert_eq!(entry.stage, "Planning");
    }

    #[tokio::test]
    async fn dequeue_empty_returns_none() {
        let q = JobQueue::open_in_memory().unwrap();
        let entry = q.dequeue().await.unwrap();
        assert!(entry.is_none());
    }

    #[tokio::test]
    async fn fifo_order() {
        let q = JobQueue::open_in_memory().unwrap();
        q.enqueue("a", "Planning").await.unwrap();
        q.enqueue("b", "Research").await.unwrap();

        let first = q.dequeue().await.unwrap().unwrap();
        assert_eq!(first.job_id, "a");
        let second = q.dequeue().await.unwrap().unwrap();
        assert_eq!(second.job_id, "b");
    }

    #[tokio::test]
    async fn claimed_items_not_redequeued() {
        let q = JobQueue::open_in_memory().unwrap();
        q.enqueue("x", "Script").await.unwrap();

        let _entry = q.dequeue().await.unwrap().unwrap();
        // Second dequeue should return None since 'x' is claimed
        let none = q.dequeue().await.unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn acknowledge_removes_entry() {
        let q = JobQueue::open_in_memory().unwrap();
        q.enqueue("y", "Tts").await.unwrap();

        let entry = q.dequeue().await.unwrap().unwrap();
        q.acknowledge(entry.entry_id).await.unwrap();

        assert_eq!(q.pending_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn nack_returns_entry_to_queue() {
        let q = JobQueue::open_in_memory().unwrap();
        q.enqueue("job-1", "Planning").await.unwrap();

        let entry = q.dequeue().await.unwrap().unwrap();
        // Entry is claimed, queue appears empty
        assert!(q.dequeue().await.unwrap().is_none());

        // Nack returns it to the queue
        q.nack(entry.entry_id).await.unwrap();
        let retry = q.dequeue().await.unwrap().unwrap();
        assert_eq!(retry.job_id, "job-1");
        assert_eq!(retry.entry_id, entry.entry_id);
    }

    #[tokio::test]
    async fn pending_count_tracks_unclaimed() {
        let q = JobQueue::open_in_memory().unwrap();
        assert_eq!(q.pending_count().await.unwrap(), 0);

        q.enqueue("a", "Planning").await.unwrap();
        q.enqueue("b", "Research").await.unwrap();
        assert_eq!(q.pending_count().await.unwrap(), 2);

        q.dequeue().await.unwrap(); // claims one
        assert_eq!(q.pending_count().await.unwrap(), 1);
    }
}
