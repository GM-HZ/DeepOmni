//! Token usage and cost tracking types.

use serde::{Deserialize, Serialize};

/// Token usage for a turn or request.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.total_tokens
            .unwrap_or(self.prompt_tokens + self.completion_tokens)
    }
}

/// Cost estimate for a turn or request. Best-effort, may be unknown.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CostInfo {
    /// Total cost in USD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// True if the cost is known/concrete.
    #[serde(default)]
    pub is_estimated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_total() {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            ..Default::default()
        };
        assert_eq!(usage.total(), 150);
    }

    #[test]
    fn test_usage_with_reasoning() {
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 200,
            reasoning_tokens: Some(500),
            cached_tokens: Some(300),
            total_tokens: Some(1700),
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("reasoning_tokens"));
        assert!(json.contains("500"));
    }

    #[test]
    fn test_cost_info_unknown() {
        let cost = CostInfo::default();
        let json = serde_json::to_string(&cost).unwrap();
        // Unknown costs don't emit cost_usd field
        assert!(!json.contains("cost_usd"));
    }
}
