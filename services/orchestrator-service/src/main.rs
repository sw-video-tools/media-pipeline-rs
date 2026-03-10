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

    #[cfg(test)]
    fn from_map(urls: HashMap<String, String>) -> Self {
        Self { urls }
    }

    fn endpoint_for(&self, stage: &str) -> Option<String> {
        let base = self.urls.get(stage)?;
        let path = stage_to_path(stage);
        Some(format!("{base}{path}"))
    }

    fn healthz_url(&self, stage: &str) -> Option<String> {
        let base = self.urls.get(stage)?;
        Some(format!("{base}/healthz"))
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
#[derive(Debug)]
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

/// Lightweight health probe — GET /healthz with a short timeout.
/// Returns Ok(()) if the service responds 2xx, or an error message.
async fn check_health(http: &reqwest::Client, url: &str) -> Result<(), String> {
    match http.get(url).send().await {
        Ok(resp) if resp.status().is_success() => Ok(()),
        Ok(resp) => Err(format!("healthz returned {}", resp.status())),
        Err(e) => Err(format!("{e}")),
    }
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

    // Pre-flight health check — detect down services before doing any
    // heavy work (building requests, updating status, etc.).
    if let Some(healthz_url) = registry.healthz_url(stage)
        && let Err(reason) = check_health(http, &healthz_url).await
    {
        return DispatchOutcome::ServiceUnavailable(format!(
            "service {stage} failed health check: {reason}"
        ));
    }

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

    let body = match build_stage_request(stage, &job, store).await {
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

    // Parse and persist the stage output for downstream stages.
    let resp_body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return DispatchOutcome::Failed(format!("failed to parse response from {stage}: {e}"));
        }
    };
    if let Err(e) = store.save_stage_output(job_id, stage, &resp_body).await {
        error!(error = %e, "failed to save stage output (non-fatal)");
    }

    info!(job_id, stage, "stage completed successfully");

    // Determine next stage
    let next = current_stage.next_mvp();

    // Update job status
    let status_result = match &next {
        Some(Stage::Complete) => {
            store
                .update_job_status(job_id, &JobStatus::Completed, &Stage::Complete)
                .await
        }
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

    // Don't enqueue terminal stages — signal completion by returning None.
    let next = next.filter(|s| !matches!(s, Stage::Complete | Stage::Failed));

    if let Err(e) = status_result {
        return DispatchOutcome::Failed(format!("failed to update job status: {e}"));
    }

    DispatchOutcome::Ok(next)
}

/// Build the JSON body for a stage's service call.
///
/// Uses prior stage outputs when available; falls back to deriving
/// from the original job request.
async fn build_stage_request(
    stage: &str,
    job: &pipeline_types::PipelineJob,
    store: &ArtifactStore,
) -> anyhow::Result<serde_json::Value> {
    let job_id = &job.job_id;

    match stage {
        "Planning" => Ok(serde_json::json!({
            "job_id": job_id,
            "title": job.request.title,
            "idea": job.request.idea,
            "audience": job.request.audience,
            "target_duration_seconds": job.request.target_duration_seconds,
            "tone": job.request.tone,
            "must_include": job.request.must_include,
            "must_avoid": job.request.must_avoid,
        })),
        "Research" => {
            // Use planning output for research queries if available.
            let plan = store
                .get_stage_output(job_id, "Planning")
                .await
                .ok()
                .flatten();
            let queries = if let Some(ref p) = plan {
                p["research_queries"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            } else {
                vec![
                    serde_json::json!(format!("{} overview", job.request.title)),
                    serde_json::json!(format!("{} for {}", job.request.idea, job.request.audience)),
                ]
            };
            Ok(serde_json::json!({
                "job_id": job_id,
                "queries": queries,
            }))
        }
        "Script" => {
            // Use planning output if available, else derive from request.
            let plan = store
                .get_stage_output(job_id, "Planning")
                .await
                .ok()
                .flatten();
            let plan_body = plan.unwrap_or_else(|| {
                serde_json::json!({
                    "job_id": job_id,
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
                })
            });
            Ok(serde_json::json!({ "plan": plan_body }))
        }
        "Tts" => {
            // Use script output if available.
            let script = store
                .get_stage_output(job_id, "Script")
                .await
                .ok()
                .flatten();
            Ok(script.unwrap_or_else(|| {
                serde_json::json!({
                    "job_id": job_id,
                    "title": job.request.title,
                    "total_duration_seconds": job.request.target_duration_seconds,
                    "segments": [{
                        "segment_number": 1,
                        "title": job.request.title,
                        "narration_text": job.request.idea,
                        "estimated_duration_seconds": job.request.target_duration_seconds,
                        "visual_notes": "default",
                    }],
                })
            }))
        }
        "AsrValidation" => {
            // Use TTS output for audio paths + script for expected text.
            let tts = store.get_stage_output(job_id, "Tts").await.ok().flatten();
            let segments = if let Some(ref t) = tts {
                t["segments"].as_array().cloned().unwrap_or_default()
            } else {
                vec![]
            };
            Ok(serde_json::json!({
                "job_id": job_id,
                "segments": segments,
            }))
        }
        "Captions" => {
            // Use script output if available.
            let script = store
                .get_stage_output(job_id, "Script")
                .await
                .ok()
                .flatten();
            Ok(script.unwrap_or_else(|| {
                serde_json::json!({
                    "job_id": job_id,
                    "title": job.request.title,
                    "total_duration_seconds": job.request.target_duration_seconds,
                    "segments": [{
                        "segment_number": 1,
                        "title": job.request.title,
                        "narration_text": job.request.idea,
                        "estimated_duration_seconds": job.request.target_duration_seconds,
                        "visual_notes": "default",
                    }],
                })
            }))
        }
        "RenderFinal" => {
            // Pull audio paths from TTS output.
            let tts = store.get_stage_output(job_id, "Tts").await.ok().flatten();
            let audio_files: Vec<serde_json::Value> = tts
                .as_ref()
                .and_then(|t| t["audio_files"].as_array().cloned())
                .or_else(|| {
                    tts.as_ref().and_then(|t| {
                        t["segments"].as_array().map(|segs| {
                            segs.iter()
                                .filter_map(|s| {
                                    s["audio_path"].as_str().map(|p| serde_json::json!(p))
                                })
                                .collect()
                        })
                    })
                })
                .unwrap_or_default();
            Ok(serde_json::json!({
                "job_id": job_id,
                "audio_files": audio_files,
            }))
        }
        "QaFinal" => {
            // Pull output path from render output.
            let render = store
                .get_stage_output(job_id, "RenderFinal")
                .await
                .ok()
                .flatten();
            let output_path = render
                .as_ref()
                .and_then(|r| r["output_path"].as_str().map(String::from))
                .unwrap_or_else(|| format!("./data/renders/{job_id}.mp4"));
            Ok(serde_json::json!({
                "job_id": job_id,
                "output_path": output_path,
                "expected_duration_seconds": job.request.target_duration_seconds,
            }))
        }
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

    #[tokio::test]
    async fn build_planning_request() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let job = sample_job();
        let body = build_stage_request("Planning", &job, &store).await.unwrap();
        assert_eq!(body["job_id"], "test-1");
        assert_eq!(body["title"], "Test Video");
    }

    #[tokio::test]
    async fn build_research_request_without_plan() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let job = sample_job();
        let body = build_stage_request("Research", &job, &store).await.unwrap();
        assert_eq!(body["job_id"], "test-1");
        assert!(body["queries"].as_array().unwrap().len() >= 2);
    }

    #[tokio::test]
    async fn build_research_uses_plan_output() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let plan_output = serde_json::json!({
            "job_id": "test-1",
            "research_queries": ["query from plan"],
        });
        store
            .save_stage_output("test-1", "Planning", &plan_output)
            .await
            .unwrap();

        let job = sample_job();
        let body = build_stage_request("Research", &job, &store).await.unwrap();
        let queries = body["queries"].as_array().unwrap();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0], "query from plan");
    }

    #[tokio::test]
    async fn build_qa_uses_render_output() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let render_output = serde_json::json!({
            "job_id": "test-1",
            "output_path": "/custom/path.mp4",
        });
        store
            .save_stage_output("test-1", "RenderFinal", &render_output)
            .await
            .unwrap();

        let job = sample_job();
        let body = build_stage_request("QaFinal", &job, &store).await.unwrap();
        assert_eq!(body["output_path"], "/custom/path.mp4");
    }

    #[tokio::test]
    async fn build_unsupported_stage_errors() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let job = sample_job();
        assert!(build_stage_request("Unknown", &job, &store).await.is_err());
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
    /// Returns the base URL (e.g. "http://127.0.0.1:XXXXX").
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
    async fn dispatch_stage_success() {
        let base_url = spawn_mock_service().await;
        let store = ArtifactStore::open_in_memory().unwrap();
        let registry = mock_registry(&base_url);
        let http = reqwest::Client::new();
        let job = sample_job();
        store.save_job(&job).await.unwrap();

        let outcome = dispatch_stage(&http, &registry, &store, "test-1", "Planning").await;
        assert!(
            matches!(outcome, DispatchOutcome::Ok(Some(Stage::Research))),
            "expected Ok(Research), got {outcome:?}"
        );

        // Verify stage output was saved
        let output = store.get_stage_output("test-1", "Planning").await.unwrap();
        assert!(output.is_some());
    }

    #[tokio::test]
    async fn dispatch_stage_service_unavailable() {
        let store = ArtifactStore::open_in_memory().unwrap();
        // Point to a port where nothing is listening
        let urls: HashMap<String, String> =
            [("Planning".into(), "http://127.0.0.1:1".into())].into();
        let registry = ServiceRegistry::from_map(urls);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let job = sample_job();
        store.save_job(&job).await.unwrap();

        let outcome = dispatch_stage(&http, &registry, &store, "test-1", "Planning").await;
        assert!(
            matches!(outcome, DispatchOutcome::ServiceUnavailable(_)),
            "expected ServiceUnavailable, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn health_check_catches_down_service_before_dispatch() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let urls: HashMap<String, String> =
            [("Planning".into(), "http://127.0.0.1:1".into())].into();
        let registry = ServiceRegistry::from_map(urls);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let job = sample_job();
        store.save_job(&job).await.unwrap();

        let outcome = dispatch_stage(&http, &registry, &store, "test-1", "Planning").await;
        assert!(
            matches!(outcome, DispatchOutcome::ServiceUnavailable(_)),
            "expected ServiceUnavailable from health check, got {outcome:?}"
        );

        // Job status should NOT have been changed to Running since
        // the health check fails before we update status.
        let job = store.get_job("test-1").await.unwrap().unwrap();
        assert!(
            matches!(job.status, JobStatus::Queued),
            "expected Queued (unchanged), got {}",
            job.status
        );
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
