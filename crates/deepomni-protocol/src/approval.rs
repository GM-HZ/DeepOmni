//! Approval request and decision types.
//!
//! Hosts receive approval requests via events and respond with structured
//! decisions that flow back through the policy engine to the tool orchestrator.

use serde::{Deserialize, Serialize};

use crate::id::ToolCallId;
use crate::id::TurnId;

/// An approval request emitted to the host when a tool needs authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub turn_id: TurnId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub reason: String,
    /// Available decisions the host can make.
    pub available_decisions: Vec<ApprovalDecisionKind>,
}

/// The host's response to an approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecision {
    pub approval_id: String,
    pub decision: ApprovalDecisionKind,
    /// If true, remember this decision for the session.
    #[serde(default)]
    pub remember: bool,
}

/// Kinds of approval decisions a host can make.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionKind {
    /// Allow this execution once.
    Approved,
    /// Allow and remember for the session.
    ApprovedForSession,
    /// Apply a proposed policy amendment and approve.
    ApprovedExecpolicyAmendment,
    /// Deny this execution.
    Denied,
    /// Abort the current turn.
    Abort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_approval_request_json() {
        let req = ApprovalRequest {
            approval_id: "approval-1".into(),
            turn_id: TurnId::from_string("turn-5"),
            call_id: ToolCallId::from_string("call-3"),
            tool_name: "shell_exec".into(),
            reason: "Mutating command: rm -rf".into(),
            available_decisions: vec![
                ApprovalDecisionKind::Approved,
                ApprovalDecisionKind::Denied,
                ApprovalDecisionKind::Abort,
            ],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("shell_exec"));
        assert!(json.contains("approved"));

        let parsed: ApprovalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "shell_exec");
        assert_eq!(parsed.available_decisions.len(), 3);
    }

    #[test]
    fn test_approval_decision_json() {
        let dec = ApprovalDecision {
            approval_id: "approval-1".into(),
            decision: ApprovalDecisionKind::ApprovedForSession,
            remember: true,
        };
        let json = serde_json::to_string(&dec).unwrap();
        let parsed: ApprovalDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.decision, ApprovalDecisionKind::ApprovedForSession);
        assert!(parsed.remember);
    }
}
