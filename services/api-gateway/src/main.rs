use std::path::PathBuf;
use std::{net::SocketAddr, sync::Arc};

use artifact_store::ArtifactStore;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use job_queue::JobQueue;
use pipeline_types::{JobStatus, PipelineJob, PipelineJobRequest, Stage};
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

    let state = AppState {
        store: Arc::new(ArtifactStore::open(&db_path)?),
        queue: Arc::new(JobQueue::new()),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/jobs", post(create_job))
        .with_state(state);

    let addr: SocketAddr = "0.0.0.0:3000".parse()?;
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
        .enqueue(&job)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((StatusCode::CREATED, Json(job)))
}
