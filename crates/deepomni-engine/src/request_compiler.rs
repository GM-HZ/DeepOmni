use deepomni_agent::{RequestCompiler, TurnConfig};
use deepomni_context::ContextFragment;
use deepomni_journal::TurnJournal;
use deepomni_model_provider::{MessageRole, ModelMessage};
use deepomni_protocol::id::{ThreadId, TurnId};

pub struct EngineRequestCompiler {
    inner: RequestCompiler,
}

impl EngineRequestCompiler {
    pub fn new() -> Self {
        Self {
            inner: RequestCompiler::new(),
        }
    }

    pub fn compile_new_turn(
        &self,
        config: &TurnConfig,
        fragments: &[ContextFragment],
        history: &[ModelMessage],
        user_input: &str,
    ) -> Vec<ModelMessage> {
        self.inner
            .compile_new_turn(config, fragments, history, user_input)
    }

    pub fn compile_tool_continuation(
        &self,
        config: &TurnConfig,
        fragments: &[ContextFragment],
        transcript: &[ModelMessage],
    ) -> Vec<ModelMessage> {
        self.inner
            .compile_tool_continuation(config, fragments, transcript)
    }

    pub fn compile_approval_resume(
        &self,
        config: &TurnConfig,
        fragments: &[ContextFragment],
        journal: &dyn TurnJournal,
        thread_id: &ThreadId,
        turn_id: &TurnId,
    ) -> Vec<ModelMessage> {
        let transcript = journal
            .replay(thread_id, 0)
            .unwrap_or_default()
            .into_iter()
            .filter(|record| record.turn_id == *turn_id)
            .filter_map(|record| match record.entry {
                deepomni_journal::JournalEntry::TurnStarted { user_input } => Some(ModelMessage {
                    role: MessageRole::User,
                    content: Some(user_input),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    reasoning_content: None,
                }),
                deepomni_journal::JournalEntry::AssistantDelta { delta, .. } => Some(ModelMessage {
                    role: MessageRole::Assistant,
                    content: Some(delta),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    reasoning_content: None,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        self.compile_tool_continuation(config, fragments, &transcript)
    }
}

impl Default for EngineRequestCompiler {
    fn default() -> Self {
        Self::new()
    }
}

