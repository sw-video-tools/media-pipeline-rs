//! Render service: assembles final video from audio segments, images,
//! and captions using ffmpeg via media-tools.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

#[derive(Clone)]
struct AppState {
    output_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct RenderRequest {
    job_id: String,
    audio_files: Vec<String>,
    #[serde(default)]
    image_files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RenderResult {
    job_id: String,
    output_path: String,
    format: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let output_dir: PathBuf = std::env::var("RENDER_OUTPUT_DIR")
        .unwrap_or_else(|_| "./data/renders".into())
        .into();
    std::fs::create_dir_all(&output_dir)?;

    let state = AppState { output_dir };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/render", post(render_video))
        .with_state(state);

    let addr: SocketAddr = std::env::var("RENDER_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3197".into())
        .parse()?;
    info!("render-service listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn render_video(
    State(state): State<AppState>,
    Json(req): Json<RenderRequest>,
) -> Result<Json<RenderResult>, StatusCode> {
    // Step 1: Concatenate audio segments
    let concat_path = state.output_dir.join(format!("{}_audio.mp3", req.job_id));
    let audio_paths: Vec<PathBuf> = req.audio_files.iter().map(PathBuf::from).collect();
    let audio_refs: Vec<&Path> = audio_paths.iter().map(|p| p.as_path()).collect();

    if !audio_refs.is_empty() {
        media_tools::ffmpeg::concat_audio(&audio_refs, &concat_path).map_err(|e| {
            error!(error = %e, "audio concatenation failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    // Step 2: Render slideshow if images provided, otherwise output audio-only
    let output_path = if !req.image_files.is_empty() {
        let video_path = state.output_dir.join(format!("{}.mp4", req.job_id));
        let image_paths: Vec<PathBuf> = req.image_files.iter().map(PathBuf::from).collect();
        let image_refs: Vec<&Path> = image_paths.iter().map(|p| p.as_path()).collect();

        media_tools::ffmpeg::render_slideshow(&image_refs, &concat_path, &video_path, 30).map_err(
            |e| {
                error!(error = %e, "slideshow render failed");
                StatusCode::INTERNAL_SERVER_ERROR
            },
        )?;
        video_path
    } else {
        concat_path
    };

    let format = if output_path.extension().is_some_and(|e| e == "mp4") {
        "mp4"
    } else {
        "mp3"
    };

    info!(
        job_id = %req.job_id,
        path = %output_path.display(),
        "render complete"
    );

    Ok(Json(RenderResult {
        job_id: req.job_id,
        output_path: output_path.to_string_lossy().into_owned(),
        format: format.into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_request_deserializes_minimal() {
        let json = r#"{
            "job_id": "test-1",
            "audio_files": ["/tmp/seg001.mp3"]
        }"#;
        let req: RenderRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.job_id, "test-1");
        assert!(req.image_files.is_empty());
    }

    #[test]
    fn render_result_serializes() {
        let result = RenderResult {
            job_id: "test-1".into(),
            output_path: "/tmp/test-1.mp4".into(),
            format: "mp4".into(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("mp4"));
    }
}
