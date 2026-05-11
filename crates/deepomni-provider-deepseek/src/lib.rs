//! # DeepOmni DeepSeek Provider
//!
//! DeepSeek-native provider via OpenAI-compatible Chat Completions API.
//! Handles streaming, reasoning/thinking blocks, tool calls, reasoning
//! replay for continuation, and token/cost metadata.
//!
//! PRD §7.6, §23. Based on DeepSeek-TUI client patterns.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::debug;

use deepomni_model_provider::{
    MessageRole, ModelDelta, ModelInfo, ModelProvider,
    ModelProviderError, ModelRequest, ReasoningReplay,
};

// ── DeepSeek defaults ──

pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";
pub const DEEPSEEK_CHAT_ENDPOINT: &str = "/chat/completions";
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-v4-pro";
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Known DeepSeek models.
pub fn deepseek_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "deepseek-v4-pro".into(),
            provider: "deepseek".into(),
            display_name: "DeepSeek V4 Pro".into(),
            context_window: 1_000_000,
            supports_tools: true,
            supports_reasoning: true,
            aliases: vec!["deepseek-chat".into()],
            max_output_tokens: Some(32000),
        },
        ModelInfo {
            id: "deepseek-v4-flash".into(),
            provider: "deepseek".into(),
            display_name: "DeepSeek V4 Flash".into(),
            context_window: 1_000_000,
            supports_tools: true,
            supports_reasoning: false,
            aliases: vec!["deepseek-reasoner".into()],
            max_output_tokens: Some(32000),
        },
    ]
}

/// Check if a model ID requires reasoning content.
fn requires_reasoning_content(model: &str) -> bool {
    let m = model.to_lowercase();
    // Flash models don't support reasoning, exclude them.
    if m.contains("flash") {
        return false;
    }
    m.contains("deepseek-v4")
        || m.contains("reasoner")
        || m.contains("-reasoning")
        || m.contains("-thinking")
}

/// Check if reasoning content should be replayed.
fn should_replay_reasoning(model: &str) -> bool {
    requires_reasoning_content(model)
}

// ── Wire-format types (OpenAI-compatible Chat Completions) ──

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ChatTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: ChatFunctionCall,
}

#[derive(Debug, Serialize)]
struct ChatFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct ChatTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ChatFunctionDef,
}

#[derive(Debug, Serialize)]
struct ChatFunctionDef {
    name: String,
    description: String,
    parameters: Value,
}

// ── SSE response parsing ──

#[derive(Debug, Deserialize)]
struct SseChunk {
    #[serde(default)]
    choices: Vec<SseChoice>,
    #[serde(default)]
    #[allow(dead_code)]
    usage: Option<SseUsage>,
}

