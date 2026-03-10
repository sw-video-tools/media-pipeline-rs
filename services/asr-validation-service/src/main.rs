//! ASR validation service: transcribes TTS audio and compares the
//! transcript against the original narration script to catch errors.

use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use validator_core::ValidationIssue;

#[derive(Clone)]
struct AppState {
    http: reqwest::Client,
}

/// Input: audio file path + expected narration text.
#[derive(Debug, Deserialize)]
struct ValidationRequest {
    job_id: String,
    segments: Vec<SegmentValidation>,
}

#[derive(Debug, Deserialize)]
struct SegmentValidation {
    segment_number: u32,
    audio_path: String,
    expected_text: String,
}

/// Output: validation report per segment.
#[derive(Debug, Serialize)]
struct ValidationReport {
    job_id: String,
    passed: bool,
    segments: Vec<SegmentReport>,
}

#[derive(Debug, Serialize)]
struct SegmentReport {
    segment_number: u32,
    transcript: String,
    similarity: f64,
    issues: Vec<ValidationIssue>,
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let state = AppState { http };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/validate", post(validate_asr))
        .with_state(state);

    let addr: SocketAddr = std::env::var("ASR_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3195".into())
        .parse()?;
    info!("asr-validation-service listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn validate_asr(
    State(state): State<AppState>,
    Json(req): Json<ValidationRequest>,
) -> Result<Json<ValidationReport>, StatusCode> {
    let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
        error!("OPENAI_API_KEY not set");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut segments = Vec::new();
    let mut all_passed = true;

    for seg in &req.segments {
        let transcript = transcribe_audio(&state.http, &api_key, &seg.audio_path)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, segment = seg.segment_number, "transcription failed, using empty");
                String::new()
            });

        let similarity = text_similarity(&seg.expected_text, &transcript);
        let mut issues = validator_core::require(
            similarity >= 0.85,
            "ASR_MISMATCH",
            &format!("transcript similarity {similarity:.2} below threshold 0.85"),
        );

        if transcript.is_empty() {
            issues.extend(validator_core::require(
                false,
                "ASR_EMPTY",
                "transcription returned empty text",
            ));
        }

        if !issues.is_empty() {
            all_passed = false;
        }

        segments.push(SegmentReport {
            segment_number: seg.segment_number,
            transcript,
            similarity,
            issues,
        });
    }

    info!(
        job_id = %req.job_id,
        passed = all_passed,
        segments = segments.len(),
        "ASR validation complete"
    );

    Ok(Json(ValidationReport {
        job_id: req.job_id,
        passed: all_passed,
        segments,
    }))
}

async fn transcribe_audio(
    http: &reqwest::Client,
    api_key: &str,
    audio_path: &str,
) -> anyhow::Result<String> {
    let file_bytes = tokio::fs::read(audio_path).await?;
    let file_name = std::path::Path::new(audio_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let part = reqwest::multipart::Part::bytes(file_bytes)
        .file_name(file_name)
        .mime_str("audio/mpeg")?;

    let form = reqwest::multipart::Form::new()
        .text("model", "gpt-4o-transcribe")
        .part("file", part);

    let resp = http
        .post("https://api.openai.com/v1/audio/transcriptions")
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await?;

    let body: TranscriptionResponse = resp.json().await?;
    Ok(body.text)
}

/// Simple word-level similarity between two texts (Jaccard index).
fn text_similarity(expected: &str, actual: &str) -> f64 {
    fn normalize(w: &str) -> &str {
        w.trim_matches(|c: char| !c.is_alphanumeric())
    }
    let expected_words: std::collections::HashSet<&str> =
        expected.split_whitespace().map(normalize).collect();
    let actual_words: std::collections::HashSet<&str> =
        actual.split_whitespace().map(normalize).collect();

    if expected_words.is_empty() && actual_words.is_empty() {
        return 1.0;
    }
    if expected_words.is_empty() || actual_words.is_empty() {
        return 0.0;
    }

    let intersection = expected_words.intersection(&actual_words).count();
    let union = expected_words.union(&actual_words).count();
    intersection as f64 / union as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_score_one() {
        assert!((text_similarity("hello world", "hello world") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn completely_different_texts_score_zero() {
        assert!((text_similarity("hello world", "foo bar")).abs() < f64::EPSILON);
    }

    #[test]
    fn partial_overlap_scores_between() {
        let score = text_similarity("the quick brown fox", "the slow brown dog");
        assert!(score > 0.0 && score < 1.0);
    }

    #[test]
    fn empty_texts_score_one() {
        assert!((text_similarity("", "") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn validation_issue_from_require() {
        let issues = validator_core::require(false, "TEST", "test message");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "TEST");

        let no_issues = validator_core::require(true, "TEST", "test message");
        assert!(no_issues.is_empty());
    }
}
