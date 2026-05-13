//! # DeepOmni Model Provider
//!
//! Provider abstraction and registry. Defines the `ModelProvider` trait,
//! streaming types, and provider selection logic.
//!
//! Based on DeepSeek-TUI agent ModelRegistry and PRD §7.5.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use deepomni_protocol::id::MessageId;
use deepomni_protocol::tool::ToolSpec;

// ── Model info ──

/// Metadata about a known model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Canonical model identifier (e.g., "deepseek-v4-pro").
    pub id: String,
    /// Provider this model belongs to.
    pub provider: String,
    /// User-facing display name.
    pub display_name: String,
    /// Context window size in tokens.
    pub context_window: u64,
    /// Whether the model supports native tool calling.
    pub supports_tools: bool,
    /// Whether the model supports reasoning/thinking tokens.
    pub supports_reasoning: bool,
    /// Aliases that resolve to this model.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Max output tokens (if known).
    pub max_output_tokens: Option<u64>,
}

// ── Provider trait ──

/// Streaming delta from a model.
#[derive(Debug, Clone)]
pub enum ModelDelta {
    /// Content text delta.
    Text(String),
    /// Reasoning/thinking text delta.
    Reasoning {
        delta: String,
        /// Whether this reasoning must be replayed for later continuation.
        replay_required: bool,
    },
    /// A tool call is being formed.
    ToolCallStart { call_id: String, tool_name: String },
    /// Tool call argument delta (streaming).
    ToolCallArgumentsDelta { call_id: String, delta: String },
    /// Tool call is complete and ready to execute.
    ToolCallComplete {
        call_id: String,
        tool_name: String,
        arguments: Value,
    },
    /// End of streaming response.
    End,
}

/// A streaming response from a model provider.
pub type ModelStream =
    Box<dyn futures::Stream<Item = Result<ModelDelta, ModelProviderError>> + Send + Unpin>;

/// Request to a model provider.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: Option<u64>,
    pub temperature: Option<f32>,
    pub system_prompt: Option<String>,
    /// For DeepSeek: replay required reasoning content.
    pub replay_reasoning: Option<Vec<ReasoningReplay>>,
}

/// A message in a model request.
#[derive(Debug, Clone)]
pub struct ModelMessage {
    pub role: MessageRole,
    pub content: Option<String>,
    pub tool_calls: Vec<ModelToolCall>,
    pub tool_call_id: Option<String>,
    /// DeepSeek reasoning content attached to an assistant message.
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool call from the assistant included in the conversation history.
#[derive(Debug, Clone)]
pub struct ModelToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Value,
}

/// Reasoning content to replay for DeepSeek continuation.
#[derive(Debug, Clone)]
pub struct ReasoningReplay {
    pub message_id: MessageId,
    pub reasoning_content: String,
}

// ── Provider trait ──

/// Trait for model providers (DeepSeek, OpenAI, Ollama, etc.).
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Return the provider name (e.g., "deepseek").
    fn provider_name(&self) -> &str;

    /// List known models from this provider.
    fn known_models(&self) -> Vec<ModelInfo>;

    /// Stream a completion response.
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelProviderError>;
}

// ── Provider registry ──

