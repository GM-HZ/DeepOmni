//! # DeepOmni Skills
//!
//! Skill discovery, loading, rendering, and selection.
//! Skills are prompt-time behavior modules loaded from global, workspace,
//! and plugin directories. They are not tools — they guide how the agent
//! uses tools.
//!
//! PRD §7.14

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A loaded skill, ready for context injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Unique skill identifier (namespace/name).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Short description for capability summaries.
    pub description: String,
    /// The skill's instruction content (from SKILL.md body).
    pub content: String,
    /// Source directory for asset/script resolution.
    pub root: PathBuf,
    /// Whether this skill came from a plugin.
    pub plugin_id: Option<String>,
    /// Estimated token count for budget allocation.
    pub estimated_tokens: u64,
}

/// Frontmatter parsed from SKILL.md.
#[derive(Debug, Default, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

/// Discovered skill directories.
#[derive(Debug, Clone)]
pub struct SkillRoots {
    pub roots: Vec<PathBuf>,
}

impl SkillRoots {
    pub fn new() -> Self {
        Self { roots: Vec::new() }
    }

    pub fn add_root(&mut self, path: PathBuf) {
        if path.exists() {
            self.roots.push(path);
        }
    }
}

impl Default for SkillRoots {
    fn default() -> Self {
        Self::new()
    }
}

/// Manages skill discovery, loading, and rendering.
pub struct SkillManager {
    roots: SkillRoots,
    skills: HashMap<String, Skill>,
}

impl SkillManager {
    pub fn new(roots: SkillRoots) -> Self {
        Self {
            roots,
            skills: HashMap::new(),
        }
    }

    /// Discover and load all skills from configured roots.
    pub fn discover(&mut self) -> Result<usize, SkillError> {
        self.skills.clear();
        let mut count = 0;
        let roots = self.roots.roots.clone();

        for root in &roots {
            count += self.discover_from_dir(root, None)?;
        }

        // Prioritize: workspace skills override global, plugin skills are namespaced.
        Ok(count)
    }

    /// Discover skills from a plugin directory, namespaced under plugin-id.
    pub fn discover_plugin_skills(
        &mut self,
        plugin_root: &Path,
        plugin_id: &str,
    ) -> Result<usize, SkillError> {
        self.discover_from_dir(plugin_root, Some(plugin_id))
    }

    fn discover_from_dir(
        &mut self,
        dir: &Path,
        plugin_id: Option<&str>,
    ) -> Result<usize, SkillError> {
        if !dir.exists() || !dir.is_dir() {
            return Ok(0);
        }

        let mut count = 0;
        let entries = fs::read_dir(dir).map_err(|e| SkillError::ReadError {
            path: dir.to_path_buf(),
            source: e,
        })?;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let skill_md = path.join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }

