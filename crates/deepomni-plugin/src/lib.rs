//! # DeepOmni Plugin
//!
//! Plugin manifest, local discovery, enable/disable state, capability
//! summaries, and permission declarations.
//!
//! PRD §7.15. Based on Codex core-plugins manifest shape.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::info;

/// The on-disk plugin manifest, loaded from `plugin.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin identifier (e.g., "deepomni.github-review").
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Semantic version.
    pub version: Option<String>,
    /// Short description.
    pub description: Option<String>,
    /// Path to skills directory (relative to plugin root).
    #[serde(default)]
    pub skills: Option<String>,
    /// Path to tools definition (relative to plugin root).
    #[serde(default)]
    pub tools: Option<String>,
    /// Path to MCP server definitions (relative to plugin root).
    #[serde(default)]
    pub mcp_servers: Option<String>,
    /// Path to hooks definitions (relative to plugin root).
    #[serde(default)]
    pub hooks: Option<String>,
    /// Path to slash commands directory.
    #[serde(default)]
    pub commands: Option<String>,
    /// Required permissions.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Runtime configuration.
    #[serde(default)]
    pub runtime: Option<PluginRuntime>,
    /// Interface metadata for UI rendering.
    #[serde(default)]
    pub interface: Option<PluginInterface>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRuntime {
    #[serde(rename = "type")]
    pub runtime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInterface {
    pub display_name: Option<String>,
    pub capabilities: Option<Vec<String>>,
}

/// Load result for a plugin.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum PluginLoadOutcome {
    /// Plugin loaded successfully.
    Loaded(LoadedPlugin),
    /// Invalid manifest.
    InvalidManifest { path: PathBuf, error: String },
    /// Plugin root doesn't contain a valid plugin.json.
    NotFound(PathBuf),
}

/// A successfully loaded and validated plugin.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub root: PathBuf,
    pub enabled: bool,
    /// Absolute paths resolved from manifest.
    pub resolved_paths: ResolvedPluginPaths,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedPluginPaths {
    pub skills: Option<PathBuf>,
    pub tools: Option<PathBuf>,
    pub mcp_servers: Option<PathBuf>,
    pub hooks: Option<PathBuf>,
    pub commands: Option<PathBuf>,
}

/// Capability summary for introspection APIs.
#[derive(Debug, Clone, Serialize)]
pub struct PluginCapabilitySummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub enabled: bool,
    pub has_skills: bool,
    pub has_mcp_servers: bool,
    pub has_hooks: bool,
    pub permissions: Vec<String>,
    pub capabilities: Vec<String>,
}

/// Manages plugin discovery, enable/disable state, and capability loading.
pub struct PluginManager {
    /// Directories to scan for plugins.
    search_paths: Vec<PathBuf>,
    /// Loaded plugins, keyed by plugin ID.
    plugins: HashMap<String, LoadedPlugin>,
    /// Current capability snapshot.
    snapshot: Vec<PluginCapabilitySummary>,
}

impl PluginManager {
    pub fn new(search_paths: Vec<PathBuf>) -> Self {
        Self {
            search_paths,
            plugins: HashMap::new(),
            snapshot: Vec::new(),
        }
    }

    /// Discover and load all plugins from configured search paths.
    pub fn discover(&mut self) -> Vec<PluginLoadOutcome> {
        let mut outcomes = Vec::new();

        for search_path in &self.search_paths.clone() {
            if !search_path.exists() || !search_path.is_dir() {
                continue;
            }

            let entries = match fs::read_dir(search_path) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let plugin_dir = entry.path();
                if !plugin_dir.is_dir() {
                    continue;
                }

                let manifest_path = plugin_dir.join("plugin.json");
                if !manifest_path.exists() {
                    continue;
                }

                match self.load_plugin(&manifest_path, &plugin_dir) {
                    PluginLoadOutcome::Loaded(plugin) => {
                        let plugin_id = plugin.manifest.id.clone();
                        info!(%plugin_id, "plugin loaded");
                        let summary = self.build_summary(&plugin);
                        self.snapshot.push(summary);
                        self.plugins.insert(plugin_id.clone(), plugin);
                        outcomes.push(PluginLoadOutcome::Loaded(self.plugins[&plugin_id].clone()));
                    }
                    other => outcomes.push(other),
                }
            }
        }

