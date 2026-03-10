//! Service registry: maps pipeline stages to service URLs.

use std::collections::HashMap;

/// The 8 MVP stage names, in pipeline order.
pub const STAGE_NAMES: &[&str] = &[
    "Planning",
    "Research",
    "Script",
    "Tts",
    "AsrValidation",
    "Captions",
    "RenderFinal",
    "QaFinal",
];

/// Maps stage names to service base URLs.
#[derive(Debug, Clone)]
pub struct ServiceRegistry {
    urls: HashMap<String, String>,
}

impl ServiceRegistry {
    pub fn from_env() -> Self {
        let mut urls = HashMap::new();
        urls.insert(
            "Planning".into(),
            std::env::var("PLANNER_URL").unwrap_or_else(|_| "http://127.0.0.1:3191".into()),
        );
        urls.insert(
            "Research".into(),
            std::env::var("RESEARCH_URL").unwrap_or_else(|_| "http://127.0.0.1:3192".into()),
        );
        urls.insert(
            "Script".into(),
            std::env::var("SCRIPT_URL").unwrap_or_else(|_| "http://127.0.0.1:3193".into()),
        );
        urls.insert(
            "Tts".into(),
            std::env::var("TTS_URL").unwrap_or_else(|_| "http://127.0.0.1:3194".into()),
        );
        urls.insert(
            "AsrValidation".into(),
            std::env::var("ASR_URL").unwrap_or_else(|_| "http://127.0.0.1:3195".into()),
        );
        urls.insert(
            "Captions".into(),
            std::env::var("CAPTIONS_URL").unwrap_or_else(|_| "http://127.0.0.1:3196".into()),
        );
        urls.insert(
            "RenderFinal".into(),
            std::env::var("RENDER_URL").unwrap_or_else(|_| "http://127.0.0.1:3197".into()),
        );
        urls.insert(
            "QaFinal".into(),
            std::env::var("QA_URL").unwrap_or_else(|_| "http://127.0.0.1:3198".into()),
        );
        Self { urls }
    }

    #[cfg(test)]
    pub fn from_map(urls: HashMap<String, String>) -> Self {
        Self { urls }
    }

    pub fn endpoint_for(&self, stage: &str) -> Option<String> {
        let base = self.urls.get(stage)?;
        let path = stage_to_path(stage);
        Some(format!("{base}{path}"))
    }

    pub fn healthz_url(&self, stage: &str) -> Option<String> {
        let base = self.urls.get(stage)?;
        Some(format!("{base}/healthz"))
    }
}

/// Map stage names to their service endpoint paths.
pub fn stage_to_path(stage: &str) -> &'static str {
    match stage {
        "Planning" => "/plan",
        "Research" => "/research",
        "Script" => "/script",
        "Tts" => "/tts",
        "AsrValidation" => "/validate",
        "Captions" => "/captions",
        "RenderFinal" => "/render",
        "QaFinal" => "/qa",
        _ => "/healthz",
    }
}

/// Return the max retry attempts for a stage.
///
/// GPU-bound stages (TTS, ASR, Render) get more retries because
/// their services are expected to be down more often (shared GPU host,
/// only one service can run at a time). CPU-bound stages (Planning,
/// Research, Script, Captions, QA) use the default limit.
pub fn max_attempts_for_stage(stage: &str, default: u32) -> u32 {
    // Allow per-stage overrides via env: MAX_RETRY_TTS=100, etc.
    let env_key = format!("MAX_RETRY_{}", stage.to_uppercase());
    if let Ok(val) = std::env::var(&env_key)
        && let Ok(n) = val.parse::<u32>()
    {
        return n;
    }

    match stage {
        // GPU stages: default to 2x the base limit
        "Tts" | "AsrValidation" | "RenderFinal" => default.saturating_mul(2),
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_to_path_maps_correctly() {
        assert_eq!(stage_to_path("Planning"), "/plan");
        assert_eq!(stage_to_path("Research"), "/research");
        assert_eq!(stage_to_path("Script"), "/script");
        assert_eq!(stage_to_path("Tts"), "/tts");
        assert_eq!(stage_to_path("AsrValidation"), "/validate");
        assert_eq!(stage_to_path("Captions"), "/captions");
        assert_eq!(stage_to_path("RenderFinal"), "/render");
        assert_eq!(stage_to_path("QaFinal"), "/qa");
    }

    #[test]
    fn service_registry_builds_endpoints() {
        let registry = ServiceRegistry::from_env();
        let endpoint = registry.endpoint_for("Planning").unwrap();
        assert!(endpoint.ends_with("/plan"));
    }

    #[test]
    fn gpu_stages_get_more_retries() {
        let default = 50;
        assert_eq!(max_attempts_for_stage("Tts", default), 100);
        assert_eq!(max_attempts_for_stage("AsrValidation", default), 100);
        assert_eq!(max_attempts_for_stage("RenderFinal", default), 100);
    }

    #[test]
    fn cpu_stages_use_default_retries() {
        let default = 50;
        assert_eq!(max_attempts_for_stage("Planning", default), 50);
        assert_eq!(max_attempts_for_stage("Research", default), 50);
        assert_eq!(max_attempts_for_stage("Script", default), 50);
        assert_eq!(max_attempts_for_stage("Captions", default), 50);
        assert_eq!(max_attempts_for_stage("QaFinal", default), 50);
    }
}
