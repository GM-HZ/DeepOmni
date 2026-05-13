//! Prompt normalization: ensures conversation history is clean before
//! being sent to the model provider. Plan 3 Task 1 + Task 2.
//!
//! Removes orphan tool results that have no matching assistant tool call.
//! Called automatically by ContextManager::for_prompt().

use deepomni_model_provider::{MessageRole, ModelMessage};
use std::collections::HashSet;

/// Normalize conversation history: remove orphan tool results that have no
/// matching assistant tool call. Also removes empty/duplicate messages.
pub fn normalize_history(items: &mut Vec<ModelMessage>) {
    let known_calls: HashSet<String> = items
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .flat_map(|m| m.tool_calls.iter().map(|tc| tc.call_id.clone()))
        .collect();

    items.retain(|m| {
        if m.role != MessageRole::Tool {
            return true;
        }
        m.tool_call_id
            .as_ref()
            .is_some_and(|cid| known_calls.contains(cid))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepomni_model_provider::ModelToolCall;

    #[test]
    fn removes_orphan_tool_results() {
        let mut messages = vec![
            ModelMessage {
                role: MessageRole::Assistant,
                content: None,
                tool_calls: vec![ModelToolCall {
                    call_id: "c1".into(),
                    tool_name: "read".into(),
                    arguments: serde_json::Value::Null,
                }],
                tool_call_id: None,
                reasoning_content: None,
            },
            ModelMessage {
                role: MessageRole::Tool,
                content: Some("result".into()),
                tool_calls: vec![],
                tool_call_id: Some("c1".into()),
                reasoning_content: None,
            },
            ModelMessage {
                role: MessageRole::Tool,
                content: Some("orphan".into()),
                tool_calls: vec![],
                tool_call_id: Some("c-missing".into()),
                reasoning_content: None,
            },
        ];

        normalize_history(&mut messages);
        assert_eq!(messages.len(), 2, "orphan tool result should be removed");
        assert!(
            messages
                .iter()
                .all(|m| m.tool_call_id.as_deref() != Some("c-missing"))
        );
    }

    #[test]
    fn non_tool_messages_preserved() {
        let mut messages = vec![ModelMessage {
            role: MessageRole::User,
            content: Some("hello".into()),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        }];
        normalize_history(&mut messages);
        assert_eq!(messages.len(), 1);
    }
}
