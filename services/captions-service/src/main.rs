//! Captions service: generates SRT caption files from narration scripts
//! with word-level timing estimates.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use pipeline_types::NarrationScript;

#[derive(Clone)]
struct AppState {
    output_dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct CaptionResult {
    job_id: String,
    srt_path: String,
    cue_count: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let output_dir: PathBuf = std::env::var("CAPTIONS_OUTPUT_DIR")
        .unwrap_or_else(|_| "./data/captions".into())
        .into();
    std::fs::create_dir_all(&output_dir)?;

    let state = AppState { output_dir };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/captions", post(generate_captions))
        .with_state(state);

    let addr: SocketAddr = std::env::var("CAPTIONS_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3196".into())
        .parse()?;
    info!("captions-service listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn generate_captions(
    State(state): State<AppState>,
    Json(script): Json<NarrationScript>,
) -> Result<Json<CaptionResult>, StatusCode> {
    let srt_content = script_to_srt(&script);
    let cue_count = script.segments.len();

    let srt_path = state.output_dir.join(format!("{}.srt", script.job_id));
    tokio::fs::write(&srt_path, &srt_content)
        .await
        .map_err(|e| {
            error!(error = %e, "failed to write SRT file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    info!(
        job_id = %script.job_id,
        cues = cue_count,
        path = %srt_path.display(),
        "captions generated"
    );

    Ok(Json(CaptionResult {
        job_id: script.job_id,
        srt_path: srt_path.to_string_lossy().into_owned(),
        cue_count,
    }))
}

/// Convert a narration script into SRT format with estimated timing.
fn script_to_srt(script: &NarrationScript) -> String {
    let mut srt = String::new();
    let mut offset_seconds: u32 = 0;

    for (i, seg) in script.segments.iter().enumerate() {
        let start = format_srt_time(offset_seconds);
        let end = format_srt_time(offset_seconds + seg.estimated_duration_seconds);

        srt.push_str(&format!(
            "{}\n{start} --> {end}\n{}\n\n",
            i + 1,
            seg.narration_text,
        ));

        offset_seconds += seg.estimated_duration_seconds;
    }

    srt
}

/// Format seconds as SRT timecode: HH:MM:SS,mmm
fn format_srt_time(total_seconds: u32) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02},000")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_types::ScriptSegment;

    fn sample_script() -> NarrationScript {
        NarrationScript {
            job_id: "test-1".into(),
            title: "Test".into(),
            total_duration_seconds: 35,
            segments: vec![
                ScriptSegment {
                    segment_number: 1,
                    title: "Intro".into(),
                    narration_text: "Welcome to the show.".into(),
                    estimated_duration_seconds: 5,
                    visual_notes: "title card".into(),
                },
                ScriptSegment {
                    segment_number: 2,
                    title: "Main".into(),
                    narration_text: "Let's talk about Rust.".into(),
                    estimated_duration_seconds: 30,
                    visual_notes: "code".into(),
                },
            ],
        }
    }

    #[test]
    fn srt_format() {
        let srt = script_to_srt(&sample_script());
        assert!(srt.contains("1\n00:00:00,000 --> 00:00:05,000"));
        assert!(srt.contains("2\n00:00:05,000 --> 00:00:35,000"));
        assert!(srt.contains("Welcome to the show."));
    }

    #[test]
    fn format_time_zero() {
        assert_eq!(format_srt_time(0), "00:00:00,000");
    }

    #[test]
    fn format_time_large() {
        assert_eq!(format_srt_time(3661), "01:01:01,000");
    }
}
