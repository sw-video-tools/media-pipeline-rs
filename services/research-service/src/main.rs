//! Research service: queries the RLM service for each research query
//! in a project plan and produces research packets with sourced facts.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tracing::{error, info};

use pipeline_types::{ResearchFact, ResearchPacket};
use rlm_client::RlmClient;

#[derive(Clone)]
struct AppState {
    rlm: Arc<RlmClient>,
}

/// Request body: job_id + list of research queries from the planner.
#[derive(Debug, Deserialize)]
struct ResearchRequest {
    job_id: String,
    queries: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let rlm_url = std::env::var("RLM_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());

    let state = AppState {
        rlm: Arc::new(RlmClient::new(rlm_url)),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/research", post(do_research))
        .with_state(state);

    let addr: SocketAddr = std::env::var("RESEARCH_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3192".into())
        .parse()?;
    info!("research-service listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn do_research(
    State(state): State<AppState>,
    Json(req): Json<ResearchRequest>,
) -> Result<Json<Vec<ResearchPacket>>, StatusCode> {
    let mut packets = Vec::with_capacity(req.queries.len());

    for query in &req.queries {
        let resp = state.rlm.ask(query).await.map_err(|e| {
            error!(error = %e, query = %query, "RLM query failed");
            StatusCode::BAD_GATEWAY
        })?;

        let facts: Vec<ResearchFact> = resp
            .evidence
            .into_iter()
            .map(|ev| ResearchFact {
                claim: ev.snippet,
                source: ev.source,
                confidence: ev.relevance_score,
            })
            .collect();

        packets.push(ResearchPacket {
            job_id: req.job_id.clone(),
            query: query.clone(),
            facts,
            summary: resp.answer,
        });
    }

    info!(
        job_id = %req.job_id,
        packet_count = packets.len(),
        "research complete"
    );
    Ok(Json(packets))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_rlm() -> RlmClient {
        // Points to unreachable address; triggers stub fallback
        RlmClient::new("http://127.0.0.1:1")
    }

    #[tokio::test]
    async fn research_with_stub_rlm() {
        let state = AppState {
            rlm: Arc::new(stub_rlm()),
        };
        let app = Router::new()
            .route("/research", post(do_research))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/research"))
            .json(&serde_json::json!({
                "job_id": "test-1",
                "queries": ["What is Rust?", "Memory safety"]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let packets: Vec<ResearchPacket> = resp.json().await.unwrap();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].job_id, "test-1");
        assert!(packets[0].summary.contains("Stub"));
    }
}
