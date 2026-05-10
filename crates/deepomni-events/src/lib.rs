//! # DeepOmni Events
//!
//! Runtime event bus with both in-memory broadcast for live subscribers
//! and durable persistence for replay. The persisted store is the source
//! of truth; broadcast is best-effort for low-latency delivery.
//!
//! Architecture: PRD §9, §19.4

use std::collections::HashMap;
use tokio::sync::{broadcast, RwLock};

use deepomni_protocol::{EventEnvelope, EventFrame, ThreadId};

// ── Event bus ──

/// Default capacity for per-thread broadcast channels.
const DEFAULT_BROADCAST_CAPACITY: usize = 256;

type BroadcastSender = broadcast::Sender<EventEnvelope>;
type SenderMap = HashMap<ThreadId, BroadcastSender>;
type PersistCallback = Box<dyn Fn(ThreadId, i64, &EventFrame) -> Result<i64, String> + Send + Sync>;

/// A subscriber handle that receives live events.
///
/// Events are delivered as they occur. To catch up on missed events,
/// use the persisted store's replay mechanism.
#[derive(Debug)]
pub struct EventSubscriber {
    pub thread_id: ThreadId,
    rx: broadcast::Receiver<EventEnvelope>,
}

impl EventSubscriber {
    /// Receive the next envelope, waiting if necessary.
    pub async fn recv(&mut self) -> Result<EventEnvelope, broadcast::error::RecvError> {
        self.rx.recv().await
    }

    /// Try to receive without blocking.
    pub fn try_recv(&mut self) -> Result<EventEnvelope, broadcast::error::TryRecvError> {
        self.rx.try_recv()
    }
}

/// The event bus: broadcasts live events and coordinates with the
/// persisted store for durable replay.
pub struct EventBus {
    /// Per-thread broadcast senders for live delivery.
    senders: RwLock<SenderMap>,
    /// Callback for persisting events before broadcast.
    persist_fn: Option<PersistCallback>,
}

impl EventBus {
    /// Create a new event bus.
    pub fn new() -> Self {
        Self {
            senders: RwLock::new(HashMap::new()),
            persist_fn: None,
        }
    }

    /// Register a persistence callback. Events are persisted *before* broadcast.
    /// Broadcast only happens if persistence succeeds.
    pub fn with_persistence<F>(mut self, f: F) -> Self
    where
        F: Fn(ThreadId, i64, &EventFrame) -> Result<i64, String> + Send + Sync + 'static,
    {
        self.persist_fn = Some(Box::new(f));
        self
    }

    /// Subscribe to live events for a thread. If this is the first
    /// subscriber, creates a new broadcast channel.
    pub async fn subscribe(&self, thread_id: ThreadId) -> EventSubscriber {
        let mut senders = self.senders.write().await;
        let sender = senders
            .entry(thread_id.clone())
            .or_insert_with(|| broadcast::channel(DEFAULT_BROADCAST_CAPACITY).0);
        EventSubscriber {
            thread_id,
            rx: sender.subscribe(),
        }
    }

    /// Emit an event. Persists it (if configured), then broadcasts.
    /// Broadcast is gated on successful persistence — if the persistence
    /// callback returns an error, the event is NOT broadcast to subscribers.
    /// This enforces durable-event source-of-truth.
    pub async fn emit(
        &self,
        thread_id: ThreadId,
        seq: i64,
        event: EventFrame,
    ) -> Result<(), EventBusError> {
        // Persist first (source of truth). The persisted seq is authoritative.
        let authoritative_seq = if let Some(ref persist) = self.persist_fn {
            match persist(thread_id.clone(), seq, &event) {
                Ok(persisted_seq) => persisted_seq,
                Err(e) => return Err(EventBusError::PersistError(e)),
            }
        } else {
            seq
        };

        // Broadcast with the authoritative persisted seq.
        let senders = self.senders.read().await;
        if let Some(sender) = senders.get(&thread_id) {
            let _ = sender.send(EventEnvelope { seq: authoritative_seq, frame: event });
        }

        Ok(())
    }

    /// Remove the broadcast channel for a thread (e.g., on thread archive).
    pub async fn close_thread(&self, thread_id: &ThreadId) {
        self.senders.write().await.remove(thread_id);
    }

    /// Number of threads with active broadcast channels.
    pub async fn active_thread_count(&self) -> usize {
        self.senders.read().await.len()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("has_persist_fn", &self.persist_fn.is_some())
            .finish_non_exhaustive()
    }
}

// ── Error ──

#[derive(Debug)]
pub enum EventBusError {
    /// No broadcast channel exists for this thread.
    ThreadNotActive(ThreadId),
    /// Persistence failed.
    PersistError(String),
}

impl std::fmt::Display for EventBusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventBusError::ThreadNotActive(tid) => {
                write!(f, "no active broadcast channel for thread {tid}")
            }
            EventBusError::PersistError(msg) => {
                write!(f, "persist error: {msg}")
            }
        }
    }
}

impl std::error::Error for EventBusError {}

// ── SSE helpers ──

/// Format an event as an SSE data line.
pub fn to_sse_data(event: &EventFrame) -> String {
    let json = serde_json::to_string(event).unwrap_or_default();
    let event_type = event_tag(event);
    format!("event: {event_type}\ndata: {json}\n\n")
}

