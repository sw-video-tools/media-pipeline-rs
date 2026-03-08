//! Config-driven model routing for pipeline capabilities.
//!
//! Reads a TOML file (e.g. `config/models.toml`) that maps capability
//! names to provider + model pairs. Unknown capabilities fall back to
//! a configurable default.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tracing::info;

/// A resolved provider + model pair for a given capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRoute {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Deserialize)]
struct RouteEntry {
    provider: String,
    model: String,
}

/// Routes capability names (e.g. "planning", "tts") to model providers.
#[derive(Debug)]
pub struct ProviderRouter {
    routes: HashMap<String, ModelRoute>,
    default_provider: String,
    default_model: String,
}

impl ProviderRouter {
    /// Load routes from a TOML file. Each top-level key is a capability.
    ///
    /// ```toml
    /// [planning]
    /// provider = "openai"
    /// model = "gpt-4.1"
    /// ```
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read models config at {}", path.display()))?;
        Self::from_toml(&contents)
    }

    /// Parse routes from a TOML string.
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        let table: HashMap<String, RouteEntry> =
            toml::from_str(toml_str).context("failed to parse models TOML")?;

        let routes: HashMap<String, ModelRoute> = table
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    ModelRoute {
                        provider: v.provider,
                        model: v.model,
                    },
                )
            })
            .collect();

        info!(count = routes.len(), "loaded model routes");

        Ok(Self {
            routes,
            default_provider: "openai".into(),
            default_model: "gpt-4.1-mini".into(),
        })
    }

    /// Set the fallback provider and model for unknown capabilities.
    pub fn with_default(mut self, provider: &str, model: &str) -> Self {
        self.default_provider = provider.into();
        self.default_model = model.into();
        self
    }

    /// Resolve the model route for a capability name.
    pub fn route_for(&self, capability: &str) -> Result<ModelRoute> {
        match self.routes.get(capability) {
            Some(route) => Ok(route.clone()),
            None => {
                info!(
                    capability,
                    provider = %self.default_provider,
                    model = %self.default_model,
                    "no explicit route, using default"
                );
                Ok(ModelRoute {
                    provider: self.default_provider.clone(),
                    model: self.default_model.clone(),
                })
            }
        }
    }

    /// Resolve the model route, failing if no explicit route exists.
    pub fn route_for_strict(&self, capability: &str) -> Result<ModelRoute> {
        match self.routes.get(capability) {
            Some(route) => Ok(route.clone()),
            None => bail!("no route configured for capability '{capability}'"),
        }
    }

    /// List all configured capabilities.
    pub fn capabilities(&self) -> Vec<&str> {
        self.routes.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TOML: &str = r#"
[planning]
provider = "openai"
model = "gpt-4.1"

[tts]
provider = "openai"
model = "gpt-4o-mini-tts"

[asr]
provider = "openai"
model = "gpt-4o-transcribe"
"#;

    #[test]
    fn parse_routes_from_toml() {
        let router = ProviderRouter::from_toml(SAMPLE_TOML).unwrap();
        assert_eq!(router.routes.len(), 3);
    }

    #[test]
    fn route_for_known_capability() {
        let router = ProviderRouter::from_toml(SAMPLE_TOML).unwrap();
        let route = router.route_for("planning").unwrap();
        assert_eq!(route.provider, "openai");
        assert_eq!(route.model, "gpt-4.1");
    }

    #[test]
    fn route_for_unknown_returns_default() {
        let router = ProviderRouter::from_toml(SAMPLE_TOML).unwrap();
        let route = router.route_for("unknown-cap").unwrap();
        assert_eq!(route.provider, "openai");
        assert_eq!(route.model, "gpt-4.1-mini");
    }

    #[test]
    fn route_for_strict_fails_on_unknown() {
        let router = ProviderRouter::from_toml(SAMPLE_TOML).unwrap();
        let result = router.route_for_strict("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn custom_default() {
        let router = ProviderRouter::from_toml(SAMPLE_TOML)
            .unwrap()
            .with_default("anthropic", "claude-sonnet-4-6");
        let route = router.route_for("something").unwrap();
        assert_eq!(route.provider, "anthropic");
        assert_eq!(route.model, "claude-sonnet-4-6");
    }

    #[test]
    fn capabilities_lists_all() {
        let router = ProviderRouter::from_toml(SAMPLE_TOML).unwrap();
        let caps = router.capabilities();
        assert_eq!(caps.len(), 3);
        assert!(caps.contains(&"planning"));
        assert!(caps.contains(&"tts"));
        assert!(caps.contains(&"asr"));
    }

    #[test]
    fn from_file_loads_config() {
        let path = Path::new("../../config/models.toml");
        if path.exists() {
            let router = ProviderRouter::from_file(path).unwrap();
            assert!(router.routes.len() >= 3);
        }
    }
}
