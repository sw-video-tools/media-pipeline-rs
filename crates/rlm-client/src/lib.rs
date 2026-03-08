use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct RlmClient {
    pub base_url: String,
    pub http: Client,
}

#[derive(Debug, Serialize)]
pub struct QueryRequest {
    pub question: String,
}

#[derive(Debug, Deserialize)]
pub struct QueryResponse {
    pub answer: String,
}

impl RlmClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self { base_url: base_url.into(), http: Client::new() }
    }

    pub async fn query(&self, question: &str) -> Result<QueryResponse> {
        let _ = question;
        Ok(QueryResponse { answer: "stubbed RLM response".into() })
    }
}