            let skill = self.load_skill_from_dir(&path, &skill_md, plugin_id)?;
            let id = skill.id.clone();
            self.skills.insert(id, skill);
            count += 1;
        }

        Ok(count)
    }

    fn load_skill_from_dir(
        &self,
        dir: &Path,
        skill_md: &Path,
        plugin_id: Option<&str>,
    ) -> Result<Skill, SkillError> {
        let raw = fs::read_to_string(skill_md).map_err(|e| SkillError::ReadError {
            path: skill_md.to_path_buf(),
            source: e,
        })?;

        // Parse optional frontmatter (YAML-style between --- delimiters).
        let (meta, body) = if let Some(rest) = raw.strip_prefix("---") {
            if let Some(end) = rest.find("---") {
                let front = &rest[..end];
                let body = &rest[end + 3..];
                let meta = parse_frontmatter(front);
                (meta, body.trim().to_string())
            } else {
                (SkillFrontmatter::default(), raw)
            }
        } else {
            (SkillFrontmatter::default(), raw)
        };

        let dir_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let name = meta
            .name
            .unwrap_or_else(|| dir_name.replace(['-', '_'], " "));
        let description = meta.description.unwrap_or_else(|| format!("Skill: {name}"));

        let id = if let Some(pid) = plugin_id {
            format!("{pid}/{dir_name}")
        } else {
            dir_name.clone()
        };

        let estimated_tokens = (body.len() as u64).div_ceil(4);

        Ok(Skill {
            id,
            name,
            description,
            content: body,
            root: dir.to_path_buf(),
            plugin_id: plugin_id.map(|s| s.to_string()),
            estimated_tokens,
        })
    }

    /// Get a skill by its ID.
    pub fn get(&self, id: &str) -> Option<&Skill> {
        self.skills.get(id)
    }

    /// List all discovered skills.
    pub fn list(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// List skills contributed by a specific plugin.
    pub fn list_for_plugin(&self, plugin_id: &str) -> Vec<&Skill> {
        self.skills
            .values()
            .filter(|s| s.plugin_id.as_deref() == Some(plugin_id))
            .collect()
    }

    /// Remove all skills from a plugin (on disable).
    pub fn remove_plugin_skills(&mut self, plugin_id: &str) -> usize {
        let before = self.skills.len();
        self.skills
            .retain(|_, s| s.plugin_id.as_deref() != Some(plugin_id));
        before - self.skills.len()
    }

    /// Render active skills into context fragments for injection.
    pub fn render_for_context(&self, budget_tokens: u64) -> String {
        let mut parts = Vec::new();
        let mut used = 0u64;

        for skill in self.skills.values() {
            if used + skill.estimated_tokens > budget_tokens {
                break;
            }
            parts.push(format!("## Skill: {}\n{}\n", skill.name, skill.content));
            used += skill.estimated_tokens;
        }

        parts.join("\n")
    }

    /// Find a skill mentioned by name in user input.
    pub fn find_by_mention(&self, text: &str) -> Option<&Skill> {
        for skill in self.skills.values() {
            if text.contains(&format!("@{}", skill.id)) {
                return Some(skill);
            }
            if text.to_lowercase().contains(&skill.name.to_lowercase()) {
                return Some(skill);
            }
        }
        None
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

#[derive(Debug)]
pub enum SkillError {
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },
    ParseError {
        path: PathBuf,
        message: String,
    },
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillError::ReadError { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            SkillError::ParseError { path, message } => {
                write!(f, "failed to parse {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for SkillError {}

fn parse_frontmatter(text: &str) -> SkillFrontmatter {
    let mut meta = SkillFrontmatter::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches('"').trim();
            match key.trim() {
                "name" => meta.name = Some(value.to_string()),
                "description" => meta.description = Some(value.to_string()),
                _ => {}
            }
        }
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_skill_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skill-test-{}", uuid::Uuid::new_v4()));
        let skill_dir = dir.join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: My Test Skill\ndescription: A test skill\n---\n\nThis is the skill content.\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn test_discover_skills() {
        let dir = setup_skill_dir();
        let mut roots = SkillRoots::new();
        roots.add_root(dir.clone());

        let mut manager = SkillManager::new(roots);
        let count = manager.discover().unwrap();
        assert_eq!(count, 1);

        let skill = manager.get("my-skill").unwrap();
        assert_eq!(skill.name, "My Test Skill");
        assert_eq!(skill.description, "A test skill");
        assert!(skill.content.contains("This is the skill content"));
    }

    #[test]
    fn test_plugin_skills_namespaced() {
        let dir = setup_skill_dir();
        let mut manager = SkillManager::new(SkillRoots::new());
        let count = manager.discover_plugin_skills(&dir, "my-plugin").unwrap();
        assert_eq!(count, 1);

        let skill = manager.get("my-plugin/my-skill").unwrap();
        assert_eq!(skill.plugin_id.as_deref(), Some("my-plugin"));
    }

    #[test]
    fn test_render_for_context() {
        let dir = setup_skill_dir();
        let mut roots = SkillRoots::new();
        roots.add_root(dir);
        let mut manager = SkillManager::new(roots);
        manager.discover().unwrap();

        let rendered = manager.render_for_context(1000);
        assert!(rendered.contains("My Test Skill"));
        assert!(rendered.contains("This is the skill content"));
    }

    #[test]
    fn test_remove_plugin_skills() {
        let dir = setup_skill_dir();
        let mut manager = SkillManager::new(SkillRoots::new());
        manager.discover_plugin_skills(&dir, "my-plugin").unwrap();
        assert_eq!(manager.len(), 1);

        let removed = manager.remove_plugin_skills("my-plugin");
        assert_eq!(removed, 1);
        assert!(manager.is_empty());
    }
}
