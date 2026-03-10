//! Request handlers for the API gateway routes.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::Utc;
use pipeline_types::{JobStatus, PipelineJob, PipelineJobRequest, Stage};
use serde::Serialize;
use tracing::info;
use uuid::Uuid;

use crate::AppState;
use crate::errors::ApiError;
use crate::extract::JsonBody;

pub async fn healthz() -> &'static str {
    "ok"
}

pub fn validate_request(req: &PipelineJobRequest) -> Result<(), ApiError> {
    let title = req.title.trim();
    if title.is_empty() {
        return Err(ApiError::bad_request("title must not be empty"));
    }
    if title.len() > 200 {
        return Err(ApiError::bad_request(
            "title must be 200 characters or fewer",
        ));
    }
    if req.idea.trim().is_empty() {
        return Err(ApiError::bad_request("idea must not be empty"));
    }
    if req.audience.trim().is_empty() {
        return Err(ApiError::bad_request("audience must not be empty"));
    }
    if req.tone.trim().is_empty() {
        return Err(ApiError::bad_request("tone must not be empty"));
    }
    if req.target_duration_seconds == 0 {
        return Err(ApiError::bad_request(
            "target_duration_seconds must be greater than 0",
        ));
    }
    if req.target_duration_seconds > 3600 {
        return Err(ApiError::bad_request(
            "target_duration_seconds must be 3600 or fewer",
        ));
    }
    Ok(())
}

pub async fn create_job(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<PipelineJobRequest>,
) -> Result<(StatusCode, Json<PipelineJob>), ApiError> {
    validate_request(&req)?;

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
        .map_err(|e| ApiError::internal(format!("failed to save job: {e}")))?;
    state
        .queue
        .enqueue(&job.job_id, "Planning")
        .await
        .map_err(|e| ApiError::internal(format!("failed to enqueue job: {e}")))?;

    info!(job_id = %job.job_id, "job created");
    Ok((StatusCode::CREATED, Json(job)))
}

pub async fn list_jobs(State(state): State<AppState>) -> Result<Json<Vec<PipelineJob>>, ApiError> {
    let jobs = state
        .store
        .list_jobs()
        .await
        .map_err(|e| ApiError::internal(format!("failed to list jobs: {e}")))?;
    Ok(Json(jobs))
}

pub async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PipelineJob>, ApiError> {
    let job = state
        .store
        .get_job(&id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to get job: {e}")))?
        .ok_or_else(|| ApiError::not_found(format!("job '{id}' not found")))?;
    Ok(Json(job))
}

/// Enriched job detail with completed stage outputs.
#[derive(Debug, Serialize)]
pub struct JobDetail {
    #[serde(flatten)]
    job: PipelineJob,
    completed_stages: Vec<String>,
    stage_outputs: serde_json::Value,
}

pub async fn get_job_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobDetail>, ApiError> {
    let job = state
        .store
        .get_job(&id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to get job: {e}")))?
        .ok_or_else(|| ApiError::not_found(format!("job '{id}' not found")))?;

    let outputs = state
        .store
        .list_stage_outputs(&id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to list stage outputs: {e}")))?;

    let completed_stages: Vec<String> = outputs.iter().map(|(s, _)| s.clone()).collect();
    let stage_outputs: serde_json::Map<String, serde_json::Value> = outputs.into_iter().collect();

    Ok(Json(JobDetail {
        job,
        completed_stages,
        stage_outputs: serde_json::Value::Object(stage_outputs),
    }))
}

pub async fn get_job_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<artifact_store::JobEvent>>, ApiError> {
    let events = state
        .store
        .list_events(&id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to list events: {e}")))?;
    Ok(Json(events))
}

pub async fn retry_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let job = state
        .store
        .get_job(&id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to get job: {e}")))?
        .ok_or_else(|| ApiError::not_found(format!("job '{id}' not found")))?;

    if !matches!(job.status, JobStatus::Failed) {
        return Err(ApiError::conflict(format!(
            "job '{id}' is {} — only failed jobs can be retried",
            job.status,
        )));
    }

    // Reset to Queued and re-enqueue at the failed stage.
    let stage_str = job.current_stage.to_string();
    state
        .store
        .update_job_status(&id, &JobStatus::Queued, &job.current_stage)
        .await
        .map_err(|e| ApiError::internal(format!("failed to update job status: {e}")))?;
    state
        .queue
        .enqueue(&id, &stage_str)
        .await
        .map_err(|e| ApiError::internal(format!("failed to enqueue job: {e}")))?;

    info!(job_id = %id, stage = %stage_str, "job retried");
    Ok(StatusCode::OK)
}

pub async fn delete_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    // Remove queue entries first, then the job data.
    state
        .queue
        .remove_by_job_id(&id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to remove queue entries: {e}")))?;
    let deleted = state
        .store
        .delete_job(&id)
        .await
        .map_err(|e| ApiError::internal(format!("failed to delete job: {e}")))?;

    if deleted {
        info!(job_id = %id, "job deleted");
        Ok(StatusCode::OK)
    } else {
        Err(ApiError::not_found(format!("job '{id}' not found")))
    }
}
