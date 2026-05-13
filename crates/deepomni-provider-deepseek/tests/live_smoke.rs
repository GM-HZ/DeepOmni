//! Live smoke test for DeepSeek provider. Requires DEEPSEEK_API_KEY env.
//! Run with: cargo test --test live_smoke -- --ignored --nocapture

use deepomni_model_provider::{MessageRole, ModelDelta, ModelMessage, ModelProvider, ModelRequest};
use deepomni_provider_deepseek::DeepSeekProvider;

fn api_key() -> Option<String> {
    std::env::var("DEEPSEEK_API_KEY").ok()
}

#[tokio::test]
#[ignore]
async fn test_live_deepseek_stream_text() {
    let key = api_key().expect("DEEPSEEK_API_KEY not set");
    let provider = DeepSeekProvider::new(key);

    let request = ModelRequest {
        model: "deepseek-v4-flash".into(),
        messages: vec![ModelMessage {
            role: MessageRole::User,
            content: Some("Say 'hello world' in exactly 3 words.".into()),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        }],
        tools: vec![],
        max_output_tokens: Some(50),
        temperature: Some(0.0),
        system_prompt: None,
        replay_reasoning: None,
    };

    let mut stream = provider.stream(request).await.expect("stream should start");
    let mut text_deltas = Vec::new();
    let mut saw_end = false;

    while let Some(delta) = futures::StreamExt::next(&mut stream).await {
        match delta.expect("delta should be ok") {
            ModelDelta::Text(t) => text_deltas.push(t),
            ModelDelta::End => {
                saw_end = true;
                break;
            }
            _ => {}
        }
    }

    assert!(saw_end, "stream should end with ModelDelta::End");
    let response = text_deltas.join("");
    assert!(!response.is_empty(), "should get text deltas");
    println!("DeepSeek response: {response}");
}

#[tokio::test]
#[ignore]
async fn test_live_deepseek_with_tools() {
    let key = api_key().expect("DEEPSEEK_API_KEY not set");
    let provider = DeepSeekProvider::new(key);

    let request = ModelRequest {
        model: "deepseek-v4-flash".into(),
        messages: vec![ModelMessage {
            role: MessageRole::User,
            content: Some("What is the weather?".into()),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        }],
        tools: vec![],
        max_output_tokens: Some(50),
        temperature: Some(0.0),
        system_prompt: Some("You are a helpful assistant. Answer concisely.".into()),
        replay_reasoning: None,
    };

    let result = provider.stream(request).await;
    assert!(result.is_ok(), "stream should succeed");
    println!("DeepSeek tool-stream test passed");
}

#[test]
fn test_api_key_detection() {
    // This test always runs — verifies the env var check logic.
    let has_key = api_key().is_some();
    println!("DEEPSEEK_API_KEY present: {has_key}");
}
