//! QA service: validates rendered output using ffprobe metadata
//! and deterministic checks (duration, streams, loudness).

use std::net::SocketAddr;
use std::path::Path;

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use validator_core::ValidationIssue;

#[derive(Debug, Deserialize)]
struct QaRequest {
    job_id: String,
    output_path: String,
    expected_duration_seconds: u32,
}

#[derive(Debug, Serialize)]
struct QaReport {
    job_id: String,
    passed: bool,
    issues: Vec<ValidationIssue>,
    media_info: Option<serde_json::Value>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/qa", post(run_qa));

    let addr: SocketAddr = std::env::var("QA_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3008".into())
        .parse()?;
    info!("qa-service listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn run_qa(Json(req): Json<QaRequest>) -> Result<Json<QaReport>, StatusCode> {
    let path = Path::new(&req.output_path);
    let mut issues = Vec::new();

    // Check file exists
    if !path.exists() {
        issues.extend(validator_core::require(
            false,
            "FILE_MISSING",
            &format!("output file not found: {}", req.output_path),
        ));
        return Ok(Json(QaReport {
            job_id: req.job_id,
            passed: false,
            issues,
            media_info: None,
        }));
    }

    // Probe the file
    let info = match media_tools::ffprobe::probe(path) {
        Ok(info) => info,
        Err(e) => {
            error!(error = %e, "ffprobe failed");
            issues.extend(validator_core::require(
                false,
                "PROBE_FAILED",
                &format!("ffprobe failed: {e}"),
            ));
            return Ok(Json(QaReport {
                job_id: req.job_id,
                passed: false,
                issues,
                media_info: None,
            }));
        }
    };

    let media_json = serde_json::to_value(&info).ok();

    // Duration check: within 20% of expected
    let expected = req.expected_duration_seconds as f64;
    let tolerance = expected * 0.2;
    issues.extend(validator_core::require(
        (info.duration_seconds - expected).abs() <= tolerance,
        "DURATION_MISMATCH",
        &format!(
            "duration {:.1}s outside expected {expected:.0}s +/- {tolerance:.0}s",
            info.duration_seconds
        ),
    ));

    // Must have at least one stream
    issues.extend(validator_core::require(
        !info.streams.is_empty(),
        "NO_STREAMS",
        "file has no media streams",
    ));

    // Must have audio
    let has_audio = info.streams.iter().any(|s| s.codec_type == "audio");
    issues.extend(validator_core::require(
        has_audio,
        "NO_AUDIO",
        "file has no audio stream",
    ));

    // Loudness check (if ffmpeg available)
    match media_tools::ffmpeg::measure_loudness(path) {
        Ok(lufs) => {
            issues.extend(validator_core::require(
                lufs > -30.0 && lufs < -5.0,
                "LOUDNESS_OUT_OF_RANGE",
                &format!("integrated loudness {lufs:.1} LUFS outside acceptable range (-30 to -5)"),
            ));
        }
        Err(e) => {
            warn!(error = %e, "loudness measurement skipped");
        }
    }

    let passed = issues.is_empty();
    info!(job_id = %req.job_id, passed, issue_count = issues.len(), "QA complete");

    Ok(Json(QaReport {
        job_id: req.job_id,
        passed,
        issues,
        media_info: media_json,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qa_request_deserializes() {
        let json = r#"{
            "job_id": "test-1",
            "output_path": "/tmp/test.mp4",
            "expected_duration_seconds": 120
        }"#;
        let req: QaRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.expected_duration_seconds, 120);
    }

    #[test]
    fn qa_report_serializes() {
        let report = QaReport {
            job_id: "test-1".into(),
            passed: true,
            issues: vec![],
            media_info: None,
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"passed\":true"));
    }

    #[test]
    fn duration_validation() {
        // Within tolerance
        let ok =
            validator_core::require((100.0_f64 - 120.0).abs() <= 120.0 * 0.2, "DURATION", "msg");
        assert!(ok.is_empty());

        // Outside tolerance
        let fail =
            validator_core::require((50.0_f64 - 120.0).abs() <= 120.0 * 0.2, "DURATION", "msg");
        assert_eq!(fail.len(), 1);
    }
}
