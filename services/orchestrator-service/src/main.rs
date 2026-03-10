//! Orchestrator service: polls the job queue and drives each job
//! through the pipeline stages by calling downstream services.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tracing::{error, info, warn};

use artifact_store::ArtifactStore;
use job_queue::JobQueue;
use pipeline_types::{JobStatus, Stage};

/// Maps stage names to service base URLs.
#[derive(Debug, Clone)]
struct ServiceRegistry {
    urls: HashMap<String, String>,
}

impl ServiceRegistry {
    fn from_env() -> Self {
        let mut urls = HashMap::new();
        urls.insert(
            "Planning".into(),
            std::env::var("PLANNER_URL").unwrap_or_else(|_| "http://127.0.0.1:3191".into()),
        );
        urls.insert(
            "Research".into(),
            std::env::var("RESEARCH_URL").unwrap_or_else(|_| "http://127.0.0.1:3192".into()),
        );
        urls.insert(
            "Script".into(),
            std::env::var("SCRIPT_URL").unwrap_or_else(|_| "http://127.0.0.1:3193".into()),
        );
        urls.insert(
            "Tts".into(),
            std::env::var("TTS_URL").unwrap_or_else(|_| "http://127.0.0.1:3194".into()),
        );
        urls.insert(
            "AsrValidation".into(),
            std::env::var("ASR_URL").unwrap_or_else(|_| "http://127.0.0.1:3195".into()),
        );
        urls.insert(
            "Captions".into(),
            std::env::var("CAPTIONS_URL").unwrap_or_else(|_| "http://127.0.0.1:3196".into()),
        );
        urls.insert(
            "RenderFinal".into(),
            std::env::var("RENDER_URL").unwrap_or_else(|_| "http://127.0.0.1:3197".into()),
        );
        urls.insert(
            "QaFinal".into(),
            std::env::var("QA_URL").unwrap_or_else(|_| "http://127.0.0.1:3198".into()),
        );
        Self { urls }
    }

    fn endpoint_for(&self, stage: &str) -> Option<String> {
        let base = self.urls.get(stage)?;
        let path = stage_to_path(stage);
        Some(format!("{base}{path}"))
    }
}

/// Map stage names to their service endpoint paths.
fn stage_to_path(stage: &str) -> &'static str {
    match stage {
        "Planning" => "/plan",
        "Research" => "/research",
        "Script" => "/script",
        "Tts" => "/tts",
        "AsrValidation" => "/validate",
        "Captions" => "/captions",
        "RenderFinal" => "/render",
        "QaFinal" => "/qa",
        _ => "/healthz",
    }
}

#[derive(Clone)]
struct AppState {
    store: Arc<ArtifactStore>,
    queue: Arc<JobQueue>,
    registry: ServiceRegistry,
    http: reqwest::Client,
}

#[derive(Debug, Serialize)]
struct OrchestratorStatus {
    running: bool,
    pending_jobs: usize,
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

