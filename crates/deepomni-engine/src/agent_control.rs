//! AgentRegistry and AgentControl — multi-agent lifecycle management.
//! Pattern from Codex: registry tracks active agents, control spawns new ones.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use deepomni_protocol::agent::{AgentInfo, AgentPath};

/// Tracks active sub-agents for a session tree.
#[derive(Default)]
pub struct AgentRegistry {
    agents: Mutex<HashMap<String, AgentInfo>>,
    total_count: AtomicUsize,
}

impl AgentRegistry {
    /// Reserve a spawn slot. Fails if over limit.
    pub fn reserve_slot(&self, max: usize) -> Result<(), AgentSpawnError> {
        let current = self.total_count.fetch_add(1, Ordering::SeqCst);
        if current >= max {
            self.total_count.fetch_sub(1, Ordering::SeqCst);
            return Err(AgentSpawnError::LimitReached { max });
        }
        Ok(())
    }

    /// Register a spawned agent.
    pub fn register(&self, info: AgentInfo) {
        self.agents
            .lock()
            .unwrap()
            .insert(info.agent_path.to_string(), info);
    }

    /// Remove an agent on completion.
    pub fn remove(&self, path: &AgentPath) {
        self.agents.lock().unwrap().remove(path.as_str());
        self.total_count.fetch_sub(1, Ordering::SeqCst);
    }

    /// List all active agents.
    pub fn list(&self) -> Vec<AgentInfo> {
        self.agents.lock().unwrap().values().cloned().collect()
    }

    /// Allocate a unique agent path under the parent.
    pub fn allocate_path(&self, parent: &AgentPath) -> AgentPath {
        let count = self.agents.lock().unwrap().len();
        AgentPath::from_string(format!("{}/agent-{}", parent.as_str(), count + 1))
    }
}

#[derive(Debug)]
pub enum AgentSpawnError {
    LimitReached { max: usize },
}

impl std::fmt::Display for AgentSpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentSpawnError::LimitReached { max } => write!(f, "agent limit reached (max: {max})"),
        }
    }
}

impl std::error::Error for AgentSpawnError {}

/// Coordinates multi-agent operations: spawn, track, communicate.
pub struct AgentControl {
    pub registry: Arc<AgentRegistry>,
    pub max_depth: u32,
    pub max_total: usize,
}

impl AgentControl {
    pub fn new(max_total: usize, max_depth: u32) -> Self {
        Self {
            registry: Arc::new(AgentRegistry::default()),
            max_depth,
            max_total,
        }
    }

    /// Check if spawning is allowed at the given depth.
    pub fn can_spawn(&self, depth: u32) -> bool {
        depth < self.max_depth
    }
}

impl Default for AgentControl {
    fn default() -> Self {
        Self::new(10, 5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_reserve_slot() {
        let reg = AgentRegistry::default();
        assert!(reg.reserve_slot(3).is_ok());
        assert!(reg.reserve_slot(3).is_ok());
        assert!(reg.reserve_slot(3).is_ok());
        assert!(reg.reserve_slot(3).is_err()); // 4th fails
    }

    #[test]
    fn test_registry_allocate_path() {
        let reg = AgentRegistry::default();
        let path = reg.allocate_path(&AgentPath::root());
        assert!(path.as_str().starts_with("/root/agent-"));
    }

    #[test]
    fn test_agent_control_depth() {
        let ctrl = AgentControl::new(10, 3);
        assert!(ctrl.can_spawn(0));
        assert!(ctrl.can_spawn(2));
        assert!(!ctrl.can_spawn(3)); // at max_depth, can't spawn deeper
    }
}
