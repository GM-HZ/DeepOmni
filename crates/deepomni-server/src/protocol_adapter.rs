//! Protocol adapter — translates core EventFrame to host-safe ServerNotification/ServerRequest.
//! Pattern from Codex App Server bespoke_event_handling.rs.

use deepomni_protocol::EventFrame;
use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_protocol::server_message::{ServerNotification, ServerRequest};

/// Context carrying thread/turn identity for events that don't include it.
#[derive(Debug, Clone)]
pub struct TranslationContext {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub approval_id: Option<String>,
}

/// Translate a core EventFrame into a server-level message, using context
/// to fill thread_id/turn_id for events that don't carry them explicitly.
pub fn event_to_server_message(
    event: &EventFrame,
    ctx: &TranslationContext,
) -> Option<ServerMessage> {
    match event {
        EventFrame::TurnStarted {
            thread_id,
            turn_id,
            user_input,
        } => Some(ServerMessage::Notification(
            ServerNotification::TurnStarted {
                thread_id: thread_id.clone(),
                turn_id: turn_id.clone(),
                user_input: user_input.clone(),
            },
        )),
        EventFrame::AssistantMessageDelta { delta, .. } => Some(ServerMessage::Notification(
            ServerNotification::AssistantDelta {
                thread_id: ctx.thread_id.clone(),
                turn_id: ctx.turn_id.clone(),
                delta: delta.clone(),
            },
        )),
        EventFrame::TurnCompleted { .. } => Some(ServerMessage::Notification(
            ServerNotification::TurnCompleted {
                thread_id: ctx.thread_id.clone(),
                turn_id: ctx.turn_id.clone(),
            },
        )),
        EventFrame::TurnFailed { error, .. } => Some(ServerMessage::Notification(
            ServerNotification::TurnFailed {
                thread_id: ctx.thread_id.clone(),
                turn_id: ctx.turn_id.clone(),
                error: error.clone(),
            },
        )),
        EventFrame::ToolCallRequiresApproval {
            call_id,
            tool_name,
            reason,
            ..
        } => Some(ServerMessage::Request(ServerRequest::ApprovalNeeded {
            thread_id: ctx.thread_id.clone(),
            turn_id: ctx.turn_id.clone(),
            approval_id: ctx
                .approval_id
                .clone()
                .unwrap_or_else(|| call_id.to_string()),
            tool_name: tool_name.clone(),
            reason: reason.clone(),
        })),
        EventFrame::ToolCallCompleted {
            tool_name, success, ..
        } => Some(ServerMessage::Notification(
            ServerNotification::ToolCallCompleted {
                thread_id: ctx.thread_id.clone(),
                turn_id: ctx.turn_id.clone(),
                tool_name: tool_name.clone(),
                success: *success,
            },
        )),
        _ => None,
    }
}

/// Translate a client response into an Op for SessionLoop submission.
/// Plan 4 Task 5: keeps the adapter pure — server handlers don't need to
/// know Op internals.
pub fn client_response_to_op(
    response: &deepomni_protocol::server_message::ClientResponse,
) -> deepomni_protocol::op::Op {
    match response {
        deepomni_protocol::server_message::ClientResponse::ApprovalDecision {
            thread_id,
            approval_id,
            approved,
        } => deepomni_protocol::op::Op::ApprovalDecision {
            thread_id: thread_id.clone(),
            approval_id: approval_id.clone(),
            approved: *approved,
        },
    }
}

/// Either a notification or a request.
#[derive(Debug, Clone)]
pub enum ServerMessage {
    Notification(ServerNotification),
    Request(ServerRequest),
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepomni_protocol::EventFrame;
    use deepomni_protocol::id::{ThreadId, TurnId};

    fn test_ctx() -> TranslationContext {
        TranslationContext {
            thread_id: ThreadId::from_string("thread-test"),
            turn_id: TurnId::from_string("turn-test"),
            approval_id: Some("approval-test".into()),
        }
    }

    #[test]
    fn test_turn_started_has_real_ids() {
        let event = EventFrame::TurnStarted {
            thread_id: ThreadId::from_string("t1"),
            turn_id: TurnId::from_string("tu1"),
            user_input: "hello".into(),
        };
        let msg = event_to_server_message(&event, &test_ctx()).unwrap();
        match msg {
            ServerMessage::Notification(n) => match n {
                ServerNotification::TurnStarted {
                    thread_id,
                    turn_id,
                    user_input,
                } => {
                    assert_eq!(thread_id.as_str(), "t1");
                    assert_eq!(turn_id.as_str(), "tu1");
                    assert_eq!(user_input, "hello");
                }
                _ => panic!("wrong notification"),
            },
            _ => panic!("expected notification"),
        }
    }

