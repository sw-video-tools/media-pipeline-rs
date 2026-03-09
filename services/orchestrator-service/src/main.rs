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
use tracing::{error, info};

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
            std::env::var("PLANNER_URL").unwrap_or_else(|_| "http://127.0.0.1:3001".into()),
        );
        urls.insert(
            "Research".into(),
            std::env::var("RESEARCH_URL").unwrap_or_else(|_| "http://127.0.0.1:3002".into()),
        );
        urls.insert(
            "Script".into(),
            std::env::var("SCRIPT_URL").unwrap_or_else(|_| "http://127.0.0.1:3003".into()),
        );
        urls.insert(
            "Tts".into(),
            std::env::var("TTS_URL").unwrap_or_else(|_| "http://127.0.0.1:3004".into()),
        );
        urls.insert(
            "AsrValidation".into(),
            std::env::var("ASR_URL").unwrap_or_else(|_| "http://127.0.0.1:3005".into()),
        );
        urls.insert(
            "Captions".into(),
            std::env::var("CAPTIONS_URL").unwrap_or_else(|_| "http://127.0.0.1:3006".into()),
        );
        urls.insert(
            "RenderFinal".into(),
            std::env::var("RENDER_URL").unwrap_or_else(|_| "http://127.0.0.1:3007".into()),
        );
        urls.insert(
            "QaFinal".into(),
            std::env::var("QA_URL").unwrap_or_else(|_| "http://127.0.0.1:3008".into()),
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
        .unwrap_or_else(|_| "0.0.0.0:3010".into())
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

/// Poll loop: dequeue jobs and dispatch to services.
async fn poll_loop(state: AppState) {
    let poll_interval = Duration::from_secs(
        std::env::var("POLL_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2),
    );

    loop {
        match state.queue.dequeue().await {
            Ok(Some(entry)) => {
                info!(
                    job_id = %entry.job_id,
                    stage = %entry.stage,
                    "processing queue entry"
                );

                let result = dispatch_stage(
                    &state.http,
                    &state.registry,
                    &state.store,
                    &entry.job_id,
                    &entry.stage,
                )
                .await;

                match result {
                    Ok(next_stage) => {
                        // Acknowledge the completed entry
                        if let Err(e) = state.queue.acknowledge(entry.entry_id).await {
                            error!(error = %e, "failed to acknowledge queue entry");
                        }

                        // Enqueue next stage if pipeline continues
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
                    Err(e) => {
                        error!(
                            job_id = %entry.job_id,
                            stage = %entry.stage,
                            error = %e,
                            "stage dispatch failed, marking job as Failed"
                        );
                        if let Err(ue) = state
                            .store
                            .update_job_status(&entry.job_id, &JobStatus::Failed, &Stage::Failed)
                            .await
                        {
                            error!(error = %ue, "failed to update job status to Failed");
                        }
                        // Still acknowledge so the entry doesn't block the queue
                        let _ = state.queue.acknowledge(entry.entry_id).await;
                    }
                }
            }
            Ok(None) => {
                // Nothing in queue, sleep
                tokio::time::sleep(poll_interval).await;
            }
            Err(e) => {
                error!(error = %e, "queue dequeue error");
                tokio::time::sleep(poll_interval).await;
            }
        }
    }
}

/// Dispatch a single stage: call the service, update job status, return next stage.
async fn dispatch_stage(
    http: &reqwest::Client,
    registry: &ServiceRegistry,
    store: &ArtifactStore,
    job_id: &str,
    stage: &str,
) -> anyhow::Result<Option<Stage>> {
    let current_stage: Stage = serde_json::from_str(&format!("\"{stage}\""))?;

    // Mark job as Running
    store
        .update_job_status(job_id, &JobStatus::Running, &current_stage)
        .await?;

    let endpoint = registry
        .endpoint_for(stage)
        .ok_or_else(|| anyhow::anyhow!("no endpoint configured for stage '{stage}'"))?;

    info!(job_id, stage, endpoint = %endpoint, "calling service");

    // Build the request body from the job data
    let job = store
        .get_job(job_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("job '{job_id}' not found in store"))?;

    let body = build_stage_request(stage, &job)?;

    let resp = http
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("HTTP request to {endpoint} failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("service {stage} returned {status}: {text}");
    }

    info!(job_id, stage, "stage completed successfully");

    // Determine next stage
    let next = current_stage.next_mvp();

    // Update job status
    match &next {
        Some(next_stage) => {
            store
                .update_job_status(job_id, &JobStatus::Running, next_stage)
                .await?;
        }
        None => {
            // Pipeline complete or failed
            if matches!(current_stage, Stage::Complete | Stage::Failed) {
                // Already terminal
            } else {
                store
                    .update_job_status(job_id, &JobStatus::Completed, &Stage::Complete)
                    .await?;
            }
        }
    }

    Ok(next)
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
