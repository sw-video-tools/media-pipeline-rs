//! Orchestrator service: polls the job queue and drives each job
//! through the pipeline stages by calling downstream services.

mod dispatch;
mod registry;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tracing::{error, info, warn};

use artifact_store::ArtifactStore;
use job_queue::JobQueue;
use pipeline_types::{JobStatus, Stage};

use crate::dispatch::{DispatchOutcome, check_health, dispatch_stage};
use crate::registry::{ServiceRegistry, max_attempts_for_stage};

#[derive(Clone)]
struct AppState {
    store: Arc<ArtifactStore>,
    queue: Arc<JobQueue>,
    registry: ServiceRegistry,
    http: reqwest::Client,
    shutting_down: Arc<AtomicBool>,
}

#[derive(Debug, Serialize)]
struct ServiceHealth {
    name: String,
    url: String,
    healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct OrchestratorStatus {
    running: bool,
    pending_jobs: usize,
    services: Vec<ServiceHealth>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let db_path: PathBuf = std::env::var("SQLITE_DB")
        .unwrap_or_else(|_| "./data/pipeline.db".into())
        .into();
    let queue_path: PathBuf = std::env::var("QUEUE_DB")
        .unwrap_or_else(|_| "./data/queue.db".into())
        .into();

    let store = Arc::new(ArtifactStore::open(&db_path)?);
    let queue = Arc::new(JobQueue::open(&queue_path)?);
    let registry = ServiceRegistry::from_env();
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(300))
        .build()?;

    let shutting_down = Arc::new(AtomicBool::new(false));

    let state = AppState {
        store: store.clone(),
        queue: queue.clone(),
        registry: registry.clone(),
        http: http.clone(),
        shutting_down: shutting_down.clone(),
    };

    // Health/status API
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/status", get(status))
        .with_state(state.clone());

    let addr: SocketAddr = std::env::var("ORCHESTRATOR_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3199".into())
        .parse()?;
    info!("orchestrator-service listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Spawn the poll loop
    let poll_state = state.clone();
    let poll_handle = tokio::spawn(async move {
        poll_loop(poll_state).await;
    });

    // Listen for shutdown signals (SIGTERM, SIGINT).
    let shutdown_flag = shutting_down.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        info!("shutdown signal received, draining...");
        shutdown_flag.store(true, Ordering::SeqCst);
    });

    // Serve until the poll loop exits (triggered by shutdown flag).
    let serve_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                // Wait until the poll loop finishes its current dispatch.
                poll_handle.await.ok();
            })
            .await
            .ok();
    });

    serve_handle.await?;
    info!("orchestrator shut down cleanly");
    Ok(())
}

/// Wait for SIGTERM or SIGINT (Ctrl-C).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl-c");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn status(State(state): State<AppState>) -> Json<OrchestratorStatus> {
    let pending = state.queue.pending_count().await.unwrap_or(0);

    let mut services = Vec::with_capacity(registry::STAGE_NAMES.len());
    for name in registry::STAGE_NAMES {
        if let Some(url) = state.registry.healthz_url(name) {
            let (healthy, error) = match check_health(&state.http, &url).await {
                Ok(()) => (true, None),
                Err(e) => (false, Some(e)),
            };
            services.push(ServiceHealth {
                name: name.to_string(),
                url,
                healthy,
                error,
            });
        }
    }

    Json(OrchestratorStatus {
        running: true,
        pending_jobs: pending,
        services,
    })
}