    #[test]
    fn test_assistant_delta_has_real_ids_from_context() {
        let event = EventFrame::AssistantMessageDelta {
            turn_id: TurnId::from_string("tu-delta"),
            message_id: deepomni_protocol::id::MessageId::from_string("msg-1"),
            delta: "hi".into(),
        };
        let msg = event_to_server_message(&event, &test_ctx()).unwrap();
        match msg {
            ServerMessage::Notification(n) => match n {
                ServerNotification::AssistantDelta {
                    thread_id, delta, ..
                } => {
                    assert_eq!(thread_id.as_str(), "thread-test");
                    assert_eq!(delta, "hi");
                }
                _ => panic!("wrong notification"),
            },
            _ => panic!("expected notification"),
        }
    }

    #[test]
    fn test_approval_needed_has_real_approval_id() {
        let event = EventFrame::ToolCallRequiresApproval {
            turn_id: TurnId::from_string("tu-approval"),
            call_id: deepomni_protocol::id::ToolCallId::from_string("call-1"),
            tool_name: "write_file".into(),
            reason: "mutating tool".into(),
        };
        let msg = event_to_server_message(&event, &test_ctx()).unwrap();
        match msg {
            ServerMessage::Request(r) => match r {
                ServerRequest::ApprovalNeeded {
                    thread_id,
                    approval_id,
                    tool_name,
                    ..
                } => {
                    assert_eq!(thread_id.as_str(), "thread-test");
                    assert_eq!(approval_id, "approval-test");
                    assert_eq!(tool_name, "write_file");
                }
            },
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn test_no_placeholder_identities_in_output() {
        let ctx = TranslationContext {
            thread_id: ThreadId::from_string("real-thread"),
            turn_id: TurnId::from_string("real-turn"),
            approval_id: Some("real-approval".into()),
        };
        let events = vec![
            EventFrame::TurnCompleted {
                turn_id: TurnId::from_string("tu1"),
            },
            EventFrame::TurnFailed {
                turn_id: TurnId::from_string("tu1"),
                error: "err".into(),
            },
            EventFrame::AssistantMessageDelta {
                turn_id: TurnId::from_string("tu1"),
                message_id: deepomni_protocol::id::MessageId::from_string("m1"),
                delta: "d".into(),
            },
        ];
        for event in &events {
            if let Some(msg) = event_to_server_message(event, &ctx) {
                match msg {
                    ServerMessage::Notification(n) => {
                        let (tid, tvid) = match n {
                            ServerNotification::TurnStarted {
                                ref thread_id,
                                ref turn_id,
                                ..
                            } => (thread_id.as_str(), turn_id.as_str()),
                            ServerNotification::AssistantDelta {
                                ref thread_id,
                                ref turn_id,
                                ..
                            } => (thread_id.as_str(), turn_id.as_str()),
                            ServerNotification::TurnCompleted {
                                ref thread_id,
                                ref turn_id,
                                ..
                            } => (thread_id.as_str(), turn_id.as_str()),
                            ServerNotification::TurnFailed {
                                ref thread_id,
                                ref turn_id,
                                ..
                            } => (thread_id.as_str(), turn_id.as_str()),
                            ServerNotification::ToolCallCompleted {
                                ref thread_id,
                                ref turn_id,
                                ..
                            } => (thread_id.as_str(), turn_id.as_str()),
                            _ => continue,
                        };
                        assert!(!tid.is_empty(), "thread_id must not be empty");
                        assert!(!tvid.is_empty(), "turn_id must not be empty");
                    }
                    ServerMessage::Request(ServerRequest::ApprovalNeeded {
                        ref approval_id,
                        ..
                    }) => {
                        assert!(!approval_id.is_empty(), "approval_id must not be empty");
                    }
                }
            }
        }
    }

    /// Plan 4 Task 5: client_response_to_op translates ApprovalDecision to Op.
    #[test]
    fn test_client_response_to_op_approval_decision() {
        use deepomni_protocol::server_message::ClientResponse;
        let response = ClientResponse::ApprovalDecision {
            thread_id: ThreadId::from_string("t1"),
            approval_id: "approval-1".into(),
            approved: true,
        };
        let op = client_response_to_op(&response);
        match op {
            deepomni_protocol::op::Op::ApprovalDecision {
                thread_id,
                approval_id,
                approved,
            } => {
                assert_eq!(thread_id.as_str(), "t1");
                assert_eq!(approval_id, "approval-1");
                assert!(approved);
            }
            other => panic!("expected ApprovalDecision op, got {other:?}"),
        }
    }

    #[test]
    fn test_client_response_to_op_rejected_approval() {
        use deepomni_protocol::server_message::ClientResponse;
        let response = ClientResponse::ApprovalDecision {
            thread_id: ThreadId::from_string("t2"),
            approval_id: "approval-2".into(),
            approved: false,
        };
        let op = client_response_to_op(&response);
        match op {
            deepomni_protocol::op::Op::ApprovalDecision {
                thread_id,
                approved,
                ..
            } => {
                assert_eq!(thread_id.as_str(), "t2");
                assert!(!approved);
            }
            other => panic!("expected ApprovalDecision op, got {other:?}"),
        }
    }
}
