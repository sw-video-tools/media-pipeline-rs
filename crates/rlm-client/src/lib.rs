//! HTTP client for the RLM (Recursive Language Model) service.
//!
//! The RLM service provides context-aware research: compact working
//! sets, evidence packets, and long-transcript exploration.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Client for the RLM research service.
#[derive(Debug, Clone)]
pub struct RlmClient {
    base_url: String,
    http: Client,
}

/// A research query sent to the RLM service.
#[derive(Debug, Serialize)]
pub struct QueryRequest {
    pub question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_results: Option<u32>,
}

/// A single evidence item from the RLM response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Evidence {
    pub source: String,
    pub snippet: String,
    pub relevance_score: f32,
}

/// Response from the RLM service.
#[derive(Debug, Deserialize, Serialize)]
pub struct QueryResponse {
    pub answer: String,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

impl RlmClient {
    /// Create a new client pointing at the given RLM base URL.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: Client::new(),
        }
    }

    /// Query the RLM service. Falls back to a stub response if the
    /// service is unreachable (local development without RLM).
    pub async fn query(&self, request: &QueryRequest) -> Result<QueryResponse> {
        let url = format!("{}/query", self.base_url);
        match self.http.post(&url).json(request).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    let body = resp
                        .json::<QueryResponse>()
                        .await
                        .context("failed to parse RLM response")?;
                    info!(
                        question = %request.question,
                        evidence_count = body.evidence.len(),
                        "RLM query succeeded"
                    );
                    Ok(body)
                } else {
                    let status = resp.status();
                    warn!(%status, "RLM returned error, using stub");
                    Ok(Self::stub_response(&request.question))
                }
            }
            Err(e) => {
                warn!(error = %e, "RLM unreachable, using stub response");
                Ok(Self::stub_response(&request.question))
            }
        }
    }

    /// Convenience method for a simple question with no context.
    pub async fn ask(&self, question: &str) -> Result<QueryResponse> {
        self.query(&QueryRequest {
            question: question.to_string(),
            context: None,
            max_results: None,
        })
        .await
    }

    fn stub_response(question: &str) -> QueryResponse {
        QueryResponse {
            answer: format!("Stub: no RLM service available to answer '{question}'"),
            evidence: vec![],
        }
    }

    /// Return the configured base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_fallback_when_unreachable() {
        let client = RlmClient::new("http://127.0.0.1:1"); // won't connect
        let resp = client.ask("What is Rust?").await.unwrap();
        assert!(resp.answer.contains("Stub"));
        assert!(resp.evidence.is_empty());
    }

    #[test]
    fn query_request_serializes() {
        let req = QueryRequest {
            question: "test".into(),
            context: Some("ctx".into()),
            max_results: Some(5),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("max_results"));
    }

    #[test]
    fn query_request_omits_none_fields() {
        let req = QueryRequest {
            question: "test".into(),
            context: None,
            max_results: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("context"));
        assert!(!json.contains("max_results"));
    }
}
