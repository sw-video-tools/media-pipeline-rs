//! Planner service: transforms a PipelineJobRequest into a ProjectPlan.
//!
//! POST /plan accepts a job request and calls the configured LLM to
//! produce a structured project plan with segments, research queries,
//! and narration guidance.

use std::path::PathBuf;
use std::sync::Arc;
use std::{net::SocketAddr, time::Duration};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use pipeline_types::{PipelineJobRequest, PlannedSegment, ProjectPlan};
use provider_router::ProviderRouter;

#[derive(Clone)]
struct AppState {
    router: Arc<ProviderRouter>,
    http: reqwest::Client,
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

#[derive(Deserialize, Serialize)]
struct PlanRequest {
    job_id: String,
    #[serde(flatten)]
    request: PipelineJobRequest,
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
        .route("/plan", post(create_plan))
        .with_state(state);

    let addr: SocketAddr = std::env::var("PLANNER_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3001".into())
        .parse()?;
    info!("planner-service listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn create_plan(
    State(state): State<AppState>,
    Json(req): Json<PlanRequest>,
) -> Result<Json<ProjectPlan>, StatusCode> {
    let route = state
        .router
        .route_for("planning")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let system_prompt = prompt_templates::PLANNER_SYSTEM_PROMPT;
    let user_prompt = build_user_prompt(&req);

    let chat_req = ChatRequest {
        model: route.model.clone(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: system_prompt.into(),
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

    let plan = parse_plan_response(&req, content);

    info!(job_id = %plan.job_id, segments = plan.segments.len(), "plan created");
    Ok(Json(plan))
}

fn build_user_prompt(req: &PlanRequest) -> String {
    format!(
        r#"Create a project plan for this video:

Title: {title}
Idea: {idea}
Audience: {audience}
Target Duration: {duration}s
Tone: {tone}
Must Include: {include}
Must Avoid: {avoid}

Respond with a JSON object containing:
- "synopsis": brief overview (1-2 sentences)
- "segments": array of {{ "segment_number", "title", "duration_seconds", "description", "visual_style" }}
- "research_queries": array of search queries to gather facts
- "narration_tone": guidance for the narrator"#,
        title = req.request.title,
        idea = req.request.idea,
        audience = req.request.audience,
        duration = req.request.target_duration_seconds,
        tone = req.request.tone,
        include = req.request.must_include.join(", "),
        avoid = req.request.must_avoid.join(", "),
    )
}

/// Best-effort JSON extraction from LLM output. Falls back to a
/// single-segment plan if parsing fails.
fn parse_plan_response(req: &PlanRequest, content: &str) -> ProjectPlan {
    // Try to extract JSON from the response (may be wrapped in markdown fences)
    let json_str = extract_json_block(content);

    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
        let segments: Vec<PlannedSegment> = parsed
            .get("segments")
            .and_then(|s| serde_json::from_value(s.clone()).ok())
            .unwrap_or_default();

        let research_queries: Vec<String> = parsed
            .get("research_queries")
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .unwrap_or_default();

        return ProjectPlan {
            job_id: req.job_id.clone(),
            title: req.request.title.clone(),
            synopsis: parsed
                .get("synopsis")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            total_duration_seconds: segments.iter().map(|s| s.duration_seconds).sum(),
            segments,
            research_queries,
            narration_tone: parsed
                .get("narration_tone")
                .and_then(|s| s.as_str())
                .unwrap_or(&req.request.tone)
                .to_string(),
        };
    }

    // Fallback: single segment with the raw content as description
    ProjectPlan {
        job_id: req.job_id.clone(),
        title: req.request.title.clone(),
        synopsis: content.chars().take(200).collect(),
        total_duration_seconds: req.request.target_duration_seconds,
        segments: vec![PlannedSegment {
            segment_number: 1,
            title: req.request.title.clone(),
            duration_seconds: req.request.target_duration_seconds,
            description: content.to_string(),
            visual_style: "default".into(),
        }],
        research_queries: vec![],
        narration_tone: req.request.tone.clone(),
    }
}

/// Extract JSON from a string that may be wrapped in ```json fences.
fn extract_json_block(s: &str) -> &str {
    if let Some(start) = s.find("```json") {
        let json_start = start + 7;
        if let Some(end) = s[json_start..].find("```") {
            return s[json_start..json_start + end].trim();
        }
    }
    if let Some(start) = s.find("```") {
        let json_start = start + 3;
        // Skip optional language tag on same line
        let json_start = s[json_start..]
            .find('\n')
            .map(|n| json_start + n + 1)
            .unwrap_or(json_start);
        if let Some(end) = s[json_start..].find("```") {
            return s[json_start..json_start + end].trim();
        }
    }
    // Try the whole string as JSON
    s.trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_types::PipelineJobRequest;

    fn sample_plan_request() -> PlanRequest {
        PlanRequest {
            job_id: "test-001".into(),
            request: PipelineJobRequest {
                title: "Test Video".into(),
                idea: "Explaining Rust ownership".into(),
                audience: "developers".into(),
                target_duration_seconds: 120,
                tone: "informative".into(),
                must_include: vec!["borrowing".into()],
                must_avoid: vec![],
            },
        }
    }

    #[test]
    fn parse_valid_json_response() {
        let json = r#"{
            "synopsis": "A video about Rust ownership",
            "segments": [
                {
                    "segment_number": 1,
                    "title": "Intro",
                    "duration_seconds": 30,
                    "description": "Introduction to ownership",
                    "visual_style": "code walkthrough"
                },
                {
                    "segment_number": 2,
                    "title": "Borrowing",
                    "duration_seconds": 90,
                    "description": "How borrowing works",
                    "visual_style": "diagram"
                }
            ],
            "research_queries": ["rust ownership model", "rust borrow checker"],
            "narration_tone": "friendly and technical"
        }"#;

        let plan = parse_plan_response(&sample_plan_request(), json);
        assert_eq!(plan.segments.len(), 2);
        assert_eq!(plan.total_duration_seconds, 120);
        assert_eq!(plan.research_queries.len(), 2);
        assert_eq!(plan.synopsis, "A video about Rust ownership");
    }

    #[test]
    fn parse_json_in_markdown_fences() {
        let content = r#"Here's the plan:

```json
{
    "synopsis": "Quick overview",
    "segments": [
        {
            "segment_number": 1,
            "title": "Main",
            "duration_seconds": 120,
            "description": "The whole video",
            "visual_style": "slides"
        }
    ],
    "research_queries": [],
    "narration_tone": "casual"
}
```

Let me know if you want changes."#;

        let plan = parse_plan_response(&sample_plan_request(), content);
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.synopsis, "Quick overview");
    }

    #[test]
    fn fallback_on_unparseable_response() {
        let content = "I couldn't understand your request, please try again.";
        let plan = parse_plan_response(&sample_plan_request(), content);
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.total_duration_seconds, 120);
        assert!(plan.segments[0].description.contains("couldn't understand"));
    }

    #[test]
    fn extract_json_from_fenced_block() {
        let input = "```json\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json_block(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_json_plain() {
        let input = r#"  {"key": "value"}  "#;
        assert_eq!(extract_json_block(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn build_prompt_includes_request_fields() {
        let req = sample_plan_request();
        let prompt = build_user_prompt(&req);
        assert!(prompt.contains("Test Video"));
        assert!(prompt.contains("Rust ownership"));
        assert!(prompt.contains("borrowing"));
    }
}
