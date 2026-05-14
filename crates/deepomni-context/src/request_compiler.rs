//! Provider-aware request compiler with three explicit paths.

use crate::ContextFragment;
#[cfg(test)]
use deepomni_model_provider::ModelToolCall;
use deepomni_model_provider::{MessageRole, ModelMessage, ModelRequest, ReasoningReplay};
use deepomni_protocol::tool::ToolSpec;

pub struct CompiledRequest {
    pub request: ModelRequest,
    pub has_tool_continuation: bool,
}

pub struct CompileConfig<'a> {
    pub system_prompt: Option<&'a str>,
    pub fragments: &'a [ContextFragment],
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: Option<u64>,
    pub model: &'a str,
    pub reasoning_to_replay: Option<Vec<ReasoningReplay>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRequestKind {
    NewTurn,
    ToolContinuation,
    ApprovalResume,
}

pub struct RequestCompiler;

impl RequestCompiler {
    pub fn compile(
        kind: TurnRequestKind,
        config: &CompileConfig<'_>,
        history_or_transcript: &[ModelMessage],
        user_input: &str,
    ) -> CompiledRequest {
        match kind {
            TurnRequestKind::NewTurn => {
                Self::compile_new_turn(config, history_or_transcript, user_input)
            }
            TurnRequestKind::ToolContinuation => {
                Self::compile_tool_continuation(config, history_or_transcript)
            }
            TurnRequestKind::ApprovalResume => {
                Self::compile_tool_continuation(config, history_or_transcript)
            }
        }
    }

    pub fn compile_new_turn(
        config: &CompileConfig<'_>,
        history: &[ModelMessage],
        user_input: &str,
    ) -> CompiledRequest {
        let mut messages = Vec::new();
        if let Some(sys) = config.system_prompt {
            messages.push(ModelMessage {
                role: MessageRole::System,
                content: Some(sys.to_string()),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            });
        }
        for f in config.fragments {
            if let ContextFragment::ConversationMessage { role, content, .. } = f {
                messages.push(ModelMessage {
                    role: match role.as_str() {
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        _ => MessageRole::User,
                    },
                    content: Some(content.clone()),
                    tool_calls: vec![],
                    tool_call_id: None,
                    reasoning_content: None,
                });
                continue;
            }
            let content = match f {
                ContextFragment::ToolSchemas { content }
                | ContextFragment::SkillInstruction { content, .. }
                | ContextFragment::PluginCapability { content, .. }
                | ContextFragment::Environment { content } => content.clone(),
                _ => continue,
            };
            let tag = f.marker_tag().unwrap_or("context");
            messages.push(ModelMessage {
                role: MessageRole::User,
                content: Some(format!("<{tag}>\n{content}\n</{tag}>")),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            });
        }
        messages.extend_from_slice(history);
        messages.push(ModelMessage {
            role: MessageRole::User,
            content: Some(user_input.to_string()),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        });
        CompiledRequest {
            request: ModelRequest {
                model: config.model.to_string(),
                messages,
                tools: config.tools.clone(),
                max_output_tokens: config.max_output_tokens,
                temperature: None,
                system_prompt: config.system_prompt.map(|s| s.to_string()),
                replay_reasoning: config.reasoning_to_replay.clone(),
            },
            has_tool_continuation: false,
        }
    }

    pub fn compile_tool_continuation(
        config: &CompileConfig<'_>,
        transcript: &[ModelMessage],
    ) -> CompiledRequest {
        let mut messages = Vec::new();
        if let Some(sys) = config.system_prompt {
            messages.push(ModelMessage {
                role: MessageRole::System,
                content: Some(sys.to_string()),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            });
        }
        for f in config.fragments {
            if let ContextFragment::ConversationMessage { role, content, .. } = f {
                messages.push(ModelMessage {
                    role: match role.as_str() {
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        _ => MessageRole::User,
                    },
                    content: Some(content.clone()),
                    tool_calls: vec![],
                    tool_call_id: None,
                    reasoning_content: None,
                });
                continue;
            }
            let content = match f {
                ContextFragment::ToolSchemas { content }
                | ContextFragment::SkillInstruction { content, .. }
                | ContextFragment::PluginCapability { content, .. }
                | ContextFragment::Environment { content } => content.clone(),
                _ => continue,
            };
            let tag = f.marker_tag().unwrap_or("context");
            messages.push(ModelMessage {
                role: MessageRole::User,
                content: Some(format!("<{tag}>\n{content}\n</{tag}>")),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            });
        }
        messages.extend_from_slice(transcript);
        CompiledRequest {
            request: ModelRequest {
                model: config.model.to_string(),
                messages,
                tools: config.tools.clone(),
                max_output_tokens: config.max_output_tokens,
                temperature: None,
                system_prompt: None,
                replay_reasoning: config.reasoning_to_replay.clone(),
            },
            has_tool_continuation: true,
        }
    }

