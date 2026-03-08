//! TTS service: converts narration script segments into audio files
//! via the OpenAI TTS API.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use pipeline_types::NarrationScript;
use provider_router::ProviderRouter;

#[derive(Clone)]
struct AppState {
    router: Arc<ProviderRouter>,
    http: reqwest::Client,
    output_dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TtsResult {
    pub job_id: String,
    pub segment_files: Vec<SegmentAudio>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SegmentAudio {
    pub segment_number: u32,
    pub file_path: String,
}

#[derive(Serialize)]
struct TtsApiRequest {
    model: String,
    input: String,
    voice: String,
    response_format: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let models_path: PathBuf = std::env::var("MODELS_CONFIG")
        .unwrap_or_else(|_| "./config/models.toml".into())
        .into();
    let router = ProviderRouter::from_file(&models_path)?;

    let output_dir: PathBuf = std::env::var("TTS_OUTPUT_DIR")
        .unwrap_or_else(|_| "./data/tts".into())
        .into();
    std::fs::create_dir_all(&output_dir)?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let state = AppState {
        router: Arc::new(router),
        http,
        output_dir,
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/tts", post(generate_tts))
        .with_state(state);

    let addr: SocketAddr = std::env::var("TTS_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3004".into())
        .parse()?;
    info!("tts-service listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn generate_tts(
    State(state): State<AppState>,
    Json(script): Json<NarrationScript>,
) -> Result<Json<TtsResult>, StatusCode> {
    let route = state
        .router
        .route_for("tts")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
        error!("OPENAI_API_KEY not set");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut segment_files = Vec::new();

    for seg in &script.segments {
        let file_name = format!("{}_seg{:03}.mp3", script.job_id, seg.segment_number);
        let file_path = state.output_dir.join(&file_name);

        let tts_req = TtsApiRequest {
            model: route.model.clone(),
            input: seg.narration_text.clone(),
            voice: "alloy".into(),
            response_format: "mp3".into(),
        };

        let resp = state
            .http
            .post("https://api.openai.com/v1/audio/speech")
            .bearer_auth(&api_key)
            .json(&tts_req)
            .send()
            .await
            .map_err(|e| {
                error!(error = %e, segment = seg.segment_number, "TTS request failed");
                StatusCode::BAD_GATEWAY
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            error!(%status, segment = seg.segment_number, "TTS API error");
            return Err(StatusCode::BAD_GATEWAY);
        }

        let bytes = resp.bytes().await.map_err(|e| {
            error!(error = %e, "failed to read TTS response body");
            StatusCode::BAD_GATEWAY
        })?;

        tokio::fs::write(&file_path, &bytes).await.map_err(|e| {
            error!(error = %e, path = %file_path.display(), "failed to write audio file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        info!(
            segment = seg.segment_number,
            path = %file_path.display(),
            bytes = bytes.len(),
            "TTS segment generated"
        );

        segment_files.push(SegmentAudio {
            segment_number: seg.segment_number,
            file_path: file_path.to_string_lossy().into_owned(),
        });
    }

    Ok(Json(TtsResult {
        job_id: script.job_id,
        segment_files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tts_api_request_serializes() {
        let req = TtsApiRequest {
            model: "gpt-4o-mini-tts".into(),
            input: "Hello world".into(),
            voice: "alloy".into(),
            response_format: "mp3".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("gpt-4o-mini-tts"));
        assert!(json.contains("alloy"));
    }

    #[test]
    fn tts_result_roundtrips() {
        let result = TtsResult {
            job_id: "test-1".into(),
            segment_files: vec![SegmentAudio {
                segment_number: 1,
                file_path: "/tmp/test_seg001.mp3".into(),
            }],
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: TtsResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.job_id, "test-1");
        assert_eq!(parsed.segment_files.len(), 1);
    }
}