#[derive(Debug, Deserialize)]
struct SseChoice {
    #[serde(default)]
    #[allow(dead_code)]
    index: u32,
    #[serde(default)]
    delta: SseDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SseDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<SseToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct SseToolCallDelta {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<SseFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct SseFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SseUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

// ── DeepSeek provider ──

pub struct DeepSeekProvider {
    http: HttpClient,
    api_key: String,
    base_url: String,
    default_model: String,
    last_usage: Mutex<Option<deepomni_protocol::Usage>>,
}

impl DeepSeekProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            http: HttpClient::builder()
                .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .build()
                .expect("http client"),
            api_key,
            base_url: DEFAULT_DEEPSEEK_BASE_URL.to_string(),
            default_model: DEFAULT_DEEPSEEK_MODEL.to_string(),
            last_usage: Mutex::new(None),
        }
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }

    pub fn with_default_model(mut self, model: String) -> Self {
        self.default_model = model;
        self
    }

    /// Get the last recorded usage (for cost tracking).
    pub async fn last_usage(&self) -> Option<deepomni_protocol::Usage> {
        self.last_usage.lock().await.clone()
    }

    /// Parse an SSE data line into zero or more deltas.
    fn parse_sse_line(data: &str, tool_call_states: &mut Vec<ToolCallState>) -> Vec<ModelDelta> {
        if data.is_empty() || data == "[DONE]" {
            return vec![];
        }

        let chunk: SseChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let mut deltas = Vec::new();

        for choice in &chunk.choices {
            let delta = &choice.delta;

            // Reasoning content.
            if let Some(ref reasoning) = delta.reasoning_content
                && !reasoning.is_empty() {
                    deltas.push(ModelDelta::Reasoning {
                        delta: reasoning.clone(),
                        replay_required: true,
                    });
                }

            // Text content.
            if let Some(ref content) = delta.content
                && !content.is_empty() {
                    deltas.push(ModelDelta::Text(content.clone()));
                }

            // Tool call deltas.
            if let Some(ref tool_calls) = delta.tool_calls {
                for tc in tool_calls {
                    let idx = tc.index as usize;
                    while tool_call_states.len() <= idx {
                        tool_call_states.push(ToolCallState::default());
                    }

                    let state = &mut tool_call_states[idx];

                    if let Some(ref id) = tc.id {
                        state.id = Some(id.clone());
                    }
                    if let Some(ref func) = tc.function {
                        if let Some(ref name) = func.name {
                            state.name = Some(name.clone());
                            // Emit tool call start.
                            deltas.push(ModelDelta::ToolCallStart {
                                call_id: state.id.clone().unwrap_or_default(),
                                tool_name: name.clone(),
                            });
                        }
                        if let Some(ref args) = func.arguments {
                            state.arguments.push_str(args);
                            deltas.push(ModelDelta::ToolCallArgumentsDelta {
                                call_id: state.id.clone().unwrap_or_default(),
                                delta: args.clone(),
                            });
                        }
                    }
                }
            }

            // Tool call completion on finish_reason.
            if let Some(ref finish) = choice.finish_reason {
                if finish == "tool_calls" || finish == "stop" {
                    for state in tool_call_states.iter() {
                        if state.name.is_some() && state.id.is_some() {
                            let args: Value = serde_json::from_str(&state.arguments)
                                .unwrap_or(Value::Null);
                            deltas.push(ModelDelta::ToolCallComplete {
                                call_id: state.id.clone().unwrap(),
                                tool_name: state.name.clone().unwrap(),
                                arguments: args,
                            });
                        }
                    }
                }
                if finish == "stop" {
                    deltas.push(ModelDelta::End);
                }
            }
        }

        deltas
    }

    /// Build the chat completion request body.
    fn build_request(&self, request: &ModelRequest) -> Result<ChatCompletionRequest, ModelProviderError> {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        let tools: Vec<ChatTool> = request
            .tools
            .iter()
            .map(|t| {
                let (name, desc, params) = match t {
                    deepomni_protocol::tool::ToolSpec::Function(f) => {
                        (&f.name, &f.description, &f.parameters)
                    }
                };
                ChatTool {
                    tool_type: "function".into(),
                    function: ChatFunctionDef {
                        name: name.clone(),
                        description: desc.clone(),
                        parameters: params.clone(),
                    },
                }
            })
            .collect();

        Ok(ChatCompletionRequest {
            model,
            messages: Self::build_messages(request)?,
            tools: if tools.is_empty() { None } else { Some(tools) },
            max_tokens: request.max_output_tokens,
            temperature: request.temperature,
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
        })
    }

    /// Convert DeepOmni model messages to Chat Completion wire format.
    /// Returns an error if required reasoning replay is missing for DeepSeek V4.
    fn build_messages(request: &ModelRequest) -> Result<Vec<ChatMessage>, ModelProviderError> {
        let mut messages = Vec::new();

        // System prompt.
        if let Some(ref sys) = request.system_prompt {
            messages.push(ChatMessage {
                role: "system".into(),
                content: Some(sys.clone()),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        }

        // Collect replay entries indexed by position.
        let replay_entries: Vec<&ReasoningReplay> = request
            .replay_reasoning
            .as_ref()
            .map(|rr| rr.iter().collect())
            .unwrap_or_default();

        // Track which replay entry to attach next.
        let mut replay_idx = 0usize;

        // Conversation messages.
        for msg in &request.messages {
            let mut chat_msg = ChatMessage {
                role: role_to_str(&msg.role),
                content: msg.content.clone(),
                tool_calls: None,
                tool_call_id: msg.tool_call_id.clone(),
                reasoning_content: msg.reasoning_content.clone(),
            };

            // Convert tool calls.
            if !msg.tool_calls.is_empty() {
                chat_msg.tool_calls = Some(
                    msg.tool_calls
                        .iter()
                        .map(|tc| ChatToolCall {
                            id: tc.call_id.clone(),
                            call_type: "function".into(),
                            function: ChatFunctionCall {
                                name: tc.tool_name.clone(),
                                arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                            },
                        })
                        .collect(),
                );
            }

            // Attach reasoning replay to assistant messages with tool calls.
            // Match by position: the Nth assistant message with tool_calls
            // gets the Nth ReasoningReplay entry.
            if msg.role == MessageRole::Assistant && !msg.tool_calls.is_empty()
                && replay_idx < replay_entries.len() && chat_msg.reasoning_content.is_none() {
                    chat_msg.reasoning_content = Some(
                        replay_entries[replay_idx].reasoning_content.clone()
                    );
                    replay_idx += 1;
                }

            messages.push(chat_msg);
        }

        // Validate: DeepSeek V4 requires reasoning_content on assistant messages
        // with tool_calls. Return a typed error before network dispatch so the
        // runtime can fail deterministically.
        let model = &request.model;
        if should_replay_reasoning(model) {
            for msg in &messages {
                if msg.role == "assistant"
                    && msg.tool_calls.is_some()
                    && msg.reasoning_content.is_none()
                {
                    return Err(ModelProviderError::InvalidResponse(
                        "reasoning replay required for assistant tool-call messages but missing".into(),
                    ));
                }
            }
        }

        Ok(messages)
    }
}

#[async_trait]
impl ModelProvider for DeepSeekProvider {
    fn provider_name(&self) -> &str {
        "deepseek"
    }

    fn known_models(&self) -> Vec<ModelInfo> {
        deepseek_models()
    }

    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<deepomni_model_provider::ModelStream, ModelProviderError> {
        let url = format!("{}{}", self.base_url, DEEPSEEK_CHAT_ENDPOINT);
        let req_body = self.build_request(&request)?;

        debug!(model = %req_body.model, "DeepSeek streaming request");

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&req_body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ModelProviderError::Timeout
                } else {
                    ModelProviderError::HttpError {
                        status: 0,
                        body: e.to_string(),
                    }
                }
            })?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ModelProviderError::HttpError { status, body });
        }

        let byte_stream = resp.bytes_stream();
        let tool_call_states = Arc::new(Mutex::new(Vec::new()));

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ModelDelta, ModelProviderError>>(256);

        let states = tool_call_states.clone();
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut byte_stream = byte_stream;
            let mut buf = String::new();

            loop {
                match byte_stream.next().await {
                    Some(Ok(chunk)) => {
                        let text = String::from_utf8_lossy(&chunk);
                        buf.push_str(&text);

                        while let Some(line_end) = buf.find('\n') {
                            let line = buf[..line_end].trim().to_string();
                            buf = buf[line_end + 1..].to_string();

                            let data = line.strip_prefix("data: ").unwrap_or("");
                            if data.is_empty() || data == "[DONE]" {
                                continue;
                            }

                            let mut states = states.lock().await;
                            let deltas = DeepSeekProvider::parse_sse_line(data, &mut states);
                            for delta in deltas {
                                let is_end = matches!(delta, ModelDelta::End);
                                if tx.send(Ok(delta)).await.is_err() {
                                    return;
                                }
                                if is_end {
                                    return;
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        let _ = tx.send(Err(ModelProviderError::StreamError(e.to_string()))).await;
                        return;
                    }
                    None => {
                        let _ = tx.send(Ok(ModelDelta::End)).await;
                        return;
                    }
                }
            }
        });

        Ok(Box::new(tokio_stream::wrappers::ReceiverStream::new(rx)) as deepomni_model_provider::ModelStream)
    }
}

