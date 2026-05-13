//! Agent Mailbox — inter-agent communication channel.
//! Pattern from Codex: mpsc channel + watch notify + sequence numbering.

use deepomni_protocol::agent::InterAgentMessage;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, watch};

/// Sending end of a mailbox.
pub struct Mailbox {
    tx: mpsc::UnboundedSender<InterAgentMessage>,
    next_seq: AtomicU64,
    seq_tx: watch::Sender<u64>,
}

/// Receiving end of a mailbox.
pub struct MailboxReceiver {
    rx: mpsc::UnboundedReceiver<InterAgentMessage>,
    pending: VecDeque<InterAgentMessage>,
}

impl Mailbox {
    pub fn new() -> (Self, MailboxReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (seq_tx, _) = watch::channel(0);
        (
            Self {
                tx,
                next_seq: AtomicU64::new(0),
                seq_tx,
            },
            MailboxReceiver {
                rx,
                pending: VecDeque::new(),
            },
        )
    }

    /// Send a message, returning the assigned sequence number.
    pub fn send(&self, msg: InterAgentMessage) -> u64 {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.tx.send(msg);
        self.seq_tx.send_replace(seq);
        seq
    }

    /// Subscribe to sequence number changes (for select! wakeup).
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.seq_tx.subscribe()
    }
}

impl MailboxReceiver {
    fn sync(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.pending.push_back(msg);
        }
    }

    pub fn has_pending(&mut self) -> bool {
        self.sync();
        !self.pending.is_empty()
    }

    pub fn has_trigger_turn(&mut self) -> bool {
        self.sync();
        self.pending.iter().any(|m| m.trigger_turn)
    }

    pub fn drain(&mut self) -> Vec<InterAgentMessage> {
        self.sync();
        self.pending.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mailbox_send_receive() {
        let (mailbox, mut receiver) = Mailbox::new();
        let msg = InterAgentMessage {
            author: deepomni_protocol::agent::AgentPath::root(),
            recipient: deepomni_protocol::agent::AgentPath::from_string("/root/child"),
            other_recipients: Vec::new(),
            content: "hello".into(),
            trigger_turn: true,
        };

        let seq = mailbox.send(msg);
        assert_eq!(seq, 1);
        assert!(receiver.has_trigger_turn());

        let drained = receiver.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].content, "hello");
    }

    #[test]
    fn test_mailbox_sequence_monotonic() {
        let (mailbox, _) = Mailbox::new();
        assert_eq!(
            mailbox.send(InterAgentMessage {
                author: deepomni_protocol::agent::AgentPath::root(),
                recipient: deepomni_protocol::agent::AgentPath::from_string("/a"),
                other_recipients: vec![],
                content: "1".into(),
                trigger_turn: false,
            }),
            1
        );
        assert_eq!(
            mailbox.send(InterAgentMessage {
                author: deepomni_protocol::agent::AgentPath::root(),
                recipient: deepomni_protocol::agent::AgentPath::from_string("/b"),
                other_recipients: vec![],
                content: "2".into(),
                trigger_turn: false,
            }),
            2
        );
    }
}
