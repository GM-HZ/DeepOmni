//! # DeepOmni Context
//!
//! Context assembly, token budgeting, and compaction.
//! Implements the deterministic overflow strategy from PRD §24.
//!
//! Based on Codex context fragment patterns.

use std::collections::VecDeque;

use sha2::{Digest, Sha256};

use deepomni_protocol::id::{MessageId, TurnId};

/// Token budget allocation for a turn.
///
/// PRD §24.2: default V1 allocation divides total input budget
/// into system, schemas, skills, history, and margin buckets.
#[derive(Debug, Clone)]
pub struct TokenBudget {
    /// Total input token limit.
    pub total: u64,
    /// Tokens allocated for system and safety instructions.
    pub system_tokens: u64,
    /// Tokens for tool schemas and capability summaries.
    pub schema_tokens: u64,
    /// Tokens for active skills and plugin instructions.
    pub skill_tokens: u64,
    /// Tokens for conversation history and tool results.
    pub history_tokens: u64,
    /// Safety margin.
    pub margin_tokens: u64,
    /// Tokens consumed so far.
    pub consumed: u64,
}

impl TokenBudget {
    /// Create a new budget with V1 default allocation percentages.
    pub fn new(total: u64) -> Self {
        Self {
            total,
            system_tokens: total * 10 / 100,   // 10%
            schema_tokens: total * 10 / 100,   // 10%
            skill_tokens: total * 10 / 100,    // 10%
            history_tokens: total * 65 / 100,  // 65%
            margin_tokens: total * 5 / 100,    // 5%
            consumed: 0,
        }
    }

    /// Remaining budget (excluding margin).
    pub fn remaining(&self) -> u64 {
        let cap = self.total.saturating_sub(self.margin_tokens);
        cap.saturating_sub(self.consumed)
    }

    /// Whether the budget is exhausted.
    pub fn is_exhausted(&self) -> bool {
        self.remaining() == 0
    }

    /// Record token consumption.
    pub fn consume(&mut self, tokens: u64) {
        self.consumed = self.consumed.saturating_add(tokens);
    }

