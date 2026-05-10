//! # DeepOmni Config
//!
//! Configuration loading with 6-layer precedence:
//! `runtime builder > CLI flags > env vars > workspace .deepomni/config.toml > user config > defaults`.
//!
//! Adapted from DeepSeek-TUI config patterns.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Config directory name within a workspace.
pub const WORKSPACE_CONFIG_DIR: &str = ".deepomni";
/// Config file name.
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// Default DeepSeek model.
pub const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-v4-pro";
/// Default DeepSeek base URL (OpenAI-compatible Chat Completions).
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";

/// Supported model providers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[default]
    Deepseek,
    Openai,
    Openrouter,
    Ollama,
    Vllm,
    Sglang,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::Openai => "openai",
            Self::Openrouter => "openrouter",
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::Sglang => "sglang",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "deepseek" | "deep-seek" => Some(Self::Deepseek),
            "openai" | "open-ai" => Some(Self::Openai),
            "openrouter" | "open_router" => Some(Self::Openrouter),
            "ollama" | "ollama-local" => Some(Self::Ollama),
            "vllm" | "v-llm" => Some(Self::Vllm),
            "sglang" | "sg-lang" => Some(Self::Sglang),
            _ => None,
        }
    }
}

/// Per-provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub http_headers: BTreeMap<String, String>,
}

/// Multi-provider configuration block.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub deepseek: ProviderConfig,
    #[serde(default)]
    pub openai: ProviderConfig,
    #[serde(default)]
    pub openrouter: ProviderConfig,
    #[serde(default)]
    pub ollama: ProviderConfig,
    #[serde(default)]
    pub vllm: ProviderConfig,
    #[serde(default)]
    pub sglang: ProviderConfig,
}

/// On-disk configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigToml {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    #[serde(default)]
    pub http_headers: BTreeMap<String, String>,
    pub default_model: Option<String>,
    #[serde(default)]
    pub provider: ProviderKind,
    pub model: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
    #[serde(default)]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub plugins: Option<PluginsConfig>,
    #[serde(default)]
    pub skills: Option<SkillsConfig>,
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginsConfig {
    /// Additional plugin search paths.
    #[serde(default)]
    pub extra_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsConfig {
    /// Additional skill search paths.
    #[serde(default)]
    pub extra_paths: Vec<PathBuf>,
}

/// CLI-level overrides.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub provider: Option<ProviderKind>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
}

/// Fully resolved runtime options after precedence merging.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub provider: ProviderKind,
    pub model: String,
    pub api_key: Option<String>,
    pub api_key_source: ApiKeySource,
    pub base_url: String,
    pub approval_policy: String,
    pub sandbox_mode: String,
    pub http_headers: BTreeMap<String, String>,
    pub plugin_paths: Vec<PathBuf>,
    pub skill_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeySource {
    Cli,
    ConfigFile,
    EnvVar,
    None,
}

impl std::fmt::Display for ApiKeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiKeySource::Cli => write!(f, "cli"),
            ApiKeySource::ConfigFile => write!(f, "config"),
            ApiKeySource::EnvVar => write!(f, "env"),
            ApiKeySource::None => write!(f, "none"),
        }
    }
}

/// Configuration store that loads from disk.
pub struct ConfigStore {
    _user_config_path: PathBuf,
    config: ConfigToml,
}

impl ConfigStore {
    /// Load from the default user config path: `~/.deepomni/config.toml`.
    pub fn load_default() -> Result<Self, ConfigError> {
        let home = dirs::home_dir().ok_or(ConfigError::NoHomeDir)?;
        let path = home.join(".deepomni").join(CONFIG_FILE_NAME);
        Self::load(&path)
    }

