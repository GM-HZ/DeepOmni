//! # DeepOmni Context
//!
//! Context assembly, token budgeting, and compaction.
//! Implements the deterministic overflow strategy from PRD §24.
//!
//! Based on Codex context fragment patterns.

pub mod compaction;
pub mod normalize;

pub use compaction::{build_compacted_history, build_compaction_request};
pub use normalize::normalize_history;

use std::collections::VecDeque;

use sha2::{Digest, Sha256};

use deepomni_model_provider::{MessageRole, ModelMessage};
use deepomni_protocol::Usage;
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
            system_tokens: total * 10 / 100,  // 10%
            schema_tokens: total * 10 / 100,  // 10%
            skill_tokens: total * 10 / 100,   // 10%
            history_tokens: total * 65 / 100, // 65%
            margin_tokens: total * 5 / 100,   // 5%
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

// ── Context manager ──

/// Policy applied when recording new transcript items.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TruncationPolicy {
    /// Append all items exactly as supplied.
    #[default]
    None,
    /// Keep only the newest `max_items` transcript items.
    KeepLast { max_items: usize },
    /// Keep newest items whose estimated token total fits within `max_tokens`.
    TokenBudget { max_tokens: u64 },
}

/// Snapshot of prompt history and token state for rollback/compaction.
#[derive(Debug, Clone)]
pub struct ContextSnapshot {
    pub items: Vec<ModelMessage>,
    pub history_version: u64,
    pub token_info: Option<TokenUsageInfo>,
}

/// Token accounting captured from the last provider response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUsageInfo {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub context_window: Option<u64>,
}

/// Token-aware conversation history owner.
#[derive(Debug, Clone, Default)]
pub struct ContextManager {
    items: Vec<ModelMessage>,
    history_version: u64,
    token_info: Option<TokenUsageInfo>,
    reference_context: Option<ContextSnapshot>,
    /// Item count at last API usage snapshot, for incremental token estimation.
    last_usage_item_count: usize,
}

impl ContextManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_items(items: Vec<ModelMessage>) -> Self {
        Self {
            items,
            ..Self::default()
        }
    }

    pub fn record_items(&mut self, items: &[ModelMessage], policy: TruncationPolicy) {
        self.items.extend_from_slice(items);
        self.apply_truncation(policy);
    }

    /// Return the current prompt history with normalization applied:
    /// orphan tool results (no matching assistant tool call) are removed.
    pub fn for_prompt(&self) -> Vec<ModelMessage> {
        let mut items = self.items.clone();
        normalize_history(&mut items);
        items
    }

    pub fn replace_history(&mut self, items: Vec<ModelMessage>) {
        self.items = items;
        self.history_version = self.history_version.saturating_add(1);
    }

    pub fn update_token_info(&mut self, usage: &Usage, context_window: Option<u64>) {
        self.last_usage_item_count = self.items.len();
        self.token_info = Some(TokenUsageInfo {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total(),
            context_window,
        });
    }

    /// Estimate total tokens including items added after last API usage.
    fn estimated_local_tokens(&self) -> u64 {
        let items_after = self.items.len().saturating_sub(self.last_usage_item_count);
        if items_after == 0 {
            return 0;
        }
        // Rough estimate: 4 chars per token for items after last usage.
        let text_len: usize = self.items[self.last_usage_item_count..]
            .iter()
            .map(|m| m.content.as_deref().unwrap_or("").len())
            .sum();
        (text_len as u64).div_ceil(4)
    }

    pub fn needs_compaction(&self, threshold: f64) -> bool {
        let Some(info) = &self.token_info else {
            return false;
        };
        let Some(window) = info.context_window else {
            return false;
        };
        if window == 0 {
            return false;
        }
        let estimated = info
            .total_tokens
            .saturating_add(self.estimated_local_tokens());
        (estimated as f64 / window as f64) >= threshold
    }

    pub fn snapshot(&self) -> ContextSnapshot {
        ContextSnapshot {
            items: self.items.clone(),
            history_version: self.history_version,
            token_info: self.token_info.clone(),
        }
    }

    pub fn set_reference_context(&mut self) {
        self.reference_context = Some(self.snapshot());
    }

    pub fn reference_context(&self) -> Option<&ContextSnapshot> {
        self.reference_context.as_ref()
    }

    pub fn history_version(&self) -> u64 {
        self.history_version
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Drop the last N user turns and all following items.
    /// Returns the number of items removed.
    pub fn drop_last_n_user_turns(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        // Find the Nth user message from the end.
        let mut user_count = 0usize;
        let mut cut_index = None;
        for (i, msg) in self.items.iter().enumerate().rev() {
            if msg.role == MessageRole::User {
                user_count += 1;
                if user_count == n {
                    cut_index = Some(i);
                    break;
                }
            }
        }
        let Some(idx) = cut_index else {
            return 0;
        };
        let removed = self.items.len() - idx;
        self.items.truncate(idx);
        self.history_version = self.history_version.saturating_add(1);
        removed
    }

    fn apply_truncation(&mut self, policy: TruncationPolicy) {
        match policy {
            TruncationPolicy::None => {}
            TruncationPolicy::KeepLast { max_items } => {
                let excess = self.items.len().saturating_sub(max_items);
                if excess > 0 {
                    self.items.drain(0..excess);
                    self.history_version = self.history_version.saturating_add(1);
                }
            }
            TruncationPolicy::TokenBudget { max_tokens } => {
                while estimated_messages_tokens(&self.items) > max_tokens && self.items.len() > 1 {
                    self.items.remove(0);
                    self.history_version = self.history_version.saturating_add(1);
                }
            }
        }
    }
}