/// Poll loop: dequeue jobs one at a time and dispatch sequentially.
///
/// All dispatches are serialized — only one service call is in flight at
/// any time. This is required because downstream GPU services cannot
/// handle concurrent requests without quality degradation.
///
/// When a service is unreachable the queue entry is nacked (returned to
/// pending) and the job is marked `Pending` so the orchestrator will
/// retry it on a future poll cycle once the service comes back up.
async fn poll_loop(state: AppState) {
    let poll_interval = Duration::from_secs(
        std::env::var("POLL_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5),
    );

    // Longer backoff when services are down to avoid log spam.
    let unavailable_backoff = Duration::from_secs(
        std::env::var("UNAVAILABLE_BACKOFF_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15),
    );

    // Default max retry attempts before marking a job as permanently failed.
    let default_max_attempts: u32 = std::env::var("MAX_RETRY_ATTEMPTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    loop {
        if state.shutting_down.load(Ordering::SeqCst) {
            info!("shutdown flag set, exiting poll loop");
            break;
        }
        match state.queue.dequeue().await {
            Ok(Some(entry)) => {
                info!(
                    job_id = %entry.job_id,
                    stage = %entry.stage,
                    "processing queue entry"
                );
                let _ = state
                    .store
                    .append_event(
                        &entry.job_id,
                        "info",
                        &format!("dispatching stage {}", entry.stage),
                    )
                    .await;

                // Sequential dispatch — one at a time, awaited to completion.
                let outcome = dispatch_stage(
                    &state.http,
                    &state.registry,
                    &state.store,
                    &entry.job_id,
                    &entry.stage,
                )
                .await;

                match outcome {
                    DispatchOutcome::Ok(next_stage) => {
                        let _ = state
                            .store
                            .append_event(
                                &entry.job_id,
                                "info",
                                &format!("stage {} completed", entry.stage),
                            )
                            .await;
                        if let Err(e) = state.queue.acknowledge(entry.entry_id).await {
                            error!(error = %e, "failed to acknowledge queue entry");
                        }

                        if let Some(next) = next_stage {
                            let next_str = next.to_string();
                            info!(
                                job_id = %entry.job_id,
                                next_stage = %next_str,
                                "enqueueing next stage"
                            );
                            if let Err(e) = state.queue.enqueue(&entry.job_id, &next_str).await {
                                error!(error = %e, "failed to enqueue next stage");
                            }
                        }
                    }
                    DispatchOutcome::ServiceUnavailable(reason) => {
                        let _ = state
                            .store
                            .append_event(
                                &entry.job_id,
                                "warn",
                                &format!(
                                    "stage {} service unavailable (attempt {}): {}",
                                    entry.stage,
                                    entry.attempts + 1,
                                    reason,
                                ),
                            )
                            .await;
                        let max_attempts =
                            max_attempts_for_stage(&entry.stage, default_max_attempts);
                        if entry.attempts >= max_attempts {
                            error!(
                                job_id = %entry.job_id,
                                stage = %entry.stage,
                                attempts = entry.attempts,
                                "max retry attempts reached, marking job Failed"
                            );
                            if let Err(e) = state
                                .store
                                .update_job_status(
                                    &entry.job_id,
                                    &JobStatus::Failed,
                                    &Stage::Failed,
                                )
                                .await
                            {
                                error!(error = %e, "failed to update job status");
                            }
                            let _ = state.queue.acknowledge(entry.entry_id).await;
                        } else {
                            warn!(
                                job_id = %entry.job_id,
                                stage = %entry.stage,
                                attempts = entry.attempts,
                                max_attempts,
                                reason = %reason,
                                "service unavailable, holding job pending"
                            );
                            let current_stage: Option<Stage> =
                                serde_json::from_str(&format!("\"{}\"", entry.stage)).ok();
                            if let Some(stage) = current_stage {
                                let _ = state
                                    .store
                                    .update_job_status(&entry.job_id, &JobStatus::Pending, &stage)
                                    .await;
                            }
                            if let Err(e) = state.queue.nack(entry.entry_id).await {
                                error!(error = %e, "failed to nack queue entry");
                            }
                            tokio::time::sleep(unavailable_backoff).await;
                        }
                    }
                    DispatchOutcome::Failed(reason) => {
                        let _ = state
                            .store
                            .append_event(
                                &entry.job_id,
                                "error",
                                &format!("stage {} failed: {reason}", entry.stage),
                            )
                            .await;
                        error!(
                            job_id = %entry.job_id,
                            stage = %entry.stage,
                            reason = %reason,
                            "stage dispatch failed, marking job as Failed"
                        );
                        if let Err(e) = state
                            .store
                            .update_job_status(&entry.job_id, &JobStatus::Failed, &Stage::Failed)
                            .await
                        {
                            error!(error = %e, "failed to update job status to Failed");
                        }
                        let _ = state.queue.acknowledge(entry.entry_id).await;
                    }
                }
            }
            Ok(None) => {
                tokio::time::sleep(poll_interval).await;
            }
            Err(e) => {
                error!(error = %e, "queue dequeue error");
                tokio::time::sleep(poll_interval).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{DispatchOutcome, dispatch_stage};
    use crate::registry::ServiceRegistry;
    use pipeline_types::PipelineJobRequest;
    use std::collections::HashMap;

    #[test]
    fn mvp_stage_sequence() {
        let stages = [
            Stage::Planning,
            Stage::Research,
            Stage::Script,
            Stage::Tts,
            Stage::AsrValidation,
            Stage::Captions,
            Stage::RenderFinal,
            Stage::QaFinal,
        ];

        for i in 0..stages.len() - 1 {
            let next = stages[i].next_mvp().expect("expected a next stage");
            assert_eq!(
                next.to_string(),
                stages[i + 1].to_string(),
                "after {} expected {} got {}",
                stages[i],
                stages[i + 1],
                next
            );
        }

        // QaFinal leads to Complete
        assert!(matches!(Stage::QaFinal.next_mvp(), Some(Stage::Complete)));

        // Complete/Failed are terminal
        assert!(Stage::Complete.next_mvp().is_none());
        assert!(Stage::Failed.next_mvp().is_none());
    }

    #[test]
    fn orchestrator_status_serializes() {
        let status = OrchestratorStatus {
            running: true,
            pending_jobs: 3,
            services: vec![ServiceHealth {
                name: "Planning".into(),
                url: "http://127.0.0.1:3191/healthz".into(),
                healthy: true,
                error: None,
            }],
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"running\":true"));
        assert!(json.contains("\"pending_jobs\":3"));
        assert!(json.contains("\"healthy\":true"));
        // error field should be omitted when None
        assert!(!json.contains("\"error\""));
    }

    fn sample_job() -> pipeline_types::PipelineJob {
        pipeline_types::PipelineJob {
            job_id: "test-1".into(),
            submitted_at: chrono::Utc::now(),
            status: JobStatus::Queued,
            current_stage: Stage::Planning,
            request: PipelineJobRequest {
                title: "Test Video".into(),
                idea: "A test idea".into(),
                audience: "developers".into(),
                target_duration_seconds: 120,
                tone: "informative".into(),
                must_include: vec![],
                must_avoid: vec![],
            },
        }
    }

    /// Mock handler that echoes back a success response with a job_id.
    async fn mock_stage_handler(
        axum::Json(body): axum::Json<serde_json::Value>,
    ) -> axum::Json<serde_json::Value> {
        let job_id = body
            .get("job_id")
            .or_else(|| body.get("plan").and_then(|p| p.get("job_id")))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        axum::Json(serde_json::json!({
            "job_id": job_id,
            "status": "ok",
        }))
    }

    /// Spawn a mock service with all stage endpoints on an ephemeral port.
    async fn spawn_mock_service() -> String {
        use axum::routing::post;

        async fn mock_healthz() -> &'static str {
            "ok"
        }

        let app = Router::new()
            .route("/healthz", get(mock_healthz))
            .route("/plan", post(mock_stage_handler))
            .route("/research", post(mock_stage_handler))
            .route("/script", post(mock_stage_handler))
            .route("/tts", post(mock_stage_handler))
            .route("/validate", post(mock_stage_handler))
            .route("/captions", post(mock_stage_handler))
            .route("/render", post(mock_stage_handler))
            .route("/qa", post(mock_stage_handler));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn mock_registry(base_url: &str) -> ServiceRegistry {
        let stages = [
            "Planning",
            "Research",
            "Script",
            "Tts",
            "AsrValidation",
            "Captions",
            "RenderFinal",
            "QaFinal",
        ];
        let urls: HashMap<String, String> = stages
            .into_iter()
            .map(|s| (s.into(), base_url.into()))
            .collect();
        ServiceRegistry::from_map(urls)
    }

    #[tokio::test]
    async fn full_pipeline_with_mock_services() {
        let base_url = spawn_mock_service().await;
        let store = Arc::new(ArtifactStore::open_in_memory().unwrap());
        let queue = Arc::new(JobQueue::open_in_memory().unwrap());
        let registry = mock_registry(&base_url);
        let http = reqwest::Client::new();

        // Save job and enqueue first stage
        let job = sample_job();
        store.save_job(&job).await.unwrap();
        queue.enqueue("test-1", "Planning").await.unwrap();

        // Process all stages sequentially
        let mvp_stages = [
            "Planning",
            "Research",
            "Script",
            "Tts",
            "AsrValidation",
            "Captions",
            "RenderFinal",
            "QaFinal",
        ];

        for expected_stage in &mvp_stages {
            let entry = queue
                .dequeue()
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("expected queue entry for {expected_stage}"));
            assert_eq!(&entry.stage, expected_stage);

            let outcome =
                dispatch_stage(&http, &registry, &store, &entry.job_id, &entry.stage).await;

            match outcome {
                DispatchOutcome::Ok(next) => {
                    queue.acknowledge(entry.entry_id).await.unwrap();
                    if let Some(next_stage) = next {
                        let next_str = next_stage.to_string();
                        queue.enqueue(&entry.job_id, &next_str).await.unwrap();
                    }
                }
                other => panic!("stage {expected_stage} failed: {other:?}"),
            }
        }

        // Verify final job status
        let final_job = store.get_job("test-1").await.unwrap().unwrap();
        assert!(
            matches!(final_job.status, JobStatus::Completed),
            "expected Completed, got {}",
            final_job.status
        );
        assert!(
            matches!(final_job.current_stage, Stage::Complete),
            "expected Complete, got {}",
            final_job.current_stage
        );

        // Verify queue is empty
        assert!(queue.dequeue().await.unwrap().is_none());

        // Verify all stage outputs were saved
        for stage in &mvp_stages {
            let output = store.get_stage_output("test-1", stage).await.unwrap();
            assert!(output.is_some(), "missing output for stage {stage}");
        }
    }
}