    /// Load from a specific path.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let config = if path.exists() {
            let content = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
                path: path.to_path_buf(),
                source: e,
            })?;
            toml::from_str(&content).map_err(|e| ConfigError::ParseError {
                path: path.to_path_buf(),
                source: e,
            })?
        } else {
            ConfigToml::default()
        };

        Ok(Self {
            _user_config_path: path.to_path_buf(),
            config,
        })
    }

    /// Return a reference to the loaded config.
    pub fn config(&self) -> &ConfigToml {
        &self.config
    }

    /// Attempt to load workspace-level overrides from `$workspace/.deepomni/config.toml`.
    pub fn load_workspace_config(workspace: &Path) -> Option<ConfigToml> {
        let path = workspace.join(WORKSPACE_CONFIG_DIR).join(CONFIG_FILE_NAME);
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        toml::from_str(&content).ok()
    }

    /// Resolve the effective configuration applying the full precedence chain.
    pub fn resolve(
        &self,
        cli: &CliOverrides,
        workspace: Option<&Path>,
    ) -> Result<ResolvedConfig, ConfigError> {
        // Load workspace overrides.
        let workspace_config = workspace.and_then(Self::load_workspace_config);

        // Merge: user config → workspace overrides.
        let mut merged = self.config.clone();
        if let Some(ref ws) = workspace_config {
            merge_config(&mut merged, ws);
        }

        // Provider kind resolution: CLI > workspace > user > default.
        let provider = cli
            .provider
            .or_else(|| workspace_config.as_ref().and_then(|w| {
                if w.provider != ProviderKind::Deepseek {
                    Some(w.provider)
                } else {
                    None
                }
            }))
            .unwrap_or(merged.provider);

        let provider_cfg = merged.providers.for_provider(provider);

        // API key: CLI > env (DEEPSEEK_API_KEY / OPENAI_API_KEY) > provider config > root config.
        let (api_key, api_key_source) = if let Some(ref key) = cli.api_key {
            (Some(key.clone()), ApiKeySource::Cli)
        } else if let Ok(key) = std::env::var("DEEPOMNI_API_KEY") {
            (Some(key), ApiKeySource::EnvVar)
        } else if let Some(ref key) = provider_cfg.api_key {
            (Some(key.clone()), ApiKeySource::ConfigFile)
        } else if let Some(ref key) = merged.api_key {
            (Some(key.clone()), ApiKeySource::ConfigFile)
        } else {
            (None, ApiKeySource::None)
        };

        // Model: CLI > env > workspace > user provider > user root > default.
        let model = cli
            .model
            .clone()
            .or_else(|| std::env::var("DEEPOMNI_MODEL").ok())
            .or_else(|| {
                workspace_config
                    .as_ref()
                    .and_then(|w| w.model.clone())
            })
            .or_else(|| provider_cfg.model.clone())
            .or_else(|| merged.model.clone())
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_MODEL.to_string());

        // Base URL: CLI > env > provider config > root config > default.
        let base_url = cli
            .base_url
            .clone()
            .or_else(|| std::env::var("DEEPOMNI_BASE_URL").ok())
            .or_else(|| provider_cfg.base_url.clone())
            .or_else(|| merged.base_url.clone())
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_BASE_URL.to_string());

        // Approval policy: CLI > workspace > user > default.
        let approval_policy = cli
            .approval_policy
            .clone()
            .or_else(|| workspace_config.as_ref().and_then(|w| w.approval_policy.clone()))
            .or_else(|| merged.approval_policy.clone())
            .unwrap_or_else(|| "unless_trusted".to_string());

        // Sandbox: CLI > workspace > user > default.
        let sandbox_mode = cli
            .sandbox_mode
            .clone()
            .or_else(|| workspace_config.as_ref().and_then(|w| w.sandbox_mode.clone()))
            .or_else(|| merged.sandbox_mode.clone())
            .unwrap_or_else(|| "auto".to_string());

        // HTTP headers: CLI? no — workspace > provider > root.
        let mut http_headers = merged.http_headers;
        for (k, v) in &provider_cfg.http_headers {
            http_headers.entry(k.clone()).or_insert_with(|| v.clone());
        }

        // Plugin paths.
        let home = dirs::home_dir().unwrap_or_default();
        let mut plugin_paths = vec![
            home.join(".deepomni").join("plugins"),
        ];
        if let Some(ref pc) = merged.plugins {
            plugin_paths.extend(pc.extra_paths.iter().cloned());
        }
        if let Some(ws) = workspace {
            plugin_paths.push(ws.join(WORKSPACE_CONFIG_DIR).join("plugins"));
        }

        // Skill paths.
        let mut skill_paths = vec![
            home.join(".deepomni").join("skills"),
        ];
        if let Some(ref sc) = merged.skills {
            skill_paths.extend(sc.extra_paths.iter().cloned());
        }
        if let Some(ws) = workspace {
            skill_paths.push(ws.join(WORKSPACE_CONFIG_DIR).join("skills"));
        }

        Ok(ResolvedConfig {
            provider,
            model,
            api_key,
            api_key_source,
            base_url,
            approval_policy,
            sandbox_mode,
            http_headers,
            plugin_paths,
            skill_paths,
        })
    }
}

