//! Compaction helpers: build model-driven compaction requests and
//! construct compacted history from summaries. Plan 3 Task 5.

use deepomni_model_provider::{MessageRole, ModelMessage};

/// Build a compaction request prompt from conversation history.
/// Appends a summarization instruction to the existing messages.
pub fn build_compaction_request(history: &[ModelMessage]) -> Vec<ModelMessage> {
    let summary_prompt = "Please summarize the conversation above, including key decisions, \
        file changes made, and any unresolved issues. Keep the summary concise.";
    let mut messages: Vec<ModelMessage> = history.to_vec();
    messages.push(ModelMessage {
        role: MessageRole::User,
        content: Some(summary_prompt.to_string()),
        tool_calls: vec![],
        tool_call_id: None,
        reasoning_content: None,
    });
    messages
}

/// Build compacted history from a summary and recent user messages.
/// The first message is a marker with the summary; recent messages follow.
pub fn build_compacted_history(
    summary: &str,
    recent_user_messages: &[ModelMessage],
) -> Vec<ModelMessage> {
    let prefix = "This conversation was compacted. Summary of previous context:";
    let mut messages = vec![ModelMessage {
        role: MessageRole::User,
        content: Some(format!("{prefix}\n{summary}")),
        tool_calls: vec![],
        tool_call_id: None,
        reasoning_content: None,
    }];
    messages.extend_from_slice(recent_user_messages);
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(content: &str) -> ModelMessage {
        ModelMessage {
            role: MessageRole::User,
            content: Some(content.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn compaction_request_includes_summary_prompt() {
        let history = vec![
            user_msg("fix the bug"),
            ModelMessage {
                role: MessageRole::Assistant,
                content: Some("done".into()),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            },
        ];
        let request = build_compaction_request(&history);
        assert_eq!(request.len(), 3);
        assert!(
            request
                .last()
                .unwrap()
                .content
                .as_deref()
                .unwrap()
                .contains("summarize")
        );
    }

    #[test]
    fn compacted_history_starts_with_summary_prefix() {
        let recent = vec![user_msg("latest question")];
        let compacted = build_compacted_history("fixed the login bug", &recent);
        assert_eq!(compacted.len(), 2);
        assert!(
            compacted[0]
                .content
                .as_deref()
                .unwrap()
                .contains("compacted")
        );
        assert!(
            compacted[0]
                .content
                .as_deref()
                .unwrap()
                .contains("fixed the login bug")
        );
    }
}