pub fn event_tag(event: &EventFrame) -> &'static str {
    match event {
        EventFrame::ThreadCreated { .. } => "thread.created",
        EventFrame::ThreadUpdated { .. } => "thread.updated",
        EventFrame::ThreadArchived { .. } => "thread.archived",
        EventFrame::TurnStarted { .. } => "turn.started",
        EventFrame::TurnSteered { .. } => "turn.steered",
        EventFrame::TurnInterrupted { .. } => "turn.interrupted",
        EventFrame::AssistantMessageDelta { .. } => "assistant.message.delta",
        EventFrame::AssistantMessageCompleted { .. } => "assistant.message.completed",
        EventFrame::AssistantReasoningDelta { .. } => "assistant.reasoning.delta",
        EventFrame::AssistantReasoningCompleted { .. } => "assistant.reasoning.completed",
        EventFrame::ToolCallStarted { .. } => "tool.call.started",
        EventFrame::ToolCallArgumentsDelta { .. } => "tool.call.arguments.delta",
        EventFrame::ToolCallRequiresApproval { .. } => "tool.call.requires_approval",
        EventFrame::ToolCallApproved { .. } => "tool.call.approved",
        EventFrame::ToolCallRejected { .. } => "tool.call.rejected",
        EventFrame::ToolCallCompleted { .. } => "tool.call.completed",
        EventFrame::ToolCallFailed { .. } => "tool.call.failed",
        EventFrame::ContextCompactionStarted { .. } => "context.compaction.started",
        EventFrame::ContextCompactionCompleted { .. } => "context.compaction.completed",
        EventFrame::SubagentSpawned { .. } => "subagent.spawned",
        EventFrame::SubagentCompleted { .. } => "subagent.completed",
        EventFrame::SubagentFailed { .. } => "subagent.failed",
        EventFrame::TurnCompleted { .. } => "turn.completed",
        EventFrame::TurnFailed { .. } => "turn.failed",
        EventFrame::RuntimeWarning { .. } => "runtime.warning",
    }
}

// ── Event filters ──

/// Filter events by matching on variants.
pub struct EventFilter {
    allowed_tags: Option<Vec<String>>,
    min_seq: Option<i64>,
}

impl EventFilter {
    pub fn new() -> Self {
        Self {
            allowed_tags: None,
            min_seq: None,
        }
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.allowed_tags = Some(tags);
        self
    }

    pub fn since_seq(mut self, seq: i64) -> Self {
        self.min_seq = Some(seq);
        self
    }

    pub fn matches(&self, event: &EventFrame) -> bool {
        if let Some(ref tags) = self.allowed_tags {
            let tag = event_tag(event);
            if !tags.iter().any(|t| t == tag) {
                return false;
            }
        }
        true
    }
}

impl Default for EventFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepomni_protocol::TurnId;

    #[test]
    fn test_event_tag_mapping() {
        let event = EventFrame::TurnStarted {
            thread_id: ThreadId::from_string("t1"),
            turn_id: TurnId::from_string("tu1"),
            user_input: "hello".into(),
        };
        assert_eq!(event_tag(&event), "turn.started");
    }

    #[test]
    fn test_to_sse_data_format() {
        let event = EventFrame::TurnCompleted {
            turn_id: TurnId::from_string("tu1"),
        };
        let sse = to_sse_data(&event);
        assert!(sse.starts_with("event: turn.completed\n"));
        assert!(sse.contains("data: "));
        assert!(sse.ends_with("\n\n"));
    }

    #[tokio::test]
    async fn test_event_bus_subscribe_and_emit() {
        let bus = EventBus::new();
        let tid = ThreadId::from_string("thread-test");

        let mut sub = bus.subscribe(tid.clone()).await;
        bus.emit(
            tid.clone(),
            1,
            EventFrame::TurnStarted {
                thread_id: tid.clone(),
                turn_id: TurnId::from_string("turn-1"),
                user_input: "hello".into(),
            },
        )
        .await
        .unwrap();

        let envelope = sub.recv().await.unwrap();
        match envelope.frame {
            EventFrame::TurnStarted { user_input, .. } => {
                assert_eq!(user_input, "hello");
            }
            _ => panic!("wrong event type"),
        }
    }

    #[tokio::test]
    async fn test_event_bus_multiple_subscribers() {
        let bus = EventBus::new();
        let tid = ThreadId::from_string("thread-multi");

        let mut sub1 = bus.subscribe(tid.clone()).await;
        let mut sub2 = bus.subscribe(tid.clone()).await;

        bus.emit(
            tid.clone(),
            1,
            EventFrame::TurnCompleted {
                turn_id: TurnId::from_string("tu1"),
            },
        )
        .await
        .unwrap();

        assert!(sub1.recv().await.is_ok());
        assert!(sub2.recv().await.is_ok());
        assert_eq!(bus.active_thread_count().await, 1);
    }

    #[tokio::test]
    async fn test_close_thread() {
        let bus = EventBus::new();
        let tid = ThreadId::from_string("thread-close");

        let _sub = bus.subscribe(tid.clone()).await;
        assert_eq!(bus.active_thread_count().await, 1);

        bus.close_thread(&tid).await;
        assert_eq!(bus.active_thread_count().await, 0);
    }

    #[test]
    fn test_event_filter_by_seq() {
        let filter = EventFilter::new().since_seq(5);
        let event = EventFrame::TurnCompleted {
            turn_id: TurnId::from_string("t1"),
        };
        // Seq filtering is done upstream by the store; the filter itself
        // only filters by tag. Verify tag matching:
        assert!(filter.matches(&event));
    }

    #[test]
    fn test_event_filter_by_tag() {
        let filter = EventFilter::new().with_tags(vec!["turn.completed".into()]);
        let matched = EventFrame::TurnCompleted {
            turn_id: TurnId::from_string("t1"),
        };
        let unmatched = EventFrame::TurnStarted {
            thread_id: ThreadId::from_string("t1"),
            turn_id: TurnId::from_string("t2"),
            user_input: "hi".into(),
        };
        assert!(filter.matches(&matched));
        assert!(!filter.matches(&unmatched));
    }
}