        outcomes
    }

    fn load_plugin(&mut self, manifest_path: &Path, plugin_dir: &Path) -> PluginLoadOutcome {
        let raw = match fs::read_to_string(manifest_path) {
            Ok(r) => r,
            Err(e) => {
                return PluginLoadOutcome::InvalidManifest {
                    path: manifest_path.to_path_buf(),
                    error: e.to_string(),
                };
            }
        };

        let manifest: PluginManifest = match serde_json::from_str(&raw) {
            Ok(m) => m,
            Err(e) => {
                return PluginLoadOutcome::InvalidManifest {
                    path: manifest_path.to_path_buf(),
                    error: e.to_string(),
                };
            }
        };

        // Resolve relative paths.
        let resolved = ResolvedPluginPaths {
            skills: manifest.skills.as_ref().map(|p| plugin_dir.join(p)),
            tools: manifest.tools.as_ref().map(|p| plugin_dir.join(p)),
            mcp_servers: manifest.mcp_servers.as_ref().map(|p| plugin_dir.join(p)),
            hooks: manifest.hooks.as_ref().map(|p| plugin_dir.join(p)),
            commands: manifest.commands.as_ref().map(|p| plugin_dir.join(p)),
        };

        PluginLoadOutcome::Loaded(LoadedPlugin {
            manifest,
            root: plugin_dir.to_path_buf(),
            enabled: true,
            resolved_paths: resolved,
        })
    }

    fn build_summary(&self, plugin: &LoadedPlugin) -> PluginCapabilitySummary {
        PluginCapabilitySummary {
            id: plugin.manifest.id.clone(),
            name: plugin.manifest.name.clone(),
            description: plugin.manifest.description.clone(),
            version: plugin.manifest.version.clone(),
            enabled: plugin.enabled,
            has_skills: plugin.resolved_paths.skills.is_some(),
            has_mcp_servers: plugin.resolved_paths.mcp_servers.is_some(),
            has_hooks: plugin.resolved_paths.hooks.is_some(),
            permissions: plugin.manifest.permissions.clone(),
            capabilities: plugin
                .manifest
                .interface
                .as_ref()
                .and_then(|i| i.capabilities.clone())
                .unwrap_or_default(),
        }
    }

    /// Enable a plugin by ID.
    pub fn enable(&mut self, plugin_id: &str) -> bool {
        if let Some(plugin) = self.plugins.get_mut(plugin_id) {
            plugin.enabled = true;
            self.refresh_snapshot();
            true
        } else {
            false
        }
    }

    /// Disable a plugin by ID.
    pub fn disable(&mut self, plugin_id: &str) -> bool {
        if let Some(plugin) = self.plugins.get_mut(plugin_id) {
            plugin.enabled = false;
            self.refresh_snapshot();
            true
        } else {
            false
        }
    }

    fn refresh_snapshot(&mut self) {
        self.snapshot = self
            .plugins
            .values()
            .map(|p| self.build_summary(p))
            .collect();
    }

    /// Get a loaded plugin by ID.
    pub fn get(&self, plugin_id: &str) -> Option<&LoadedPlugin> {
        self.plugins.get(plugin_id)
    }

    /// List all plugin capability summaries.
    pub fn list(&self) -> &[PluginCapabilitySummary] {
        &self.snapshot
    }

    /// List enabled plugins.
    pub fn list_enabled(&self) -> Vec<&LoadedPlugin> {
        self.plugins.values().filter(|p| p.enabled).collect()
    }

    /// Get paths for active plugin skills.
    pub fn active_skill_paths(&self) -> Vec<PathBuf> {
        self.plugins
            .values()
            .filter(|p| p.enabled)
            .filter_map(|p| p.resolved_paths.skills.clone())
            .collect()
    }

    /// Get active plugin MCP server config paths.
    pub fn active_mcp_paths(&self) -> Vec<PathBuf> {
        self.plugins
            .values()
            .filter(|p| p.enabled)
            .filter_map(|p| p.resolved_paths.mcp_servers.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_plugin_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("plugin-test-{}", uuid::Uuid::new_v4()));
        let plugin_dir = dir.join("test-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();

        let manifest = serde_json::json!({
            "id": "deepomni.test",
            "name": "Test Plugin",
            "version": "0.1.0",
            "description": "A test plugin",
            "skills": "./skills",
            "permissions": ["filesystem.read"],
            "interface": {
                "displayName": "Test",
                "capabilities": ["testing"]
            }
        });

        fs::write(
            plugin_dir.join("plugin.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        dir
    }

    #[test]
    fn test_discover_plugin() {
        let dir = setup_plugin_dir();
        let mut manager = PluginManager::new(vec![dir]);
        let outcomes = manager.discover();
        assert_eq!(outcomes.len(), 1);

        let plugin = manager.get("deepomni.test").unwrap();
        assert_eq!(plugin.manifest.name, "Test Plugin");
        assert!(plugin.enabled);
        assert!(plugin.resolved_paths.skills.is_some());
    }

    #[test]
    fn test_enable_disable() {
        let dir = setup_plugin_dir();
        let mut manager = PluginManager::new(vec![dir]);
        manager.discover();

        assert!(manager.disable("deepomni.test"));
        assert!(!manager.get("deepomni.test").unwrap().enabled);

        assert!(manager.enable("deepomni.test"));
        assert!(manager.get("deepomni.test").unwrap().enabled);

        assert!(!manager.enable("nonexistent"));
    }

    #[test]
    fn test_capability_summary() {
        let dir = setup_plugin_dir();
        let mut manager = PluginManager::new(vec![dir]);
        manager.discover();

        let summaries = manager.list();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "deepomni.test");
        assert!(summaries[0].has_skills);
        assert!(!summaries[0].has_mcp_servers);
    }

    #[test]
    fn test_manifest_validation() {
        let dir = std::env::temp_dir().join(format!("bad-plugin-{}", uuid::Uuid::new_v4()));
        let plugin_dir = dir.join("bad");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("plugin.json"), "not json").unwrap();

        let outcomes = PluginManager::new(vec![dir]).discover();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0],
            PluginLoadOutcome::InvalidManifest { .. }
        ));
    }
}
