//! Stage dispatch: call downstream services and handle outcomes.

use artifact_store::ArtifactStore;
use pipeline_types::{JobStatus, Stage};
use tracing::{error, info};

use crate::registry::ServiceRegistry;

/// Outcome of a stage dispatch attempt.
#[derive(Debug)]
pub enum DispatchOutcome {
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
pub async fn check_health(http: &reqwest::Client, url: &str) -> Result<(), String> {
    match http.get(url).send().await {
        Ok(resp) if resp.status().is_success() => Ok(()),
        Ok(resp) => Err(format!("healthz returned {}", resp.status())),
        Err(e) => Err(format!("{e}")),
    }
}

/// Dispatch a single stage: call the service, update job status, return outcome.
pub async fn dispatch_stage(
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
pub async fn build_stage_request(
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

            // Include research output so the script has factual context.
            let research = store
                .get_stage_output(job_id, "Research")
                .await
                .ok()
                .flatten();
            let research_packets = research
                .as_ref()
                .and_then(|r| r["packets"].as_array().cloned())
                .or_else(|| {
                    // The research service may return a single packet at top level.
                    research.clone().map(|r| vec![r])
                })
                .unwrap_or_default();

            Ok(serde_json::json!({
                "plan": plan_body,
                "research": research_packets,
            }))
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
            // Combine TTS audio paths with script expected text.
            let tts = store.get_stage_output(job_id, "Tts").await.ok().flatten();
            let script = store
                .get_stage_output(job_id, "Script")
                .await
                .ok()
                .flatten();

            let tts_files = tts
                .as_ref()
                .and_then(|t| t["segment_files"].as_array())
                .cloned()
                .unwrap_or_default();
            let script_segs = script
                .as_ref()
                .and_then(|s| s["segments"].as_array())
                .cloned()
                .unwrap_or_default();

            let segments: Vec<serde_json::Value> = tts_files
                .iter()
                .map(|tf| {
                    let seg_num = tf["segment_number"].as_u64().unwrap_or(0) as u32;
                    let audio_path = tf["file_path"].as_str().unwrap_or_default();
                    let expected_text = script_segs
                        .iter()
                        .find(|s| s["segment_number"].as_u64().unwrap_or(0) as u32 == seg_num)
                        .and_then(|s| s["narration_text"].as_str())
                        .unwrap_or_default();
                    serde_json::json!({
                        "segment_number": seg_num,
                        "audio_path": audio_path,
                        "expected_text": expected_text,
                    })
                })
                .collect();

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
            // Pull audio paths from TTS output (segment_files[].file_path).
            let tts = store.get_stage_output(job_id, "Tts").await.ok().flatten();
            let audio_files: Vec<String> = tts
                .as_ref()
                .and_then(|t| t["segment_files"].as_array())
                .map(|files| {
                    files
                        .iter()
                        .filter_map(|f| f["file_path"].as_str().map(String::from))
                        .collect()
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
    use pipeline_types::{JobStatus, PipelineJobRequest, Stage};
    use std::collections::HashMap;

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
    async fn build_script_includes_research() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let research_output = serde_json::json!({
            "job_id": "test-1",
            "query": "test overview",
            "facts": [{"claim": "Rust is fast", "source": "rust-lang.org", "confidence": 0.95}],
            "summary": "Rust performance",
        });
        store
            .save_stage_output("test-1", "Research", &research_output)
            .await
            .unwrap();

        let job = sample_job();
        let body = build_stage_request("Script", &job, &store).await.unwrap();
        assert!(body["plan"].is_object());
        let research = body["research"].as_array().unwrap();
        assert!(!research.is_empty());
        assert_eq!(research[0]["facts"][0]["claim"], "Rust is fast");
    }

    #[tokio::test]
    async fn build_asr_uses_tts_and_script_outputs() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let tts_output = serde_json::json!({
            "job_id": "test-1",
            "segment_files": [
                {"segment_number": 1, "file_path": "/data/tts/test-1_seg001.mp3"},
                {"segment_number": 2, "file_path": "/data/tts/test-1_seg002.mp3"},
            ],
        });
        let script_output = serde_json::json!({
            "job_id": "test-1",
            "segments": [
                {"segment_number": 1, "narration_text": "Hello world"},
                {"segment_number": 2, "narration_text": "Goodbye world"},
            ],
        });
        store
            .save_stage_output("test-1", "Tts", &tts_output)
            .await
            .unwrap();
        store
            .save_stage_output("test-1", "Script", &script_output)
            .await
            .unwrap();

        let job = sample_job();
        let body = build_stage_request("AsrValidation", &job, &store)
            .await
            .unwrap();
        let segments = body["segments"].as_array().unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0]["audio_path"], "/data/tts/test-1_seg001.mp3");
        assert_eq!(segments[0]["expected_text"], "Hello world");
        assert_eq!(segments[1]["audio_path"], "/data/tts/test-1_seg002.mp3");
        assert_eq!(segments[1]["expected_text"], "Goodbye world");
    }

    #[tokio::test]
    async fn build_render_uses_tts_audio_paths() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let tts_output = serde_json::json!({
            "job_id": "test-1",
            "segment_files": [
                {"segment_number": 1, "file_path": "/data/tts/test-1_seg001.mp3"},
            ],
        });
        store
            .save_stage_output("test-1", "Tts", &tts_output)
            .await
            .unwrap();

        let job = sample_job();
        let body = build_stage_request("RenderFinal", &job, &store)
            .await
            .unwrap();
        let audio_files = body["audio_files"].as_array().unwrap();
        assert_eq!(audio_files.len(), 1);
        assert_eq!(audio_files[0], "/data/tts/test-1_seg001.mp3");
    }

    #[tokio::test]
    async fn build_unsupported_stage_errors() {
        let store = ArtifactStore::open_in_memory().unwrap();
        let job = sample_job();
        assert!(build_stage_request("Unknown", &job, &store).await.is_err());
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
        use axum::Router;
        use axum::routing::{get, post};

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
            .connect_timeout(std::time::Duration::from_millis(100))
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
            .connect_timeout(std::time::Duration::from_millis(100))
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
}