// ── Helpers ──

#[derive(Debug, Clone, Default)]
struct ToolCallState {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn role_to_str(role: &MessageRole) -> String {
    match role {
        MessageRole::System => "system".into(),
        MessageRole::User => "user".into(),
        MessageRole::Assistant => "assistant".into(),
        MessageRole::Tool => "tool".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepomni_model_provider::ModelMessage;

    #[test]
    fn test_requires_reasoning() {
        assert!(requires_reasoning_content("deepseek-v4-pro"));
        assert!(requires_reasoning_content("deepseek-reasoner"));
        assert!(!requires_reasoning_content("deepseek-v4-flash"));
    }

    #[test]
    fn test_parse_sse_text_delta() {
        let data = r#"{"choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;
        let mut states = Vec::new();
        let deltas = DeepSeekProvider::parse_sse_line(data, &mut states);
        assert_eq!(deltas.len(), 1);
        match &deltas[0] {
            ModelDelta::Text(text) => assert_eq!(text, "hello"),
            _ => panic!("expected text delta"),
        }
    }

    #[test]
    fn test_parse_sse_reasoning_delta() {
        let data = r#"{"choices":[{"index":0,"delta":{"reasoning_content":"thinking..."},"finish_reason":null}]}"#;
        let mut states = Vec::new();
        let deltas = DeepSeekProvider::parse_sse_line(data, &mut states);
        match &deltas[0] {
            ModelDelta::Reasoning { delta, replay_required } => {
                assert_eq!(delta, "thinking...");
                assert!(*replay_required);
            }
            _ => panic!("expected reasoning delta"),
        }
    }

    #[test]
    fn test_parse_sse_tool_call_stop() {
        let data = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":\"/tmp/a\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        let mut states = vec![ToolCallState::default()];
        let deltas = DeepSeekProvider::parse_sse_line(data, &mut states);
        // Should have start, args delta, and complete.
        let has_complete = deltas.iter().any(|d| matches!(d, ModelDelta::ToolCallComplete { .. }));
        assert!(has_complete);
    }

    #[test]
    fn test_build_request_messages() {
        let provider = DeepSeekProvider::new("sk-test".into());
        let request = ModelRequest {
            model: "deepseek-v4-pro".into(),
            messages: vec![ModelMessage {
                role: MessageRole::User,
                content: Some("hello".into()),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            }],
            tools: vec![],
            max_output_tokens: None,
            temperature: None,
            system_prompt: Some("You are helpful.".into()),
            replay_reasoning: None,
        };

        let req = provider.build_request(&request).unwrap();
        assert_eq!(req.model, "deepseek-v4-pro");
        assert!(req.stream);
        assert_eq!(req.messages.len(), 2); // system + user
    }

    #[test]
    fn test_deepseek_models() {
        let models = deepseek_models();
        assert!(models.len() >= 2);
        let pro = models.iter().find(|m| m.id == "deepseek-v4-pro").unwrap();
        assert!(pro.supports_reasoning);
        assert!(pro.supports_tools);
    }
}
