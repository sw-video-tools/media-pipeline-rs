//! API gateway: entry point for the media pipeline.
//!
//! Routes:
//! - POST /jobs       — create a new pipeline job
//! - GET  /jobs       — list all jobs
//! - GET  /jobs/:id   — get a single job by ID
//! - GET  /healthz    — health check

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use artifact_store::ArtifactStore;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use job_queue::JobQueue;
use pipeline_types::{JobStatus, PipelineJob, PipelineJobRequest, Stage};
use serde::Serialize;
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    store: Arc<ArtifactStore>,
    queue: Arc<JobQueue>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let db_path: PathBuf = std::env::var("SQLITE_DB")
        .unwrap_or_else(|_| "./data/pipeline.db".into())
        .into();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let queue_path: PathBuf = std::env::var("QUEUE_DB")
        .unwrap_or_else(|_| "./data/queue.db".into())
        .into();

    let state = AppState {
        store: Arc::new(ArtifactStore::open(&db_path)?),
        queue: Arc::new(JobQueue::open(&queue_path)?),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/jobs", get(list_jobs).post(create_job))
        .route("/jobs/{id}", get(get_job))
        .route("/jobs/{id}/detail", get(get_job_detail))
        .with_state(state);

    let addr: SocketAddr = std::env::var("API_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3190".into())
        .parse()?;
    info!("api-gateway listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn create_job(
    State(state): State<AppState>,
    Json(req): Json<PipelineJobRequest>,
) -> Result<(StatusCode, Json<PipelineJob>), StatusCode> {
    let job = PipelineJob {
        job_id: Uuid::new_v4().to_string(),
        submitted_at: Utc::now(),
        status: JobStatus::Queued,
        current_stage: Stage::Planning,
        request: req,
    };

    state
        .store
        .save_job(&job)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .queue
        .enqueue(&job.job_id, "Planning")
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    info!(job_id = %job.job_id, "job created");
    Ok((StatusCode::CREATED, Json(job)))
}

async fn list_jobs(State(state): State<AppState>) -> Result<Json<Vec<PipelineJob>>, StatusCode> {
    let jobs = state
        .store
        .list_jobs()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(jobs))
}

async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PipelineJob>, StatusCode> {
    let job = state
        .store
        .get_job(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(job))
}

/// Enriched job detail with completed stage outputs.
#[derive(Debug, Serialize)]
struct JobDetail {
    #[serde(flatten)]
    job: PipelineJob,
    completed_stages: Vec<String>,
    stage_outputs: serde_json::Value,
}

async fn get_job_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobDetail>, StatusCode> {
    let job = state
        .store
        .get_job(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let outputs = state
        .store
        .list_stage_outputs(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let completed_stages: Vec<String> = outputs.iter().map(|(s, _)| s.clone()).collect();
    let stage_outputs: serde_json::Map<String, serde_json::Value> = outputs.into_iter().collect();

    Ok(Json(JobDetail {
        job,
        completed_stages,
        stage_outputs: serde_json::Value::Object(stage_outputs),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState {
            store: Arc::new(ArtifactStore::open_in_memory().unwrap()),
            queue: Arc::new(JobQueue::open_in_memory().unwrap()),
        }
    }

    fn test_app() -> Router {
        Router::new()
            .route("/healthz", get(healthz))
            .route("/jobs", get(list_jobs).post(create_job))
            .route("/jobs/{id}", get(get_job))
            .route("/jobs/{id}/detail", get(get_job_detail))
            .with_state(test_state())
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = test_app();
        let resp = app
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn create_and_get_job() {
        let state = test_state();
        let app = Router::new()
            .route("/jobs", get(list_jobs).post(create_job))
            .route("/jobs/{id}", get(get_job))
            .with_state(state);

        // Create a job
        let body = serde_json::json!({
            "title": "Test",
            "idea": "Test idea",
            "audience": "devs",
            "target_duration_seconds": 60,
            "tone": "casual",
            "must_include": [],
            "must_avoid": []
        });

        let create_resp = app
            .clone()
            .oneshot(
                Request::post("/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), 201);

        let created: PipelineJob = serde_json::from_slice(
            &axum::body::to_bytes(create_resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();

        // Get the job by ID
        let get_resp = app
            .clone()
            .oneshot(
                Request::get(format!("/jobs/{}", created.job_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), 200);

        let fetched: PipelineJob = serde_json::from_slice(
            &axum::body::to_bytes(get_resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(fetched.job_id, created.job_id);
        assert_eq!(fetched.request.title, "Test");
    }

    #[tokio::test]
    async fn get_missing_job_returns_404() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::get("/jobs/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn get_job_detail_includes_stage_outputs() {
        let state = test_state();

        // Save a job and a stage output
        let job = PipelineJob {
            job_id: "detail-1".into(),
            submitted_at: Utc::now(),
            status: JobStatus::Running,
            current_stage: Stage::Research,
            request: PipelineJobRequest {
                title: "Detail Test".into(),
                idea: "test".into(),
                audience: "devs".into(),
                target_duration_seconds: 60,
                tone: "casual".into(),
                must_include: vec![],
                must_avoid: vec![],
            },
        };
        state.store.save_job(&job).await.unwrap();
        state
            .store
            .save_stage_output("detail-1", "Planning", &serde_json::json!({"plan": "done"}))
            .await
            .unwrap();

        let app = Router::new()
            .route("/jobs/{id}/detail", get(get_job_detail))
            .with_state(state);

        let resp = app
            .oneshot(
                Request::get("/jobs/detail-1/detail")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let detail: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(detail["job_id"], "detail-1");
        assert_eq!(detail["completed_stages"], serde_json::json!(["Planning"]));
        assert_eq!(detail["stage_outputs"]["Planning"]["plan"], "done");
    }

    #[tokio::test]
    async fn list_jobs_empty() {
        let app = test_app();
        let resp = app
            .oneshot(Request::get("/jobs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let jobs: Vec<PipelineJob> = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(jobs.is_empty());
    }
}