fn estimated_messages_tokens(items: &[ModelMessage]) -> u64 {
    items
        .iter()
        .map(|item| {
            let content = item.content.as_deref().unwrap_or_default().len();
            let reasoning = item.reasoning_content.as_deref().unwrap_or_default().len();
            let tool_calls = item
                .tool_calls
                .iter()
                .map(|call| call.arguments.to_string().len() + call.tool_name.len())
                .sum::<usize>();
            ((content + reasoning + tool_calls) as u64)
                .div_ceil(4)
                .max(1)
        })
        .sum()
}

#[cfg(test)]
mod context_manager_tests {
    use super::*;
    use deepomni_model_provider::MessageRole;

    fn message(role: MessageRole, content: &str) -> ModelMessage {
        ModelMessage {
            role,
            content: Some(content.to_string()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn records_items_and_preserves_prompt_order() {
        let mut manager = ContextManager::new();
        manager.record_items(
            &[
                message(MessageRole::User, "first"),
                message(MessageRole::Assistant, "second"),
            ],
            TruncationPolicy::None,
        );

        let prompt = manager.for_prompt();
        assert_eq!(prompt.len(), 2);
        assert_eq!(prompt[0].content.as_deref(), Some("first"));
        assert_eq!(prompt[1].content.as_deref(), Some("second"));
        assert_eq!(manager.history_version(), 0);
    }

    #[test]
    fn keep_last_policy_truncates_oldest_items_and_bumps_version() {
        let mut manager = ContextManager::new();
        manager.record_items(
            &[
                message(MessageRole::User, "one"),
                message(MessageRole::Assistant, "two"),
                message(MessageRole::User, "three"),
            ],
            TruncationPolicy::KeepLast { max_items: 2 },
        );

        let prompt = manager.for_prompt();
        assert_eq!(prompt.len(), 2);
        assert_eq!(prompt[0].content.as_deref(), Some("two"));
        assert_eq!(prompt[1].content.as_deref(), Some("three"));
        assert_eq!(manager.history_version(), 1);
    }

    #[test]
    fn replace_history_increments_version_and_updates_snapshot() {
        let mut manager = ContextManager::new();
        manager.record_items(&[message(MessageRole::User, "old")], TruncationPolicy::None);
        manager.replace_history(vec![message(MessageRole::User, "summary")]);
        manager.set_reference_context();

        assert_eq!(manager.history_version(), 1);
        assert_eq!(manager.reference_context().unwrap().items.len(), 1);
        assert_eq!(
            manager.reference_context().unwrap().items[0]
                .content
                .as_deref(),
            Some("summary")
        );
    }

    #[test]
    fn compaction_threshold_uses_last_usage_window() {
        let mut manager = ContextManager::new();
        manager.update_token_info(
            &Usage {
                prompt_tokens: 750,
                completion_tokens: 50,
                ..Default::default()
            },
            Some(1000),
        );

        assert!(manager.needs_compaction(0.8));
        assert!(!manager.needs_compaction(0.9));
    }

    #[test]
    fn for_prompt_removes_orphan_tool_outputs() {
        use deepomni_model_provider::ModelToolCall;
        let mut manager = ContextManager::new();
        // Assistant tool call for call-1.
        manager.record_items(
            &[
                ModelMessage {
                    role: MessageRole::Assistant,
                    content: None,
                    tool_calls: vec![ModelToolCall {
                        call_id: "call-1".into(),
                        tool_name: "read".into(),
                        arguments: serde_json::Value::Null,
                    }],
                    tool_call_id: None,
                    reasoning_content: None,
                },
                // Matching tool result.
                ModelMessage {
                    role: MessageRole::Tool,
                    content: Some("result".into()),
                    tool_calls: vec![],
                    tool_call_id: Some("call-1".into()),
                    reasoning_content: None,
                },
                // Orphan tool result (no matching call).
                ModelMessage {
                    role: MessageRole::Tool,
                    content: Some("orphan".into()),
                    tool_calls: vec![],
                    tool_call_id: Some("call-orphan".into()),
                    reasoning_content: None,
                },
            ],
            TruncationPolicy::None,
        );

        let prompt = manager.for_prompt();
        assert_eq!(prompt.len(), 2, "orphan tool result should be removed");
        assert!(
            prompt
                .iter()
                .all(|m| m.tool_call_id.as_deref() != Some("call-orphan"))
        );
    }

    /// Plan 3 Task 3: Token usage accounts for items added after last API usage.
    #[test]
    fn token_usage_includes_local_items_after_last_usage() {
        let mut manager = ContextManager::new();
        manager.update_token_info(
            &Usage {
                prompt_tokens: 700,
                completion_tokens: 50,
                ..Default::default()
            },
            Some(1000),
        );
        // Add 400 chars ≈ 100 tokens of new local items.
        manager.record_items(
            &[message(MessageRole::User, &"x".repeat(400))],
            TruncationPolicy::None,
        );
        // Total: 750 (prompt+completion) + ~100 (local) = ~850, window=1000 → 85%
        assert!(
            manager.needs_compaction(0.8),
            "should need compaction with local items pushing past 80%"
        );
    }
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
    ConversationMessage {
        role: String,
        content: String,
        message_id: Option<MessageId>,
    },
    /// Tool call result (pending continuation material — never dropped).
    ToolResult { call_id: String, content: String },
    /// Reasoning content required for replay (never dropped).
    ReasoningReplay {
        message_id: MessageId,
        content: String,
    },
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
            ContextFragment::ToolResult { .. } | ContextFragment::ReasoningReplay { .. } => {
                FragmentRole::User
            }
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
    DroppedPluginCapability {
        plugin_id: String,
    },
    DroppedInactiveSkill {
        skill_name: String,
    },
    TruncatedToolOutput {
        call_id: String,
        original_tokens: u64,
        truncated_tokens: u64,
    },
    CompactedHistory {
        removed_messages: usize,
    },
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
    let (protected, rest): (Vec<_>, Vec<_>) = fragments.into_iter().partition(|f| f.is_protected());

    // Count ALL tokens up front so budget decisions are based on real consumption.
    let protected_tokens: u64 = protected.iter().map(|f| f.estimated_tokens()).sum();
    budget.consume(protected_tokens);

    let mut plugins: Vec<_> = rest
        .iter()
        .filter(|f| matches!(f, ContextFragment::PluginCapability { .. }))
        .cloned()
        .collect();
    let mut skills: Vec<_> = rest
        .iter()
        .filter(|f| matches!(f, ContextFragment::SkillInstruction { .. }))
        .cloned()
        .collect();
    let mut history: VecDeque<_> = rest
        .iter()
        .filter(|f| matches!(f, ContextFragment::ConversationMessage { .. }))
        .cloned()
        .collect();
    let tool_schemas: Vec<_> = rest
        .iter()
        .filter(|f| matches!(f, ContextFragment::ToolSchemas { .. }))
        .cloned()
        .collect();
    let env: Vec<_> = rest
        .iter()
        .filter(|f| matches!(f, ContextFragment::Environment { .. }))
        .cloned()
        .collect();

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
    let mut tool_results: Vec<_> = rest
        .iter()
        .filter(|f| matches!(f, ContextFragment::ToolResult { .. }))
        .cloned()
        .collect();
    for tr in &mut tool_results {
        if !budget.is_exhausted() {
            break;
        }
        let est = tr.estimated_tokens();
        if est > 500 {
            let truncated = est / 2;
            if let ContextFragment::ToolResult { call_id, .. } = tr {
                budget.consumed = budget
                    .consumed
                    .saturating_sub(est.saturating_sub(truncated));
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
            ContextFragment::System {
                content: "You are an agent.".into(),
            },
            ContextFragment::ToolSchemas {
                content: "read_file, write_file".into(),
            },
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