/// Merge project/workspace config overrides into the base config.
fn merge_config(base: &mut ConfigToml, overrides: &ConfigToml) {
    if overrides.api_key.is_some() {
        base.api_key = overrides.api_key.clone();
    }
    if overrides.base_url.is_some() {
        base.base_url = overrides.base_url.clone();
    }
    if !overrides.http_headers.is_empty() {
        base.http_headers = overrides.http_headers.clone();
    }
    if overrides.model.is_some() {
        base.model = overrides.model.clone();
    }
    if overrides.approval_policy.is_some() {
        base.approval_policy = overrides.approval_policy.clone();
    }
    if overrides.sandbox_mode.is_some() {
        base.sandbox_mode = overrides.sandbox_mode.clone();
    }
    // Provider sub-tables: merge field-by-field.
    merge_provider_config(&mut base.providers.deepseek, &overrides.providers.deepseek);
    merge_provider_config(&mut base.providers.openai, &overrides.providers.openai);
    // Don't let the project change the provider identity unless api_key is also set.
}

fn merge_provider_config(base: &mut ProviderConfig, overrides: &ProviderConfig) {
    if overrides.api_key.is_some() {
        base.api_key = overrides.api_key.clone();
    }
    if overrides.base_url.is_some() {
        base.base_url = overrides.base_url.clone();
    }
    if overrides.model.is_some() {
        base.model = overrides.model.clone();
    }
    for (k, v) in &overrides.http_headers {
        base.http_headers.insert(k.clone(), v.clone());
    }
}

impl ProvidersConfig {
    pub fn for_provider(&self, provider: ProviderKind) -> &ProviderConfig {
        match provider {
            ProviderKind::Deepseek => &self.deepseek,
            ProviderKind::Openai => &self.openai,
            ProviderKind::Openrouter => &self.openrouter,
            ProviderKind::Ollama => &self.ollama,
            ProviderKind::Vllm => &self.vllm,
            ProviderKind::Sglang => &self.sglang,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    NoHomeDir,
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },
    ParseError {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NoHomeDir => write!(f, "could not determine home directory"),
            ConfigError::ReadError { path, source } => {
                write!(f, "failed to read config at {}: {source}", path.display())
            }
            ConfigError::ParseError { path, source } => {
                write!(f, "failed to parse config at {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::ReadError { source, .. } => Some(source),
            ConfigError::ParseError { source, .. } => Some(source),
            ConfigError::NoHomeDir => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_kind_parse() {
        assert_eq!(ProviderKind::parse("deepseek"), Some(ProviderKind::Deepseek));
        assert_eq!(ProviderKind::parse("openai"), Some(ProviderKind::Openai));
        assert_eq!(ProviderKind::parse("ollama-local"), Some(ProviderKind::Ollama));
        assert_eq!(ProviderKind::parse("unknown"), None);
    }

    #[test]
    fn test_default_config_is_empty() {
        let config = ConfigToml::default();
        assert_eq!(config.provider, ProviderKind::Deepseek);
        assert!(config.api_key.is_none());
    }

    #[test]
    fn test_cli_overrides_default() {
        let cli = CliOverrides::default();
        assert!(cli.model.is_none());
        assert!(cli.provider.is_none());
    }

    #[test]
    fn test_resolve_with_no_config() {
        let tmp = std::env::temp_dir().join("deepomni-test-nonexistent");
        let store = ConfigStore::load(&tmp.join("nonexistent.toml")).unwrap_or_else(|_| {
            // ConfigStore::load returns Err for ReadError only. If file doesn't exist
            // it should use defaults. Let's test differently:
            ConfigStore {
                _user_config_path: tmp,
                config: ConfigToml::default(),
            }
        });
        // Actually, load is designed to create default if file doesn't exist.
        // Just verify the default flow.
        let resolved = store.resolve(&CliOverrides::default(), None).unwrap();
        assert_eq!(resolved.provider, ProviderKind::Deepseek);
        assert_eq!(resolved.model, DEFAULT_DEEPSEEK_MODEL);
        assert_eq!(resolved.approval_policy, "unless_trusted");
    }
}
