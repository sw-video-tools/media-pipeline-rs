use anyhow::Result;

#[derive(Debug, Clone)]
pub struct ModelRoute {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Default)]
pub struct ProviderRouter;

impl ProviderRouter {
    pub fn new() -> Self {
        Self
    }

    pub fn route_for(&self, capability: &str) -> Result<ModelRoute> {
        let route = match capability {
            "planning" => ModelRoute { provider: "openai".into(), model: "gpt-4.1".into() },
            "tts" => ModelRoute { provider: "openai".into(), model: "gpt-4o-mini-tts".into() },
            _ => ModelRoute { provider: "openai".into(), model: "gpt-4.1-mini".into() },
        };
        Ok(route)
    }
}
