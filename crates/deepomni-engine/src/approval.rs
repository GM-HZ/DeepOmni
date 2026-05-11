use serde::Serialize;

use deepomni_tools::{ApprovalDecision, ApprovalStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approved,
    Rejected,
}

pub struct ApprovalCoordinator {
    store: ApprovalStore,
}

impl ApprovalCoordinator {
    pub fn new() -> Self {
        Self {
            store: ApprovalStore::new(),
        }
    }

    pub fn remember<K: Serialize>(&self, key: K, outcome: ApprovalOutcome) {
        self.store.put(
            key,
            match outcome {
                ApprovalOutcome::Approved => ApprovalDecision::Approved,
                ApprovalOutcome::Rejected => ApprovalDecision::Rejected,
            },
        );
    }

    pub fn cached<K: Serialize>(&self, key: &K) -> Option<ApprovalOutcome> {
        self.store.get(key).map(|decision| match decision {
            ApprovalDecision::Approved => ApprovalOutcome::Approved,
            ApprovalDecision::Rejected => ApprovalOutcome::Rejected,
        })
    }
}

impl Default for ApprovalCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinator_round_trips_cached_outcome() {
        let coordinator = ApprovalCoordinator::new();
        let key = serde_json::json!({"tool": "write", "path": "README.md"});
        coordinator.remember(key.clone(), ApprovalOutcome::Approved);
        assert_eq!(coordinator.cached(&key), Some(ApprovalOutcome::Approved));
    }
}

