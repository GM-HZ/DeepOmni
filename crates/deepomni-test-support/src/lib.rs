//! # DeepOmni Test Support
//!
//! Shared test fixtures, fakes, and helpers for deterministic testing
//! across all DeepOmni crates. No live API dependency required.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::sync::Mutex;

use deepomni_protocol::{
    EventFrame, ThreadId, ToolOutput, TurnId,
};

use async_trait::async_trait;
use deepomni_model_provider::{
    ModelDelta, ModelInfo, ModelProvider, ModelProviderError,
    ModelRequest, ModelStream,
};

// ── Temp workspace ──

/// Create a temporary workspace directory with the given files.
///
/// Files are specified as `(relative_path, content)` pairs. The directory
/// is cleaned up when `TempWorkspace` is dropped.
pub struct TempWorkspace {
    pub root: PathBuf,
}

impl Default for TempWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

impl TempWorkspace {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!("deepomni-ws-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    /// Add a file to the workspace.
    pub fn add_file(&self, relative_path: &str, content: &str) {
        let full_path = self.root.join(relative_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full_path, content).unwrap();
    }

    /// Add a directory.
    pub fn add_dir(&self, relative_path: &str) {
        fs::create_dir_all(self.root.join(relative_path)).unwrap();
    }

    /// Returns the canonical workspace root path.
    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// ── Mock model provider ──

/// Pre-programmed model response for deterministic agent-loop testing.
#[derive(Debug, Clone)]
pub struct MockModelResponse {
    /// Text deltas to stream as `AssistantMessageDelta` events.
    pub text_deltas: Vec<String>,
    /// Reasoning deltas (optional, for DeepSeek tests).
    pub reasoning_deltas: Vec<String>,
    /// Whether reasoning replay is required.
    pub reasoning_replay_required: bool,
    /// Tool calls to emit.
    pub tool_calls: Vec<MockToolCall>,
}

#[derive(Debug, Clone)]
pub struct MockToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Value,
}

/// A mock model provider that returns pre-programmed responses.
///
/// Users push `MockModelResponse` objects; each turn pops the next one.
pub struct MockModelProvider {
    responses: Mutex<Vec<MockModelResponse>>,
}

impl MockModelProvider {
    pub fn new(responses: Vec<MockModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }

    /// Pop the next pre-programmed response.
    pub async fn next_response(&self) -> Option<MockModelResponse> {
        self.responses.lock().await.pop()
    }

    /// Create a stream from the pre-programmed response.
    async fn to_stream(resp: MockModelResponse) -> ModelStream {
        use futures::stream;
        let mut deltas: Vec<Result<ModelDelta, ModelProviderError>> = Vec::new();

        // Reasoning deltas first.
        for rd in &resp.reasoning_deltas {
            deltas.push(Ok(ModelDelta::Reasoning {
                delta: rd.clone(),
                replay_required: resp.reasoning_replay_required,
            }));
        }

        // Tool calls.
        for tc in &resp.tool_calls {
            deltas.push(Ok(ModelDelta::ToolCallStart {
                call_id: tc.call_id.clone(),
                tool_name: tc.tool_name.clone(),
            }));
            let args_str = serde_json::to_string(&tc.arguments).unwrap_or_default();
            deltas.push(Ok(ModelDelta::ToolCallArgumentsDelta {
                call_id: tc.call_id.clone(),
                delta: args_str,
            }));
            deltas.push(Ok(ModelDelta::ToolCallComplete {
                call_id: tc.call_id.clone(),
                tool_name: tc.tool_name.clone(),
                arguments: tc.arguments.clone(),
            }));
        }

        // Text deltas.
        for td in &resp.text_deltas {
            deltas.push(Ok(ModelDelta::Text(td.clone())));
        }

        // Always end the stream.
        deltas.push(Ok(ModelDelta::End));

        Box::new(stream::iter(deltas))
    }
}

#[async_trait]
impl ModelProvider for MockModelProvider {
    fn provider_name(&self) -> &str {
        "mock"
    }

    fn known_models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: "mock-model".into(),
            provider: "mock".into(),
            display_name: "Mock Model".into(),
            context_window: 128_000,
            supports_tools: true,
            supports_reasoning: false,
            aliases: vec![],
            max_output_tokens: Some(4096),
        }]
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<ModelStream, ModelProviderError> {
        let resp = self.next_response().await.unwrap_or_else(|| MockModelResponse {
            text_deltas: vec!["mock fallback".into()],
            reasoning_deltas: vec![],
            reasoning_replay_required: false,
            tool_calls: vec![],
        });
        Ok(Self::to_stream(resp).await)
    }
}

/// Create a mock response with a simple text answer.
pub fn mock_text_response(text: &str) -> MockModelResponse {
    MockModelResponse {
        text_deltas: vec![text.to_string()],
        reasoning_deltas: vec![],
        reasoning_replay_required: false,
        tool_calls: vec![],
    }
}

