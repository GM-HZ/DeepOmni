//! # DeepOmni Prompt
//!
//! Five-layer system prompt builder. Follows the Claude Code priority model:
//! override → agent → custom → default → append.
//! Includes SHA-256 content hashing for cache boundary detection.

use sha2::{Digest, Sha256};

/// Result of prompt assembly with cache metadata.
pub struct BuiltPrompt {
    pub content: String,
    pub static_hash: String,
    pub cacheable: bool,
}

/// Builds the system prompt with five-layer priority.
pub struct PromptBuilder {
    pub override_prompt: Option<String>,
    pub agent_prompt: Option<String>,
    pub custom_prompt: Option<String>,
    pub default_prompt: String,
    pub append_prompt: Option<String>,
}

impl PromptBuilder {
    pub fn new(default_prompt: String) -> Self {
        Self {
            override_prompt: None,
            agent_prompt: None,
            custom_prompt: None,
            default_prompt,
            append_prompt: None,
        }
    }

    pub fn build(&self) -> BuiltPrompt {
        if let Some(ref ov) = self.override_prompt {
            let hash = hash_content(ov);
            return BuiltPrompt {
                content: ov.clone(),
                static_hash: hash,
                cacheable: true,
            };
        }

        let mut parts: Vec<&str> = Vec::new();

        if let Some(ref agent) = self.agent_prompt {
            parts.push(agent.as_str());
        } else if let Some(ref custom) = self.custom_prompt {
            parts.push(custom.as_str());
        } else {
            parts.push(&self.default_prompt);
        }

        if let Some(ref append) = self.append_prompt {
            parts.push(append.as_str());
        }

        let content = parts.join("\n\n");
        let hash = hash_content(&content);
        BuiltPrompt {
            content,
            static_hash: hash,
            cacheable: true,
        }
    }

    pub fn with_override(mut self, prompt: String) -> Self {
        self.override_prompt = Some(prompt);
        self
    }

    pub fn with_agent(mut self, prompt: String) -> Self {
        self.agent_prompt = Some(prompt);
        self
    }

    pub fn with_custom(mut self, prompt: String) -> Self {
        self.custom_prompt = Some(prompt);
        self
    }

    pub fn with_append(mut self, prompt: String) -> Self {
        self.append_prompt = Some(prompt);
        self
    }
}

fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_prompt() {
        let built = PromptBuilder::new("You are DeepOmni.".into()).build();
        assert_eq!(built.content, "You are DeepOmni.");
        assert!(!built.static_hash.is_empty());
    }

    #[test]
    fn test_override_replaces_all() {
        let built = PromptBuilder::new("default".into())
            .with_override("override".into())
            .with_custom("custom".into())
            .with_append("append".into())
            .build();
        assert_eq!(built.content, "override");
    }

    #[test]
    fn test_hash_stability() {
        let a = PromptBuilder::new("test".into()).build();
        let b = PromptBuilder::new("test".into()).build();
        assert_eq!(a.static_hash, b.static_hash);
    }
}
