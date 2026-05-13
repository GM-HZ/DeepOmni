//! Phase 5: Multi-agent mailbox minimum closed loop (PLAN.md).
//! Parent-child agent communication through AgentControl + Mailbox.

use deepomni_engine::{AgentControl, AgentRegistry, Mailbox};
use deepomni_protocol::agent::{AgentPath, AgentStatus, InterAgentMessage};
use deepomni_protocol::id::ThreadId;

#[test]
fn test_agent_registry_register_and_list() {
    let registry = AgentRegistry::default();
    let info = deepomni_protocol::agent::AgentInfo {
        thread_id: ThreadId::from_string("child-1"),
        agent_path: AgentPath::from_string("/root/child-1"),
        status: AgentStatus::Running,
        task: Some("review this file".into()),
    };
    registry.register(info.clone());
    let agents = registry.list();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].thread_id.as_str(), "child-1");
}

#[test]
fn test_agent_control_can_spawn_within_limits() {
    let ctrl = AgentControl::new(5, 3);
    assert!(ctrl.can_spawn(0));
    assert!(ctrl.can_spawn(2));
    assert!(!ctrl.can_spawn(3));
}

#[test]
fn test_mailbox_parent_child_communication() {
    let (mailbox, mut receiver) = Mailbox::new();

    // Child sends result to parent.
    let msg = InterAgentMessage {
        author: AgentPath::from_string("/root/child-1"),
        recipient: AgentPath::root(),
        other_recipients: vec![],
        content: "found 3 issues in src/main.rs".into(),
        trigger_turn: true,
    };
    let seq = mailbox.send(msg);
    assert_eq!(seq, 1);

    // Parent receives the message.
    assert!(receiver.has_trigger_turn());
    let drained = receiver.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].content, "found 3 issues in src/main.rs");
    assert!(drained[0].trigger_turn);
}

#[test]
fn test_agent_registry_enforces_limit() {
    let reg = AgentRegistry::default();
    assert!(reg.reserve_slot(2).is_ok());
    assert!(reg.reserve_slot(2).is_ok());
    assert!(reg.reserve_slot(2).is_err());
}

#[test]
fn test_agent_path_allocation() {
    let reg = AgentRegistry::default();
    let path = reg.allocate_path(&AgentPath::root());
    assert!(path.as_str().starts_with("/root/agent-"));
}

#[test]
fn test_agent_registry_remove() {
    let reg = AgentRegistry::default();
    let path = AgentPath::from_string("/root/child-1");
    reg.register(deepomni_protocol::agent::AgentInfo {
        thread_id: ThreadId::from_string("c1"),
        agent_path: path.clone(),
        status: AgentStatus::Running,
        task: None,
    });
    assert_eq!(reg.list().len(), 1);
    reg.remove(&path);
    assert!(reg.list().is_empty());
}

#[test]
fn test_child_failure_notification() {
    let (mailbox, mut receiver) = Mailbox::new();
    let msg = InterAgentMessage {
        author: AgentPath::from_string("/root/child-1"),
        recipient: AgentPath::root(),
        other_recipients: vec![],
        content: "error: command not found".into(),
        trigger_turn: true, // parent should be notified
    };
    mailbox.send(msg);
    assert!(receiver.has_trigger_turn());
    let drained = receiver.drain();
    assert!(drained[0].content.contains("error"));
}