/// Create a mock response with a single tool call.
pub fn mock_tool_call_response(tool_name: &str, arguments: Value) -> MockModelResponse {
    let call_id = format!("call-{}", uuid::Uuid::new_v4().simple().to_string().chars().take(8).collect::<String>());
    MockModelResponse {
        text_deltas: vec![],
        reasoning_deltas: vec![],
        reasoning_replay_required: false,
        tool_calls: vec![MockToolCall {
            call_id,
            tool_name: tool_name.to_string(),
            arguments,
        }],
    }
}

/// Create a mock response with reasoning deltas (DeepSeek thinking mode).
pub fn mock_reasoning_response(reasoning: &str, text: &str, replay_required: bool) -> MockModelResponse {
    MockModelResponse {
        text_deltas: vec![text.to_string()],
        reasoning_deltas: vec![reasoning.to_string()],
        reasoning_replay_required: replay_required,
        tool_calls: vec![],
    }
}

// ── Fake tool handler ──

/// Simple fake tool handler that returns pre-programmed outputs.
pub struct FakeToolHandler {
    pub name: String,
    pub is_mutating: bool,
    pub outputs: Mutex<Vec<Result<ToolOutput, String>>>,
}

impl FakeToolHandler {
    pub fn new(name: &str, outputs: Vec<Result<ToolOutput, String>>) -> Self {
        Self {
            name: name.to_string(),
            is_mutating: false,
            outputs: Mutex::new(outputs),
        }
    }

    pub fn new_mutating(name: &str, outputs: Vec<Result<ToolOutput, String>>) -> Self {
        Self {
            name: name.to_string(),
            is_mutating: true,
            outputs: Mutex::new(outputs),
        }
    }

    pub fn empty(name: &str) -> Self {
        Self {
            name: name.to_string(),
            is_mutating: false,
            outputs: Mutex::new(vec![Ok(ToolOutput::Function {
                body: Some(serde_json::json!({"result": "ok"})),
                success: true,
            })]),
        }
    }
}

// ── Event assertions ──

/// Collect events and provide assertion helpers.
pub struct EventCollector {
    pub events: Vec<EventFrame>,
}

impl EventCollector {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    pub fn push(&mut self, event: EventFrame) {
        self.events.push(event);
    }

    /// Assert that the event stream contains at least one event of the given variant.
    pub fn assert_contains_event<F>(&self, predicate: F)
    where
        F: Fn(&EventFrame) -> bool,
    {
        let found = self.events.iter().any(predicate);
        assert!(
            found,
            "expected event not found in {} events",
            self.events.len()
        );
    }

    /// Count events matching the predicate.
    pub fn count<F>(&self, predicate: F) -> usize
    where
        F: Fn(&EventFrame) -> bool,
    {
        self.events.iter().filter(|e| predicate(e)).count()
    }

    /// Assert the last event matches.
    pub fn assert_last<F>(&self, predicate: F)
    where
        F: Fn(&EventFrame) -> bool,
    {
        let last = self
            .events
            .last()
            .expect("no events collected");
        assert!(
            predicate(last),
            "last event did not match expected variant"
        );
    }
}

impl Default for EventCollector {
    fn default() -> Self {
        Self::new()
    }
}

// ── Snapshot helpers ──

/// Serialize a value to JSON and compare with a golden file.
/// If the golden file doesn't exist, create it.
pub fn assert_json_snapshot(name: &str, value: &impl serde::Serialize) {
    let actual = serde_json::to_string_pretty(value).unwrap();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("{name}.json"));
    if path.exists() {
        let expected = fs::read_to_string(&path).unwrap();
        assert_eq!(actual, expected, "snapshot mismatch for {name}");
    } else {
        fs::write(&path, &actual).unwrap();
        panic!("snapshot {name} created — re-run test");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temp_workspace_create_and_cleanup() {
        let ws = TempWorkspace::new();
        assert!(ws.root.exists());
        ws.add_file("src/main.rs", "fn main() {}");
        assert!(ws.root.join("src/main.rs").exists());

        let root_clone = ws.root.clone();
        drop(ws);
        assert!(!root_clone.exists());
    }

    #[test]
    fn test_mock_model_provider() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let provider = MockModelProvider::new(vec![mock_text_response("hello world")]);
            let resp = provider.next_response().await.unwrap();
            assert_eq!(resp.text_deltas[0], "hello world");
            assert!(provider.next_response().await.is_none());
        });
    }

    #[test]
    fn test_event_collector() {
        let mut collector = EventCollector::new();
        collector.push(EventFrame::TurnStarted {
            thread_id: ThreadId::from_string("t1"),
            turn_id: TurnId::from_string("turn1"),
            user_input: "test".into(),
        });
        collector.assert_contains_event(|e| matches!(e, EventFrame::TurnStarted { .. }));
        assert_eq!(collector.count(|e| matches!(e, EventFrame::TurnStarted { .. })), 1);
    }
}
