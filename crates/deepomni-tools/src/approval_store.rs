//! Per-session approval cache. Caches allow/deny decisions so repeated
//! tool calls within the same session don't re-prompt. Plan 2 Task 1.

use serde::Serialize;
use std::collections::HashMap;

/// Cached approval decision for a tool+arguments key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

/// Session-local approval cache. Decisions are keyed by a serializable
/// representation of (tool_name, arguments).
#[derive(Debug, Default)]
pub struct ApprovalStore {
    cache: std::sync::Mutex<HashMap<String, ApprovalDecision>>,
}

impl ApprovalStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get<K: Serialize>(&self, key: &K) -> Option<ApprovalDecision> {
        let key = approval_cache_key(key).ok()?;
        self.cache.lock().ok()?.get(&key).copied()
    }

    pub fn put<K: Serialize>(&self, key: K, decision: ApprovalDecision) {
        if let Ok(key) = approval_cache_key(&key)
            && let Ok(mut cache) = self.cache.lock()
        {
            cache.insert(key, decision);
        }
    }

    pub async fn with_cached_approval<K, F, Fut>(&self, keys: Vec<K>, fetch: F) -> ApprovalDecision
    where
        K: Serialize,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ApprovalDecision>,
    {
        for key in &keys {
            if let Some(decision) = self.get(key) {
                return decision;
            }
        }

        let decision = fetch().await;
        for key in keys {
            self.put(key, decision);
        }
        decision
    }

    pub fn len(&self) -> usize {
        self.cache.lock().map(|cache| cache.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn approval_cache_key<K: Serialize>(key: &K) -> Result<String, serde_json::Error> {
    serde_json::to_string(key)
}
