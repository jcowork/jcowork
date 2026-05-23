//! LLM Router - route to the correct provider based on model string.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::openai::{OpenAiConfig, OpenAiProvider};
use crate::provider::LlmProvider;

/// Model info for API responses and config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context_length: usize,
}

/// Provider config entry (loaded from providers.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    /// Env var name for API key (empty = no key needed, e.g. local/Ollama).
    pub env_key: String,
    pub base_url: String,
    pub default_model: String,
    pub context_length: usize,
    pub models: Vec<ModelInfo>,
}

/// Provider info for the /api/providers response.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub models: Vec<ModelInfo>,
}

/// Router that selects the correct LLM provider based on model string.
///
/// Model strings follow the format "provider:model", e.g.,
/// - `deepseek:deepseek-chat`
/// - `qwen:qwen-plus`
/// - `moonshot:kimi-k2.6`
pub struct LlmRouter {
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    /// Full provider configs (from providers.json).
    provider_configs: Vec<ProviderConfig>,
}

impl LlmRouter {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            provider_configs: Vec::new(),
        }
    }

    /// Register a provider with a name.
    pub fn register(&mut self, name: impl Into<String>, provider: Arc<dyn LlmProvider>) {
        self.providers.insert(name.into(), provider);
    }

    /// Register a single OpenAI-compatible provider with a custom name.
    pub fn register_openai_compatible(&mut self, name: &str, config: OpenAiConfig) {
        let provider = Arc::new(OpenAiProvider::new(config));
        self.providers.insert(name.to_string(), provider);
    }

    /// Load provider configs from a JSON file and register providers from env vars.
    ///
    /// Reads `providers.json` for provider definitions (base_url, models, etc.).
    /// For each provider with a non-empty `env_key`, checks the env var for an API key.
    /// If the key exists and is non-empty, registers the provider.
    /// Providers with empty `env_key` (like local/Ollama) are always registered.
    ///
    /// Also reads optional base URL overrides: `DEEPSEEK_BASE_URL`, `QWEN_BASE_URL`, etc.
    pub fn from_env() -> Result<Self> {
        let mut router = Self::new();

        // Load provider configs from JSON file
        let configs = Self::load_provider_configs()?;
        router.provider_configs = configs.clone();

        for config in &configs {
            let api_key = if config.env_key.is_empty() {
                // No env key = always register (e.g. local/Ollama)
                String::new()
            } else {
                match std::env::var(&config.env_key) {
                    Ok(key) if !key.is_empty() => key,
                    _ => continue, // Skip providers without a configured API key
                }
            };

            // Check for base URL override from env
            let base_url_env = format!("{}_BASE_URL", config.id.to_uppercase());
            let base_url = std::env::var(&base_url_env).unwrap_or_else(|_| config.base_url.clone());

            let openai_config = OpenAiConfig {
                provider_name: config.id.clone(),
                api_key: if api_key.is_empty() { "unused".to_string() } else { api_key },
                base_url,
                model: config.default_model.clone(),
                context_length: config.context_length,
            };

            router.register_openai_compatible(&config.id, openai_config);
        }

        Ok(router)
    }

    /// Load provider configs from providers.json.
    /// Returns an error if no providers.json file is found.
    fn load_provider_configs() -> Result<Vec<ProviderConfig>> {
        // Try multiple paths
        let paths = [
            "providers.json",
            "config/providers.json",
            "/etc/jcowork/providers.json",
        ];

        for path in &paths {
            if let Ok(content) = std::fs::read_to_string(path) {
                match serde_json::from_str(&content) {
                    Ok(configs) => {
                        tracing::info!(path = %path, "Loaded provider configs from file");
                        return Ok(configs);
                    }
                    Err(e) => {
                        return Err(anyhow::anyhow!("Failed to parse {}: {}", path, e));
                    }
                }
            }
        }

        Err(anyhow::anyhow!(
            "providers.json not found. Searched: {}",
            paths.join(", ")
        ))
    }

    /// Get a provider by model string (format: "provider:model" or just "model").
    pub fn get_provider(&self, model: &str) -> Result<Arc<dyn LlmProvider>> {
        let provider_name = if let Some(colon_pos) = model.find(':') {
            &model[..colon_pos]
        } else {
            "openai" // default provider
        };

        self.providers
            .get(provider_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!(
                "Unknown provider '{}'. Available: {}. Set the corresponding API key env var to enable it.",
                provider_name,
                self.available_providers().join(", ")
            ))
    }

    /// Extract just the model name from a "provider:model" string.
    pub fn extract_model_name(model: &str) -> &str {
        if let Some(colon_pos) = model.find(':') {
            &model[colon_pos + 1..]
        } else {
            model
        }
    }

    /// List all registered provider names.
    pub fn available_providers(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.providers.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// Get provider info with models for the API.
    /// Returns only registered (active) providers with their available models.
    pub fn providers_info(&self) -> Vec<ProviderInfo> {
        let mut result: Vec<ProviderInfo> = self.providers.keys().map(|name| {
            let config = self.provider_configs.iter().find(|c| c.id == *name);
            let (display_name, models) = match config {
                Some(c) => (c.name.clone(), c.models.clone()),
                None => (name.clone(), Vec::new()),
            };
            ProviderInfo {
                id: name.clone(),
                name: display_name,
                models,
            }
        }).collect();
        result.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }
}

impl Default for LlmRouter {
    fn default() -> Self {
        Self::new()
    }
}
