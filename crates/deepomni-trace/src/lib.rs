//! Trace writer trait and no-op implementation for rollout tracing.
//! Pattern from Codex: hot path only writes; semantic replay is external.

use deepomni_protocol::id::{ThreadId, TurnId};

/// Trace writer trait. Default no-op implementation has zero overhead.
pub trait TraceWriter: Send + Sync {
    fn record_turn_started(&self, thread_id: &ThreadId, turn_id: &TurnId);
    fn record_inference_attempt(&self, thread_id: &ThreadId, turn_id: &TurnId, model: &str);
    fn record_tool_dispatch(&self, thread_id: &ThreadId, turn_id: &TurnId, tool_name: &str);
    fn record_compaction(&self, thread_id: &ThreadId, before_tokens: u64, after_tokens: u64);
    fn record_agent_spawn(&self, parent: &ThreadId, child: &ThreadId);
    fn record_turn_completed(&self, thread_id: &ThreadId, turn_id: &TurnId);
}

/// Default no-op implementation.
pub struct NoopTraceWriter;

impl TraceWriter for NoopTraceWriter {
    fn record_turn_started(&self, _: &ThreadId, _: &TurnId) {}
    fn record_inference_attempt(&self, _: &ThreadId, _: &TurnId, _: &str) {}
    fn record_tool_dispatch(&self, _: &ThreadId, _: &TurnId, _: &str) {}
    fn record_compaction(&self, _: &ThreadId, _: u64, _: u64) {}
    fn record_agent_spawn(&self, _: &ThreadId, _: &ThreadId) {}
    fn record_turn_completed(&self, _: &ThreadId, _: &TurnId) {}
}

/// In-memory trace writer for testing.
#[derive(Default)]
pub struct MemoryTraceWriter {
    events: std::sync::Mutex<Vec<String>>,
}

impl MemoryTraceWriter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

impl TraceWriter for MemoryTraceWriter {
    fn record_turn_started(&self, tid: &ThreadId, turn_id: &TurnId) {
        self.events
            .lock()
            .unwrap()
            .push(format!("turn_started:{tid}:{turn_id}"));
    }
    fn record_inference_attempt(&self, tid: &ThreadId, turn_id: &TurnId, model: &str) {
        self.events
            .lock()
            .unwrap()
            .push(format!("inference:{tid}:{turn_id}:{model}"));
    }
    fn record_tool_dispatch(&self, tid: &ThreadId, turn_id: &TurnId, tool: &str) {
        self.events
            .lock()
            .unwrap()
            .push(format!("tool:{tid}:{turn_id}:{tool}"));
    }
    fn record_compaction(&self, tid: &ThreadId, before: u64, after: u64) {
        self.events
            .lock()
            .unwrap()
            .push(format!("compact:{tid}:{before}:{after}"));
    }
    fn record_agent_spawn(&self, parent: &ThreadId, child: &ThreadId) {
        self.events
            .lock()
            .unwrap()
            .push(format!("agent_spawn:{parent}:{child}"));
    }
    fn record_turn_completed(&self, tid: &ThreadId, turn_id: &TurnId) {
        self.events
            .lock()
            .unwrap()
            .push(format!("turn_completed:{tid}:{turn_id}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noop_accepts_all_calls() {
        let writer = NoopTraceWriter;
        writer.record_turn_started(&ThreadId::from_string("t1"), &TurnId::from_string("tu1"));
        writer.record_compaction(&ThreadId::from_string("t1"), 1000, 500);
    }

    #[test]
    fn test_memory_trace_records_events() {
        let writer = MemoryTraceWriter::new();
        writer.record_turn_started(&ThreadId::from_string("t1"), &TurnId::from_string("tu1"));
        writer.record_tool_dispatch(
            &ThreadId::from_string("t1"),
            &TurnId::from_string("tu1"),
            "read_file",
        );
        let events = writer.events();
        assert_eq!(events.len(), 2);
        assert!(events[0].contains("turn_started"));
        assert!(events[1].contains("read_file"));
    }
}