/// Registry of all available model providers.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn ModelProvider>>,
    models: Vec<ModelInfo>,
    /// Alias → canonical model ID mapping.
    alias_map: HashMap<String, String>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            models: Vec::new(),
            alias_map: HashMap::new(),
        }
    }

    /// Register a provider.
    pub fn register(&mut self, provider: Arc<dyn ModelProvider>) {
        for model in provider.known_models() {
            for alias in &model.aliases {
                self.alias_map.insert(alias.clone(), model.id.clone());
            }
            self.models.push(model);
        }
        self.providers
            .insert(provider.provider_name().to_string(), provider);
    }

    /// Resolve a model name to a canonical ID and provider.
    pub fn resolve(&self, requested: &str) -> Option<(String, &Arc<dyn ModelProvider>)> {
        // 1. Try alias resolution.
        let canonical = self
            .alias_map
            .get(requested)
            .cloned()
            .unwrap_or_else(|| requested.to_string());

        // 2. Find the canonical model.
        let model = self.models.iter().find(|m| m.id == canonical)?;

        // 3. Look up the provider.
        let provider = self.providers.get(&model.provider)?;
        Some((canonical, provider))
    }

    /// List all known models.
    pub fn list_models(&self) -> &[ModelInfo] {
        &self.models
    }

    /// Get a provider by name.
    pub fn get_provider(&self, name: &str) -> Option<&Arc<dyn ModelProvider>> {
        self.providers.get(name)
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Error ──

#[derive(Debug)]
pub enum ModelProviderError {
    HttpError { status: u16, body: String },
    StreamError(String),
    RateLimited { retry_after_secs: Option<u64> },
    Timeout,
    InvalidResponse(String),
    AuthError(String),
    UnknownModel(String),
}

impl std::fmt::Display for ModelProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelProviderError::HttpError { status, body } => {
                write!(f, "HTTP {status}: {body}")
            }
            ModelProviderError::StreamError(msg) => write!(f, "stream error: {msg}"),
            ModelProviderError::RateLimited { retry_after_secs } => {
                if let Some(secs) = retry_after_secs {
                    write!(f, "rate limited, retry after {secs}s")
                } else {
                    write!(f, "rate limited")
                }
            }
            ModelProviderError::Timeout => write!(f, "request timed out"),
            ModelProviderError::InvalidResponse(msg) => write!(f, "invalid response: {msg}"),
            ModelProviderError::AuthError(msg) => write!(f, "auth error: {msg}"),
            ModelProviderError::UnknownModel(model) => write!(f, "unknown model: {model}"),
        }
    }
}

impl std::error::Error for ModelProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock provider for testing.
    struct MockProvider {
        name: String,
        models: Vec<ModelInfo>,
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        fn provider_name(&self) -> &str {
            &self.name
        }

        fn known_models(&self) -> Vec<ModelInfo> {
            self.models.clone()
        }

        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelProviderError> {
            // Return a stream that emits a single text delta then ends.
            let deltas: Vec<Result<ModelDelta, ModelProviderError>> =
                vec![Ok(ModelDelta::Text("hello".into())), Ok(ModelDelta::End)];
            Ok(Box::new(futures::stream::iter(deltas)))
        }
    }

    #[test]
    fn test_provider_registry_resolve() {
        let mut registry = ProviderRegistry::new();
        let provider = Arc::new(MockProvider {
            name: "deepseek".into(),
            models: vec![ModelInfo {
                id: "deepseek-v4-pro".into(),
                provider: "deepseek".into(),
                display_name: "DeepSeek V4 Pro".into(),
                context_window: 1_000_000,
                supports_tools: true,
                supports_reasoning: true,
                aliases: vec!["deepseek-chat".into()],
                max_output_tokens: Some(32000),
            }],
        });
        registry.register(provider);

        // Resolve by alias.
        let (id, p) = registry.resolve("deepseek-chat").unwrap();
        assert_eq!(id, "deepseek-v4-pro");
        assert_eq!(p.provider_name(), "deepseek");

        // Resolve by canonical ID.
        let (id, _) = registry.resolve("deepseek-v4-pro").unwrap();
        assert_eq!(id, "deepseek-v4-pro");

        // Unknown model.
        assert!(registry.resolve("unknown-model").is_none());
    }

    #[test]
    fn test_model_info_defaults() {
        let model = ModelInfo {
            id: "test".into(),
            provider: "test".into(),
            display_name: "Test".into(),
            context_window: 128_000,
            supports_tools: false,
            supports_reasoning: false,
            aliases: vec![],
            max_output_tokens: None,
        };
        assert_eq!(model.context_window, 128_000);
        assert!(!model.supports_tools);
        assert!(!model.supports_reasoning);
    }
}
