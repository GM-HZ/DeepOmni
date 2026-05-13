//! Projection service — subscribes to journal and projects records
//! to EventFrame for the EventBus. Phase G: extracted from Runtime.

use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

use deepomni_events::EventBus;
use deepomni_journal::{TurnJournal, journal_to_event_frame};
use deepomni_protocol::id::ThreadId;

/// Bridges durable journal storage → live EventBus broadcast.
/// One background task per thread subscribes to the journal and
/// projects JournalRecord → EventFrame for SSE consumers.
pub struct ProjectionService {
    state: Arc<dyn TurnJournal>,
    event_bus: Arc<EventBus>,
    active_threads: RwLock<HashSet<ThreadId>>,
}

impl ProjectionService {
    pub fn new(state: Arc<dyn TurnJournal>, event_bus: Arc<EventBus>) -> Self {
        Self {
            state,
            event_bus,
            active_threads: RwLock::new(HashSet::new()),
        }
    }

    /// Start the journal→EventBus projection for a thread. Idempotent:
    /// calling multiple times for the same thread is safe (no double-spawn).
    pub async fn ensure_projection(&self, thread_id: ThreadId) {
        {
            let mut active = self.active_threads.write().await;
            if !active.insert(thread_id.clone()) {
                return;
            }
        }

        let mut journal_subscriber = self.state.subscribe(thread_id);
        let event_bus = self.event_bus.clone();
        tokio::spawn(async move {
            loop {
                match journal_subscriber.recv().await {
                    Ok(record) => {
                        if let Some(event) = journal_to_event_frame(&record) {
                            let _ = event_bus
                                .emit(record.thread_id.clone(), record.seq, event)
                                .await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}
