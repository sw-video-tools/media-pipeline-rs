//! API gateway: entry point for the media pipeline.
//!
//! Routes:
//! - POST /jobs       — create a new pipeline job
//! - GET  /jobs       — list all jobs
//! - GET  /jobs/:id   — get a single job by ID
//! - GET  /jobs/:id/detail  — enriched job with stage outputs
//! - GET  /jobs/:id/events  — job event log
//! - POST /jobs/:id/retry  — re-enqueue a failed job
//! - DELETE /jobs/:id      — remove a job and its data
//! - GET  /healthz    — health check

mod errors;
mod extract;
mod handlers;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use artifact_store::ArtifactStore;
use axum::Router;
use axum::routing::{get, post};
use job_queue::JobQueue;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tracing::info;

use crate::handlers::{
    create_job, delete_job, get_job, get_job_detail, get_job_events, healthz, list_jobs, retry_job,
};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<ArtifactStore>,
    pub queue: Arc<JobQueue>,
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

    let app = app(state);

    let addr: SocketAddr = std::env::var("API_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3190".into())
        .parse()?;
    info!("api-gateway listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn app(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/healthz", get(healthz))
        .route("/jobs", get(list_jobs).post(create_job))
        .route("/jobs/{id}", get(get_job).delete(delete_job))
        .route("/jobs/{id}/detail", get(get_job_detail))
        .route("/jobs/{id}/events", get(get_job_events))
        .route("/jobs/{id}/retry", post(retry_job))
        .layer(cors)
        .layer(RequestBodyLimitLayer::new(1024 * 1024)) // 1 MiB
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::validate_request;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::Utc;
    use pipeline_types::{JobStatus, PipelineJob, PipelineJobRequest, Stage};
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState {
            store: Arc::new(ArtifactStore::open_in_memory().unwrap()),
            queue: Arc::new(JobQueue::open_in_memory().unwrap()),
        }
    }

    fn test_app() -> Router {
        app(test_state())
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
        let app = test_app();

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

        let app = app(state);

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

    #[tokio::test]
    async fn get_job_events_returns_log() {
        let state = test_state();
        state
            .store
            .append_event("evt-1", "info", "dispatching stage Planning")
            .await
            .unwrap();
        state
            .store
            .append_event("evt-1", "info", "stage Planning completed")
            .await
            .unwrap();

        let app = app(state);

        let resp = app
            .oneshot(
                Request::get("/jobs/evt-1/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let events: Vec<serde_json::Value> = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["level"], "info");
        assert!(events[0]["message"].as_str().unwrap().contains("Planning"));
    }

    #[tokio::test]
    async fn retry_failed_job() {
        let state = test_state();
        let job = PipelineJob {
            job_id: "retry-1".into(),
            submitted_at: Utc::now(),
            status: JobStatus::Failed,
            current_stage: Stage::Script,
            request: PipelineJobRequest {
                title: "Retry Test".into(),
                idea: "test".into(),
                audience: "devs".into(),
                target_duration_seconds: 60,
                tone: "casual".into(),
                must_include: vec![],
                must_avoid: vec![],
            },
        };
        state.store.save_job(&job).await.unwrap();

        let app = app(state.clone());

        let resp = app
            .oneshot(
                Request::post("/jobs/retry-1/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Job should now be Queued
        let updated = state.store.get_job("retry-1").await.unwrap().unwrap();
        assert!(matches!(updated.status, JobStatus::Queued));

        // Queue should have an entry
        assert_eq!(state.queue.pending_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn retry_non_failed_job_returns_conflict() {
        let state = test_state();
        let job = PipelineJob {
            job_id: "retry-2".into(),
            submitted_at: Utc::now(),
            status: JobStatus::Running,
            current_stage: Stage::Script,
            request: PipelineJobRequest {
                title: "Retry Test".into(),
                idea: "test".into(),
                audience: "devs".into(),
                target_duration_seconds: 60,
                tone: "casual".into(),
                must_include: vec![],
                must_avoid: vec![],
            },
        };
        state.store.save_job(&job).await.unwrap();

        let app = app(state);

        let resp = app
            .oneshot(
                Request::post("/jobs/retry-2/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 409);
    }

    #[tokio::test]
    async fn delete_job_removes_data() {
        let state = test_state();
        let job = PipelineJob {
            job_id: "del-1".into(),
            submitted_at: Utc::now(),
            status: JobStatus::Completed,
            current_stage: Stage::Complete,
            request: PipelineJobRequest {
                title: "Delete Test".into(),
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
            .save_stage_output("del-1", "Planning", &serde_json::json!({"done": true}))
            .await
            .unwrap();

        let app = app(state.clone());

        let resp = app
            .clone()
            .oneshot(Request::delete("/jobs/del-1").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Job should be gone
        let result = state.store.get_job("del-1").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn delete_missing_job_returns_404() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::delete("/jobs/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn error_response_contains_json_body() {
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

        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["code"], 404);
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn retry_conflict_returns_json_error() {
        let state = test_state();
        let job = PipelineJob {
            job_id: "err-1".into(),
            submitted_at: Utc::now(),
            status: JobStatus::Running,
            current_stage: Stage::Script,
            request: PipelineJobRequest {
                title: "Test".into(),
                idea: "test".into(),
                audience: "devs".into(),
                target_duration_seconds: 60,
                tone: "casual".into(),
                must_include: vec![],
                must_avoid: vec![],
            },
        };
        state.store.save_job(&job).await.unwrap();

        let app = app(state);

        let resp = app
            .oneshot(
                Request::post("/jobs/err-1/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 409);

        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["code"], 409);
        assert!(body["error"].as_str().unwrap().contains("Running"));
    }

    #[tokio::test]
    async fn malformed_json_returns_400_with_json_error() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::post("/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(b"not json".to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);

        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["code"], 400);
        assert!(body["error"].as_str().unwrap().contains("invalid JSON"));
    }

    #[test]
    fn validate_rejects_empty_title() {
        let req = PipelineJobRequest {
            title: "  ".into(),
            idea: "test".into(),
            audience: "devs".into(),
            target_duration_seconds: 60,
            tone: "casual".into(),
            must_include: vec![],
            must_avoid: vec![],
        };
        let err = validate_request(&req).unwrap_err();
        assert_eq!(err.code, 400);
        assert!(err.error.contains("title"));
    }

    #[test]
    fn validate_rejects_zero_duration() {
        let req = PipelineJobRequest {
            title: "Test".into(),
            idea: "test".into(),
            audience: "devs".into(),
            target_duration_seconds: 0,
            tone: "casual".into(),
            must_include: vec![],
            must_avoid: vec![],
        };
        let err = validate_request(&req).unwrap_err();
        assert_eq!(err.code, 400);
        assert!(err.error.contains("greater than 0"));
    }

    #[test]
    fn validate_rejects_excessive_duration() {
        let req = PipelineJobRequest {
            title: "Test".into(),
            idea: "test".into(),
            audience: "devs".into(),
            target_duration_seconds: 7200,
            tone: "casual".into(),
            must_include: vec![],
            must_avoid: vec![],
        };
        let err = validate_request(&req).unwrap_err();
        assert_eq!(err.code, 400);
        assert!(err.error.contains("3600"));
    }

    #[test]
    fn validate_accepts_valid_request() {
        let req = PipelineJobRequest {
            title: "Test".into(),
            idea: "test".into(),
            audience: "devs".into(),
            target_duration_seconds: 60,
            tone: "casual".into(),
            must_include: vec![],
            must_avoid: vec![],
        };
        assert!(validate_request(&req).is_ok());
    }

    /// Spin up the api-gateway on an ephemeral port and test the full
    /// HTTP lifecycle: create → get → list → detail → events → retry → delete.
    #[tokio::test]
    async fn http_lifecycle() {
        let state = test_state();
        let test_app = app(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, test_app).await.unwrap();
        });

        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // 1. Health check
        let resp = client.get(format!("{base}/healthz")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok");

        // 2. List jobs (empty)
        let jobs: Vec<serde_json::Value> = client
            .get(format!("{base}/jobs"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(jobs.is_empty());

        // 3. Create a job
        let body = serde_json::json!({
            "title": "Integration Test",
            "idea": "test idea",
            "audience": "devs",
            "target_duration_seconds": 60,
            "tone": "casual",
            "must_include": ["rust"],
            "must_avoid": []
        });
        let resp = client
            .post(format!("{base}/jobs"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let created: serde_json::Value = resp.json().await.unwrap();
        let job_id = created["job_id"].as_str().unwrap();
        assert_eq!(created["status"], "Queued");

        // 4. Get job by ID
        let resp = client
            .get(format!("{base}/jobs/{job_id}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let fetched: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(fetched["request"]["title"], "Integration Test");

        // 5. List jobs (one)
        let jobs: Vec<serde_json::Value> = client
            .get(format!("{base}/jobs"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(jobs.len(), 1);

        // 6. Get detail (no stage outputs yet)
        let detail: serde_json::Value = client
            .get(format!("{base}/jobs/{job_id}/detail"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(detail["completed_stages"].as_array().unwrap().is_empty());

        // 7. Events (empty initially)
        let events: Vec<serde_json::Value> = client
            .get(format!("{base}/jobs/{job_id}/events"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(events.is_empty());

        // 8. Retry (should fail — job is Queued, not Failed)
        let resp = client
            .post(format!("{base}/jobs/{job_id}/retry"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409);
        let err: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(err["code"], 409);

        // 9. Not found
        let resp = client
            .get(format!("{base}/jobs/nonexistent"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
        let err: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(err["code"], 404);

        // 10. Validation error
        let bad_body = serde_json::json!({
            "title": "",
            "idea": "test",
            "audience": "devs",
            "target_duration_seconds": 60,
            "tone": "casual",
            "must_include": [],
            "must_avoid": []
        });
        let resp = client
            .post(format!("{base}/jobs"))
            .json(&bad_body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);

        // 11. Delete
        let resp = client
            .delete(format!("{base}/jobs/{job_id}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Verify deleted
        let resp = client
            .get(format!("{base}/jobs/{job_id}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }
}