    pub fn compile_approval_resume(
        config: &CompileConfig<'_>,
        journal_transcript: &[ModelMessage],
        tool_result_content: &str,
        call_id: &str,
    ) -> CompiledRequest {
        let mut messages = Vec::new();
        if let Some(sys) = config.system_prompt {
            messages.push(ModelMessage {
                role: MessageRole::System,
                content: Some(sys.to_string()),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            });
        }
        for f in config.fragments {
            if let ContextFragment::ConversationMessage { role, content, .. } = f {
                messages.push(ModelMessage {
                    role: match role.as_str() {
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        _ => MessageRole::User,
                    },
                    content: Some(content.clone()),
                    tool_calls: vec![],
                    tool_call_id: None,
                    reasoning_content: None,
                });
                continue;
            }
            let content = match f {
                ContextFragment::ToolSchemas { content }
                | ContextFragment::SkillInstruction { content, .. }
                | ContextFragment::PluginCapability { content, .. }
                | ContextFragment::Environment { content } => content.clone(),
                _ => continue,
            };
            let tag = f.marker_tag().unwrap_or("context");
            messages.push(ModelMessage {
                role: MessageRole::User,
                content: Some(format!("<{tag}>\n{content}\n</{tag}>")),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            });
        }
        messages.extend_from_slice(journal_transcript);
        messages.push(ModelMessage {
            role: MessageRole::Tool,
            content: Some(tool_result_content.to_string()),
            tool_calls: vec![],
            tool_call_id: Some(call_id.to_string()),
            reasoning_content: None,
        });
        CompiledRequest {
            request: ModelRequest {
                model: config.model.to_string(),
                messages,
                tools: config.tools.clone(),
                max_output_tokens: None,
                temperature: None,
                system_prompt: None,
                replay_reasoning: config.reasoning_to_replay.clone(),
            },
            has_tool_continuation: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextFragment;

    fn test_config() -> CompileConfig<'static> {
        CompileConfig {
            system_prompt: Some("System prompt"),
            fragments: &[],
            tools: vec![],
            max_output_tokens: None,
            model: "test",
            reasoning_to_replay: None,
        }
    }

    #[test]
    fn test_new_turn_message_order() {
        let fragments = vec![ContextFragment::ToolSchemas {
            content: "tool schemas".into(),
        }];
        let cfg = CompileConfig {
            fragments: &fragments,
            ..test_config()
        };
        let compiled = RequestCompiler::compile_new_turn(&cfg, &[], "hello");
        let msgs = compiled.request.messages;
        assert_eq!(msgs[0].role, MessageRole::System);
        assert!(
            msgs.last()
                .unwrap()
                .content
                .as_ref()
                .unwrap()
                .contains("hello")
        );
        assert!(!compiled.has_tool_continuation);
    }

    #[test]
    fn test_continuation_no_fake_user_message() {
        let transcript = vec![
            ModelMessage {
                role: MessageRole::Assistant,
                content: None,
                tool_calls: vec![ModelToolCall {
                    call_id: "call-1".into(),
                    tool_name: "read_file".into(),
                    arguments: serde_json::json!({}),
                }],
                tool_call_id: None,
                reasoning_content: None,
            },
            ModelMessage {
                role: MessageRole::Tool,
                content: Some("file content".into()),
                tool_calls: vec![],
                tool_call_id: Some("call-1".into()),
                reasoning_content: None,
            },
        ];
        let compiled = RequestCompiler::compile_tool_continuation(&test_config(), &transcript);
        let last = compiled.request.messages.last().unwrap();
        assert_eq!(last.role, MessageRole::Tool);
        assert!(compiled.has_tool_continuation);
    }

    #[test]
    fn test_approval_resume_appends_tool_result() {
        let transcript = vec![ModelMessage {
            role: MessageRole::User,
            content: Some("write file".into()),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        }];
        let compiled = RequestCompiler::compile_approval_resume(
            &test_config(),
            &transcript,
            "written 100 bytes",
            "call-1",
        );
        let last = compiled.request.messages.last().unwrap();
        assert_eq!(last.role, MessageRole::Tool);
        assert_eq!(last.tool_call_id.as_deref(), Some("call-1"));
        assert!(compiled.has_tool_continuation);
    }
}
