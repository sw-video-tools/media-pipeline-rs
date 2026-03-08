//! Script service: transforms a ProjectPlan + research facts into a
//! narration script with timed segments and visual cues.

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

use pipeline_types::{NarrationScript, ProjectPlan, ResearchPacket, ScriptSegment};
use provider_router::ProviderRouter;

#[derive(Clone)]
struct AppState {
    router: Arc<ProviderRouter>,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct ScriptRequest {
    plan: ProjectPlan,
    #[serde(default)]
    research: Vec<ResearchPacket>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessageResp,
}

#[derive(Deserialize)]
struct ChatMessageResp {
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let models_path: PathBuf = std::env::var("MODELS_CONFIG")
        .unwrap_or_else(|_| "./config/models.toml".into())
        .into();
    let router = ProviderRouter::from_file(&models_path)?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let state = AppState {
        router: Arc::new(router),
        http,
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/script", post(create_script))
        .with_state(state);

    let addr: SocketAddr = std::env::var("SCRIPT_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3003".into())
        .parse()?;
    info!("script-service listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn create_script(
    State(state): State<AppState>,
    Json(req): Json<ScriptRequest>,
) -> Result<Json<NarrationScript>, StatusCode> {
    let route = state
        .router
        .route_for("script")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let user_prompt = build_script_prompt(&req.plan, &req.research);

    let chat_req = ChatRequest {
        model: route.model.clone(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: prompt_templates::PLANNER_SYSTEM_PROMPT.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: user_prompt,
            },
        ],
        temperature: 0.7,
    };

    let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
        error!("OPENAI_API_KEY not set");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let resp = state
        .http
        .post("https://api.openai.com/v1/chat/completions")
        .bearer_auth(&api_key)
        .json(&chat_req)
        .send()
        .await
        .map_err(|e| {
            error!(error = %e, "LLM request failed");
            StatusCode::BAD_GATEWAY
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        error!(%status, %body, "LLM returned error");
        return Err(StatusCode::BAD_GATEWAY);
    }

    let chat_resp: ChatResponse = resp.json().await.map_err(|e| {
        error!(error = %e, "failed to parse LLM response");
        StatusCode::BAD_GATEWAY
    })?;

    let content = chat_resp
        .choices
        .first()
        .map(|c| c.message.content.as_str())
        .unwrap_or("");

    let script = parse_script_response(&req.plan, content);
    info!(
        job_id = %script.job_id,
        segments = script.segments.len(),
        "script created"
    );
    Ok(Json(script))
}

fn build_script_prompt(plan: &ProjectPlan, research: &[ResearchPacket]) -> String {
    let mut prompt = format!(
        "Write a narration script for this video:\n\n\
         Title: {title}\nSynopsis: {synopsis}\n\
         Tone: {tone}\nTarget Duration: {dur}s\n\n\
         Segments:\n",
        title = plan.title,
        synopsis = plan.synopsis,
        tone = plan.narration_tone,
        dur = plan.total_duration_seconds,
    );

    for seg in &plan.segments {
        prompt.push_str(&format!(
            "- Segment {}: {} ({dur}s) - {}\n",
            seg.segment_number,
            seg.title,
            seg.description,
            dur = seg.duration_seconds,
        ));
    }

    if !research.is_empty() {
        prompt.push_str("\nResearch facts to incorporate:\n");
        for packet in research {
            for fact in &packet.facts {
                prompt.push_str(&format!("- {} (source: {})\n", fact.claim, fact.source));
            }
            if packet.facts.is_empty() {
                prompt.push_str(&format!("- {}\n", packet.summary));
            }
        }
    }

    prompt.push_str(
        "\nRespond with a JSON object containing:\n\
         - \"segments\": array of {{ \"segment_number\", \"title\", \
         \"narration_text\", \"estimated_duration_seconds\", \"visual_notes\" }}\n",
    );
    prompt
}

/// Parse LLM response into a NarrationScript. Falls back to segments
/// derived from the plan if JSON parsing fails.
fn parse_script_response(plan: &ProjectPlan, content: &str) -> NarrationScript {
    let json_str = extract_json_block(content);

    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
        let segments: Vec<ScriptSegment> = parsed
            .get("segments")
            .and_then(|s| serde_json::from_value(s.clone()).ok())
            .unwrap_or_default();

        if !segments.is_empty() {
            return NarrationScript {
                job_id: plan.job_id.clone(),
                title: plan.title.clone(),
                total_duration_seconds: segments.iter().map(|s| s.estimated_duration_seconds).sum(),
                segments,
            };
        }
    }

    // Fallback: convert plan segments into script segments
    NarrationScript {
        job_id: plan.job_id.clone(),
        title: plan.title.clone(),
        total_duration_seconds: plan.total_duration_seconds,
        segments: plan
            .segments
            .iter()
            .map(|s| ScriptSegment {
                segment_number: s.segment_number,
                title: s.title.clone(),
                narration_text: content.to_string(),
                estimated_duration_seconds: s.duration_seconds,
                visual_notes: s.visual_style.clone(),
            })
            .collect(),
    }
}

fn extract_json_block(s: &str) -> &str {
    if let Some(start) = s.find("```json") {
        let json_start = start + 7;
        if let Some(end) = s[json_start..].find("```") {
            return s[json_start..json_start + end].trim();
        }
    }
    if let Some(start) = s.find("```") {
        let json_start = start + 3;
        let json_start = s[json_start..]
            .find('\n')
            .map(|n| json_start + n + 1)
            .unwrap_or(json_start);
        if let Some(end) = s[json_start..].find("```") {
            return s[json_start..json_start + end].trim();
        }
    }
    s.trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_types::PlannedSegment;

    fn sample_plan() -> ProjectPlan {
        ProjectPlan {
            job_id: "test-001".into(),
            title: "Test Video".into(),
            synopsis: "A test video".into(),
            total_duration_seconds: 120,
            segments: vec![
                PlannedSegment {
                    segment_number: 1,
                    title: "Intro".into(),
                    duration_seconds: 30,
                    description: "Introduction".into(),
                    visual_style: "talking head".into(),
                },
                PlannedSegment {
                    segment_number: 2,
                    title: "Main".into(),
                    duration_seconds: 90,
                    description: "Main content".into(),
                    visual_style: "diagrams".into(),
                },
            ],
            research_queries: vec![],
            narration_tone: "informative".into(),
        }
    }

    #[test]
    fn parse_valid_script_json() {
        let json = r#"{
            "segments": [
                {
                    "segment_number": 1,
                    "title": "Intro",
                    "narration_text": "Welcome to this video...",
                    "estimated_duration_seconds": 30,
                    "visual_notes": "Title card"
                },
                {
                    "segment_number": 2,
                    "title": "Main",
                    "narration_text": "Let's dive into the main topic...",
                    "estimated_duration_seconds": 90,
                    "visual_notes": "Code walkthrough"
                }
            ]
        }"#;

        let script = parse_script_response(&sample_plan(), json);
        assert_eq!(script.segments.len(), 2);
        assert_eq!(script.total_duration_seconds, 120);
        assert!(script.segments[0].narration_text.contains("Welcome"));
    }

    #[test]
    fn fallback_on_unparseable() {
        let content = "Here is the narration for your video...";
        let script = parse_script_response(&sample_plan(), content);
        assert_eq!(script.segments.len(), 2);
        assert_eq!(script.total_duration_seconds, 120);
    }

    #[test]
    fn build_prompt_includes_plan_details() {
        let plan = sample_plan();
        let prompt = build_script_prompt(&plan, &[]);
        assert!(prompt.contains("Test Video"));
        assert!(prompt.contains("Intro"));
        assert!(prompt.contains("informative"));
    }

    #[test]
    fn build_prompt_includes_research() {
        let plan = sample_plan();
        let research = vec![ResearchPacket {
            job_id: "test-001".into(),
            query: "Rust ownership".into(),
            facts: vec![pipeline_types::ResearchFact {
                claim: "Rust prevents data races".into(),
                source: "rust-lang.org".into(),
                confidence: 0.95,
            }],
            summary: "Ownership model".into(),
        }];
        let prompt = build_script_prompt(&plan, &research);
        assert!(prompt.contains("Rust prevents data races"));
        assert!(prompt.contains("rust-lang.org"));
    }
}