    /// Budget metadata for debug output — PRD §24.2 requires this.
    pub fn debug_metadata(&self) -> TokenBudgetDebug {
        TokenBudgetDebug {
            total: self.total,
            consumed: self.consumed,
            remaining: self.remaining(),
            allocation_percent: TokenBudgetAllocation {
                system: 10,
                schema: 10,
                skills: 10,
                history: 65,
                margin: 5,
            },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenBudgetDebug {
    pub total: u64,
    pub consumed: u64,
    pub remaining: u64,
    pub allocation_percent: TokenBudgetAllocation,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenBudgetAllocation {
    pub system: u8,
    pub schema: u8,
    pub skills: u8,
    pub history: u8,
    pub margin: u8,
}

// ── Context fragment ──

/// Role of a context fragment in the conversation. Maps to Codex's
/// `ROLE` field and Claude Code's developer-message injection pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentRole {
    /// System-level instruction (highest priority, never dropped).
    System,
    /// Developer-level context injected as a marked history message.
    /// These are interleaved into the conversation with XML-like tags,
    /// enabling later identification and filtering during compaction.
    Developer,
    /// User-visible content (conversation messages).
    User,
}

/// A fragment of context assembled for each turn.
/// Each fragment carries a role and optional marker tag for identification
/// during compaction. Pattern from Codex `ContextualUserFragment` trait.
#[derive(Debug, Clone)]
pub enum ContextFragment {
    /// System prompt (always included, never dropped).
    System { content: String },
    /// Tool schema descriptions.
    ToolSchemas { content: String },
    /// Active skill instructions.
    SkillInstruction { skill_name: String, content: String },
    /// Plugin capability summary.
    PluginCapability { plugin_id: String, content: String },
    /// Conversation message.
    ConversationMessage { role: String, content: String, message_id: Option<MessageId> },
    /// Tool call result (pending continuation material — never dropped).
    ToolResult { call_id: String, content: String },
    /// Reasoning content required for replay (never dropped).
    ReasoningReplay { message_id: MessageId, content: String },
    /// Environment info (workspace, git, etc.).
    Environment { content: String },
}

impl ContextFragment {
    /// Approximate token count (characters ÷ 4 as naive estimate).
    pub fn estimated_tokens(&self) -> u64 {
        let text = match self {
            ContextFragment::System { content } => content,
            ContextFragment::ToolSchemas { content } => content,
            ContextFragment::SkillInstruction { content, .. } => content,
            ContextFragment::PluginCapability { content, .. } => content,
            ContextFragment::ConversationMessage { content, .. } => content,
            ContextFragment::ToolResult { content, .. } => content,
            ContextFragment::ReasoningReplay { content, .. } => content,
            ContextFragment::Environment { content } => content,
        };
        (text.len() as u64).div_ceil(4)
    }

    /// The role of this fragment in the conversation.
    pub fn role(&self) -> FragmentRole {
        match self {
            ContextFragment::System { .. } => FragmentRole::System,
            ContextFragment::ToolResult { .. }
            | ContextFragment::ReasoningReplay { .. } => FragmentRole::User,
            _ => FragmentRole::Developer,
        }
    }

    /// Marker tag for identifying this fragment during compaction. Tags follow
    /// the Codex pattern: developer fragments interleaved into history with
    /// XML-like markers that can be recognized and filtered later.
    pub fn marker_tag(&self) -> Option<&'static str> {
        match self {
            ContextFragment::System { .. } => None,
            ContextFragment::ToolSchemas { .. } => Some("tool-schemas"),
            ContextFragment::SkillInstruction { .. } => Some("skill-instruction"),
            ContextFragment::PluginCapability { .. } => Some("plugin-capability"),
            ContextFragment::Environment { .. } => Some("environment"),
            ContextFragment::ConversationMessage { .. } => None,
            ContextFragment::ToolResult { .. } => None,
            ContextFragment::ReasoningReplay { .. } => None,
        }
    }

    /// Whether this fragment is in the "never drop" protection list.
    pub fn is_protected(&self) -> bool {
        matches!(
            self,
            ContextFragment::System { .. }
                | ContextFragment::ToolResult { .. }
                | ContextFragment::ReasoningReplay { .. }
        )
    }
}

// ── Context assembly ──

/// Assembled context for a turn.
#[derive(Debug, Clone)]
pub struct AssembledContext {
    pub turn_id: TurnId,
    pub messages: Vec<ContextFragment>,
    pub budget: TokenBudget,
    pub overflow_actions: Vec<OverflowAction>,
}

/// Record of what was done during overflow.
#[derive(Debug, Clone)]
pub enum OverflowAction {
    DroppedPluginCapability { plugin_id: String },
    DroppedInactiveSkill { skill_name: String },
    TruncatedToolOutput { call_id: String, original_tokens: u64, truncated_tokens: u64 },
    CompactedHistory { removed_messages: usize },
}

/// Assemble context from fragments, respecting the token budget.
///
/// Implements PRD §24.3 deterministic V1 overflow order:
/// 1. Drop low-priority plugin capability summaries.
/// 2. Drop inactive skill descriptions.
/// 3. Truncate large tool outputs.
/// 4. Compact older conversation history.
/// 5. Preserve the "never drop" items.
pub fn assemble_context(
    turn_id: TurnId,
    fragments: Vec<ContextFragment>,
    budget_total: u64,
) -> AssembledContext {
    let mut budget = TokenBudget::new(budget_total);
    let mut overflow_actions = Vec::new();

    // Separate fragments by priority.
    let (protected, rest): (Vec<_>, Vec<_>) =
        fragments.into_iter().partition(|f| f.is_protected());

    // Count ALL tokens up front so budget decisions are based on real consumption.
    let protected_tokens: u64 = protected.iter().map(|f| f.estimated_tokens()).sum();
    budget.consume(protected_tokens);

    let mut plugins: Vec<_> = rest.iter().filter(|f| matches!(f, ContextFragment::PluginCapability { .. })).cloned().collect();
    let mut skills: Vec<_> = rest.iter().filter(|f| matches!(f, ContextFragment::SkillInstruction { .. })).cloned().collect();
    let mut history: VecDeque<_> = rest.iter().filter(|f| matches!(f, ContextFragment::ConversationMessage { .. })).cloned().collect();
    let tool_schemas: Vec<_> = rest.iter().filter(|f| matches!(f, ContextFragment::ToolSchemas { .. })).cloned().collect();
    let env: Vec<_> = rest.iter().filter(|f| matches!(f, ContextFragment::Environment { .. })).cloned().collect();

    // Consume all non-protected fragments' estimated tokens to detect overflow.
    let non_protected_tokens: u64 = rest.iter().map(|f| f.estimated_tokens()).sum();
    budget.consume(non_protected_tokens);

    // Overflow strategy in priority order (lowest first):
    // 1. Drop low-priority plugin capability summaries.
    while budget.is_exhausted() && !plugins.is_empty() {
        let dropped = plugins.pop().unwrap();
        if let ContextFragment::PluginCapability { plugin_id, .. } = &dropped {
            let est = dropped.estimated_tokens();
            budget.consumed = budget.consumed.saturating_sub(est);
            overflow_actions.push(OverflowAction::DroppedPluginCapability {
                plugin_id: plugin_id.clone(),
            });
        }
    }

    // 2. Drop inactive skill descriptions.
    while budget.is_exhausted() && !skills.is_empty() {
        let dropped = skills.pop().unwrap();
        if let ContextFragment::SkillInstruction { skill_name, .. } = &dropped {
            let est = dropped.estimated_tokens();
            budget.consumed = budget.consumed.saturating_sub(est);
            overflow_actions.push(OverflowAction::DroppedInactiveSkill {
                skill_name: skill_name.clone(),
            });
        }
    }

    // 3. Truncate large tool outputs (older ones first).
    // Tool results that are protected are in the `protected` vec; any in `rest`
    // are eligible for truncation.
    let mut tool_results: Vec<_> = rest.iter().filter(|f| matches!(f, ContextFragment::ToolResult { .. })).cloned().collect();
    for tr in &mut tool_results {
        if !budget.is_exhausted() {
            break;
        }
        let est = tr.estimated_tokens();
        if est > 500 {
            let truncated = est / 2;
            if let ContextFragment::ToolResult { call_id, .. } = tr {
                budget.consumed = budget.consumed.saturating_sub(est.saturating_sub(truncated));
                overflow_actions.push(OverflowAction::TruncatedToolOutput {
                    call_id: call_id.clone(),
                    original_tokens: est,
                    truncated_tokens: truncated,
                });
            }
        }
    }

    // 4. Compact older conversation history (remove from front = oldest).
    let mut removed = 0usize;
    while budget.is_exhausted() && history.len() > 1 {
        if let Some(oldest) = history.pop_front() {
            let est = oldest.estimated_tokens();
            budget.consumed = budget.consumed.saturating_sub(est);
            removed += 1;
        }
    }
    if removed > 0 {
        overflow_actions.push(OverflowAction::CompactedHistory {
            removed_messages: removed,
        });
    }

    // Assemble final message list: protected first, then everything else.
    let mut final_messages = protected;
    final_messages.extend(tool_schemas);
    final_messages.extend(skills);
    final_messages.extend(plugins);
    final_messages.extend(history);
    final_messages.extend(env);

    // Recalculate consumption.
    let total_consumed: u64 = final_messages.iter().map(|f| f.estimated_tokens()).sum();
    budget.consumed = total_consumed;

    AssembledContext {
        turn_id,
        messages: final_messages,
        budget,
        overflow_actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_budget_defaults() {
        let budget = TokenBudget::new(100_000);
        assert_eq!(budget.total, 100_000);
        assert_eq!(budget.system_tokens, 10_000);
        assert_eq!(budget.history_tokens, 65_000);
        assert_eq!(budget.margin_tokens, 5_000);
        assert_eq!(budget.consumed, 0);
        assert!(budget.remaining() > 0);
    }

    #[test]
    fn test_token_budget_exhaustion() {
        let mut budget = TokenBudget::new(1000);
        budget.consume(960); // 950 cap - 960 = exhausted
        assert!(budget.is_exhausted());
    }

    #[test]
    fn test_never_drop_system() {
        let sys = ContextFragment::System {
            content: "system prompt".into(),
        };
        assert!(sys.is_protected());
    }

    #[test]
    fn test_never_drop_reasoning() {
        let r_replay = ContextFragment::ReasoningReplay {
            message_id: MessageId::from_string("msg-1"),
            content: "thinking...".into(),
        };
        assert!(r_replay.is_protected());
    }

    #[test]
    fn test_assemble_context_truncates_under_budget() {
        let fragments = vec![
            ContextFragment::System { content: "You are an agent.".into() },
            ContextFragment::ToolSchemas { content: "read_file, write_file".into() },
            ContextFragment::ConversationMessage {
                role: "user".into(),
                content: "Fix the bug".into(),
                message_id: Some(MessageId::from_string("msg-2")),
            },
        ];
        let assembled = assemble_context(TurnId::from_string("turn-1"), fragments, 100);
        // Should have all 3 messages since total is tiny.
        assert_eq!(assembled.messages.len(), 3);
        assert!(!assembled.budget.is_exhausted() || assembled.budget.total < 100);
    }

    #[test]
    fn test_budget_debug_metadata() {
        let budget = TokenBudget::new(100_000);
        let meta = budget.debug_metadata();
        assert_eq!(meta.allocation_percent.system, 10);
        assert_eq!(meta.allocation_percent.history, 65);
    }
}

// ── Prompt Builder ──

/// Builds the system prompt with five-layer priority, following the
/// Claude Code / Codex model. A content hash of the static portion
/// enables prompt cache hit detection in providers that support it.
pub struct PromptBuilder {
    /// Override: if set, replaces everything below (loop mode).
    pub override_prompt: Option<String>,
    /// Agent/sub-agent specific prompt.
    pub agent_prompt: Option<String>,
    /// User-customized system prompt from config.
    pub custom_prompt: Option<String>,
    /// Default system prompt (DeepOmni standard).
    pub default_prompt: String,
    /// Always-appended postscript (safety rules, tool constraints).
    pub append_prompt: Option<String>,
}

/// Result of prompt assembly with cache metadata.
pub struct BuiltPrompt {
    /// The assembled system prompt text.
    pub content: String,
    /// SHA-256 hash of the static portion (for cache boundary detection).
    pub static_hash: String,
    /// Whether the prompt is a cache hit candidate.
    pub cacheable: bool,
}

impl PromptBuilder {
    pub fn new(default_prompt: String) -> Self {
        Self {
            override_prompt: None,
            agent_prompt: None,
            custom_prompt: None,
            default_prompt,
            append_prompt: None,
        }
    }

    /// Build the prompt respecting five-layer priority.
    pub fn build(&self) -> BuiltPrompt {
        // Layer 1: Override replaces everything.
        if let Some(ref ov) = self.override_prompt {
            let hash = hash_content(ov);
            return BuiltPrompt {
                content: ov.clone(),
                static_hash: hash,
                cacheable: true,
            };
        }

        let mut parts: Vec<&str> = Vec::new();

        // Layer 2: Agent prompt (replaces default if proactive mode).
        if let Some(ref agent) = self.agent_prompt {
            parts.push(agent.as_str());
        } else {
            // Layer 3-4: Custom overrides default, otherwise use default.
            if let Some(ref custom) = self.custom_prompt {
                parts.push(custom.as_str());
            } else {
                parts.push(&self.default_prompt);
            }
        }

        // Layer 5: Always-appended postscript.
        if let Some(ref append) = self.append_prompt {
            parts.push(append.as_str());
        }

        let content = parts.join("\n\n");
        let hash = hash_content(&content);

        BuiltPrompt {
            content,
            static_hash: hash,
            cacheable: true,
        }
    }

    /// Set the override prompt (highest priority).
    pub fn with_override(mut self, prompt: String) -> Self {
        self.override_prompt = Some(prompt);
        self
    }

    /// Set agent-specific prompt.
    pub fn with_agent(mut self, prompt: String) -> Self {
        self.agent_prompt = Some(prompt);
        self
    }

    /// Set custom prompt from config.
    pub fn with_custom(mut self, prompt: String) -> Self {
        self.custom_prompt = Some(prompt);
        self
    }

    /// Set the always-appended postscript.
    pub fn with_append(mut self, prompt: String) -> Self {
        self.append_prompt = Some(prompt);
        self
    }
}

fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    #[test]
    fn test_default_prompt() {
        let builder = PromptBuilder::new("You are DeepOmni.".into());
        let built = builder.build();
        assert_eq!(built.content, "You are DeepOmni.");
        assert!(!built.static_hash.is_empty());
    }

    #[test]
    fn test_override_replaces_everything() {
        let built = PromptBuilder::new("default".into())
            .with_override("override".into())
            .with_custom("custom".into())
            .with_append("append".into())
            .build();
        assert_eq!(built.content, "override");
    }

    #[test]
    fn test_custom_overrides_default() {
        let built = PromptBuilder::new("default".into())
            .with_custom("custom".into())
            .build();
        assert_eq!(built.content, "custom");
    }

    #[test]
    fn test_agent_replaces_default() {
        let built = PromptBuilder::new("default".into())
            .with_agent("agent".into())
            .with_custom("custom".into())
            .build();
        // Agent prompt replaces default + custom.
        assert!(built.content.contains("agent"));
    }

    #[test]
    fn test_hash_stability() {
        let a = PromptBuilder::new("test".into()).build();
        let b = PromptBuilder::new("test".into()).build();
        assert_eq!(a.static_hash, b.static_hash);
    }
}