    let state = AppState {
        store: store.clone(),
        queue: queue.clone(),
        registry: registry.clone(),
        http: http.clone(),
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
    tokio::spawn(async move {
        poll_loop(poll_state).await;
    });

    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn status(State(state): State<AppState>) -> Json<OrchestratorStatus> {
    let pending = state.queue.pending_count().await.unwrap_or(0);
    Json(OrchestratorStatus {
        running: true,
        pending_jobs: pending,
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

    loop {
        match state.queue.dequeue().await {
            Ok(Some(entry)) => {
                info!(
                    job_id = %entry.job_id,
                    stage = %entry.stage,
                    "processing queue entry"
                );

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
                        warn!(
                            job_id = %entry.job_id,
                            stage = %entry.stage,
                            reason = %reason,
                            "service unavailable, holding job pending"
                        );
                        // Mark job as Pending so the UI reflects the wait.
                        let current_stage: Option<Stage> =
                            serde_json::from_str(&format!("\"{}\"", entry.stage)).ok();
                        if let Some(stage) = current_stage {
                            let _ = state
                                .store
                                .update_job_status(&entry.job_id, &JobStatus::Pending, &stage)
                                .await;
                        }
                        // Nack: return the entry to the queue for retry.
                        if let Err(e) = state.queue.nack(entry.entry_id).await {
                            error!(error = %e, "failed to nack queue entry");
                        }
                        // Back off before polling again.
                        tokio::time::sleep(unavailable_backoff).await;
                    }
                    DispatchOutcome::Failed(reason) => {
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

/// Outcome of a stage dispatch attempt.
enum DispatchOutcome {
    /// Stage completed; contains the next stage (if any).
    Ok(Option<Stage>),
    /// Service is unreachable — job should be held pending for retry.
    ServiceUnavailable(String),
    /// Permanent failure — job should be marked Failed.
    Failed(String),
}

/// Returns true if a reqwest error indicates the service is down
/// (connection refused, DNS failure, connect timeout) rather than
/// an application-level error.
fn is_service_unavailable(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_timeout()
}

/// Dispatch a single stage: call the service, update job status, return outcome.
async fn dispatch_stage(
    http: &reqwest::Client,
    registry: &ServiceRegistry,
    store: &ArtifactStore,
    job_id: &str,
    stage: &str,
) -> DispatchOutcome {
    let current_stage: Stage = match serde_json::from_str(&format!("\"{stage}\"")) {
        Ok(s) => s,
        Err(e) => return DispatchOutcome::Failed(format!("invalid stage '{stage}': {e}")),
    };

    // Mark job as Running
    if let Err(e) = store
        .update_job_status(job_id, &JobStatus::Running, &current_stage)
        .await
    {
        return DispatchOutcome::Failed(format!("failed to update job status: {e}"));
    }

    let endpoint = match registry.endpoint_for(stage) {
        Some(ep) => ep,
        None => return DispatchOutcome::Failed(format!("no endpoint for stage '{stage}'")),
    };

    info!(job_id, stage, endpoint = %endpoint, "calling service");

    // Build the request body from the job data
    let job = match store.get_job(job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => return DispatchOutcome::Failed(format!("job '{job_id}' not found")),
        Err(e) => return DispatchOutcome::Failed(format!("store error: {e}")),
    };

    let body = match build_stage_request(stage, &job) {
        Ok(b) => b,
        Err(e) => return DispatchOutcome::Failed(format!("request build error: {e}")),
    };

    let resp = match http.post(&endpoint).json(&body).send().await {
        Ok(r) => r,
        Err(e) if is_service_unavailable(&e) => {
            return DispatchOutcome::ServiceUnavailable(format!(
                "service {stage} at {endpoint} unreachable: {e}"
            ));
        }
        Err(e) => {
            return DispatchOutcome::Failed(format!("HTTP error calling {stage}: {e}"));
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return DispatchOutcome::Failed(format!("service {stage} returned {status}: {text}"));
    }

    info!(job_id, stage, "stage completed successfully");

    // Determine next stage
    let next = current_stage.next_mvp();

    // Update job status
    let status_result = match &next {
        Some(next_stage) => {
            store
                .update_job_status(job_id, &JobStatus::Running, next_stage)
                .await
        }
        None => {
            if matches!(current_stage, Stage::Complete | Stage::Failed) {
                Ok(())
            } else {
                store
                    .update_job_status(job_id, &JobStatus::Completed, &Stage::Complete)
                    .await
            }
        }
    };

    if let Err(e) = status_result {
        return DispatchOutcome::Failed(format!("failed to update job status: {e}"));
    }

    DispatchOutcome::Ok(next)
}

/// Build the JSON body for a stage's service call from the job data.
fn build_stage_request(
    stage: &str,
    job: &pipeline_types::PipelineJob,
) -> anyhow::Result<serde_json::Value> {
    match stage {
        "Planning" => Ok(serde_json::json!({
            "job_id": job.job_id,
            "title": job.request.title,
            "idea": job.request.idea,
            "audience": job.request.audience,
            "target_duration_seconds": job.request.target_duration_seconds,
            "tone": job.request.tone,
            "must_include": job.request.must_include,
            "must_avoid": job.request.must_avoid,
        })),
        "Research" => Ok(serde_json::json!({
            "job_id": job.job_id,
            "queries": [
                format!("{} overview", job.request.title),
                format!("{} for {}", job.request.idea, job.request.audience),
            ],
        })),
        "Script" => Ok(serde_json::json!({
            "plan": {
                "job_id": job.job_id,
                "title": job.request.title,
                "synopsis": job.request.idea,
                "total_duration_seconds": job.request.target_duration_seconds,
                "segments": [{
                    "segment_number": 1,
                    "title": job.request.title,
                    "duration_seconds": job.request.target_duration_seconds,
                    "description": job.request.idea,
                    "visual_style": "default",
                }],
                "research_queries": [],
                "narration_tone": job.request.tone,
            },
        })),
        "Tts" => Ok(serde_json::json!({
            "job_id": job.job_id,
            "title": job.request.title,
            "total_duration_seconds": job.request.target_duration_seconds,
            "segments": [{
                "segment_number": 1,
                "title": job.request.title,
                "narration_text": job.request.idea,
                "estimated_duration_seconds": job.request.target_duration_seconds,
                "visual_notes": "default",
            }],
        })),
        "AsrValidation" => Ok(serde_json::json!({
            "job_id": job.job_id,
            "segments": [],
        })),
        "Captions" => Ok(serde_json::json!({
            "job_id": job.job_id,
            "title": job.request.title,
            "total_duration_seconds": job.request.target_duration_seconds,
            "segments": [{
                "segment_number": 1,
                "title": job.request.title,
                "narration_text": job.request.idea,
                "estimated_duration_seconds": job.request.target_duration_seconds,
                "visual_notes": "default",
            }],
        })),
        "RenderFinal" => Ok(serde_json::json!({
            "job_id": job.job_id,
            "audio_files": [],
        })),
        "QaFinal" => Ok(serde_json::json!({
            "job_id": job.job_id,
            "output_path": format!("./data/renders/{}.mp4", job.job_id),
            "expected_duration_seconds": job.request.target_duration_seconds,
        })),
        _ => anyhow::bail!("unsupported stage: {stage}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_types::PipelineJobRequest;

    #[test]
    fn stage_to_path_maps_correctly() {
        assert_eq!(stage_to_path("Planning"), "/plan");
        assert_eq!(stage_to_path("Research"), "/research");
        assert_eq!(stage_to_path("Script"), "/script");
        assert_eq!(stage_to_path("Tts"), "/tts");
        assert_eq!(stage_to_path("AsrValidation"), "/validate");
        assert_eq!(stage_to_path("Captions"), "/captions");
        assert_eq!(stage_to_path("RenderFinal"), "/render");
        assert_eq!(stage_to_path("QaFinal"), "/qa");
    }

    #[test]
    fn service_registry_builds_endpoints() {
        let registry = ServiceRegistry::from_env();
        let endpoint = registry.endpoint_for("Planning").unwrap();
        assert!(endpoint.ends_with("/plan"));
    }

    #[test]
    fn build_planning_request() {
        let job = sample_job();
        let body = build_stage_request("Planning", &job).unwrap();
        assert_eq!(body["job_id"], "test-1");
        assert_eq!(body["title"], "Test Video");
    }

    #[test]
    fn build_research_request() {
        let job = sample_job();
        let body = build_stage_request("Research", &job).unwrap();
        assert_eq!(body["job_id"], "test-1");
        assert!(body["queries"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn build_unsupported_stage_errors() {
        let job = sample_job();
        assert!(build_stage_request("Unknown", &job).is_err());
    }

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
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"running\":true"));
        assert!(json.contains("\"pending_jobs\":3"));
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
}
