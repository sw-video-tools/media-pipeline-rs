//! SQLite-backed persistence for pipeline jobs and artifact metadata.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::info;

use pipeline_types::{JobStatus, PipelineJob, PipelineJobRequest, Stage};

/// Persists pipeline jobs in a local SQLite database.
#[derive(Debug)]
pub struct ArtifactStore {
    conn: Mutex<Connection>,
}

impl ArtifactStore {
    /// Open (or create) a SQLite database at `path` and run migrations.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        info!("artifact-store opened at {}", path.display());
        Ok(store)
    }

    /// Create an in-memory store (useful for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().expect("lock poisoned");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs (
                job_id          TEXT PRIMARY KEY,
                submitted_at    TEXT NOT NULL,
                status          TEXT NOT NULL,
                current_stage   TEXT NOT NULL,
                request_json    TEXT NOT NULL
            );",
        )?;
        Ok(())
    }

    /// Insert or replace a job.
    pub async fn save_job(&self, job: &PipelineJob) -> Result<()> {
        let conn = self.conn.lock().expect("lock poisoned");
        let request_json = serde_json::to_string(&job.request)?;
        conn.execute(
            "INSERT OR REPLACE INTO jobs (job_id, submitted_at, status, current_stage, request_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                &job.job_id,
                job.submitted_at.to_rfc3339(),
                serde_json::to_string(&job.status)?,
                serde_json::to_string(&job.current_stage)?,
                &request_json,
            ),
        )?;
        Ok(())
    }

    /// Fetch a job by its ID.
    pub async fn get_job(&self, job_id: &str) -> Result<Option<PipelineJob>> {
        let conn = self.conn.lock().expect("lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT job_id, submitted_at, status, current_stage, request_json
             FROM jobs WHERE job_id = ?1",
        )?;

        let mut rows = stmt.query_map([job_id], row_to_job)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// List all jobs, most recent first.
    pub async fn list_jobs(&self) -> Result<Vec<PipelineJob>> {
        let conn = self.conn.lock().expect("lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT job_id, submitted_at, status, current_stage, request_json
             FROM jobs ORDER BY submitted_at DESC",
        )?;
        let jobs: Vec<PipelineJob> = stmt
            .query_map([], row_to_job)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(jobs)
    }

    /// Update a job's status and current stage.
    pub async fn update_job_status(
        &self,
        job_id: &str,
        status: &JobStatus,
        stage: &Stage,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("lock poisoned");
        conn.execute(
            "UPDATE jobs SET status = ?1, current_stage = ?2 WHERE job_id = ?3",
            (
                serde_json::to_string(status)?,
                serde_json::to_string(stage)?,
                job_id,
            ),
        )?;
        Ok(())
    }
}

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<PipelineJob> {
    let job_id: String = row.get(0)?;
    let submitted_at_str: String = row.get(1)?;
    let status_str: String = row.get(2)?;
    let stage_str: String = row.get(3)?;
    let request_json: String = row.get(4)?;

    let submitted_at = chrono::DateTime::parse_from_rfc3339(&submitted_at_str)
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?
        .with_timezone(&chrono::Utc);

    let status: JobStatus = serde_json::from_str(&status_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let current_stage: Stage = serde_json::from_str(&stage_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let request: PipelineJobRequest = serde_json::from_str(&request_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(PipelineJob {
        job_id,
        submitted_at,
        status,
        current_stage,
        request,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pipeline_types::PipelineJobRequest;

    fn sample_job(id: &str) -> PipelineJob {
        PipelineJob {
            job_id: id.to_string(),
            submitted_at: Utc::now(),
            status: JobStatus::Queued,
            current_stage: Stage::Planning,
            request: PipelineJobRequest {
                title: "Test Video".into(),
                idea: "A test idea".into(),
                audience: "developers".into(),
                target_duration_seconds: 120,
                tone: "informative".into(),
                must_include: vec!["testing".into()],
                must_avoid: vec![],
            },
        }
    }

    #[tokio::test]
    async fn save_and_get_job() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let job = sample_job("job-001");

        store.save_job(&job).await.unwrap();

        let fetched = store
            .get_job("job-001")
            .await
            .unwrap()
            .expect("job not found");
        assert_eq!(fetched.job_id, "job-001");
        assert_eq!(fetched.request.title, "Test Video");
    }

    #[tokio::test]
    async fn get_missing_job_returns_none() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let result = store.get_job("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn list_jobs_returns_all() {
        let store = ArtifactStore::open_in_memory().unwrap();
        store.save_job(&sample_job("a")).await.unwrap();
        store.save_job(&sample_job("b")).await.unwrap();

        let jobs = store.list_jobs().await.unwrap();
        assert_eq!(jobs.len(), 2);
    }

    #[tokio::test]
    async fn update_job_status() {
        let store = ArtifactStore::open_in_memory().unwrap();
        store.save_job(&sample_job("u-1")).await.unwrap();

        store
            .update_job_status("u-1", &JobStatus::Running, &Stage::Research)
            .await
            .unwrap();

        let job = store.get_job("u-1").await.unwrap().unwrap();
        assert!(matches!(job.status, JobStatus::Running));
        assert!(matches!(job.current_stage, Stage::Research));
    }

    #[tokio::test]
    async fn save_job_upserts() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let mut job = sample_job("dup-1");
        store.save_job(&job).await.unwrap();

        job.status = JobStatus::Completed;
        store.save_job(&job).await.unwrap();

        let jobs = store.list_jobs().await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(matches!(jobs[0].status, JobStatus::Completed));
    }

    #[tokio::test]
    async fn open_file_backed_store() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        let store = ArtifactStore::open(&db_path).unwrap();
        store.save_job(&sample_job("f-1")).await.unwrap();

        // Re-open same file and verify data persists
        let store2 = ArtifactStore::open(&db_path).unwrap();
        let job = store2
            .get_job("f-1")
            .await
            .unwrap()
            .expect("job not found after reopen");
        assert_eq!(job.job_id, "f-1");
    }
}
