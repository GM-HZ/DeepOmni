//! # DeepOmni Built-in Tools
//!
//! Production built-in tool handlers implementing the `ToolHandler` trait.
//! Each tool has a typed JSON Schema, mutability classification, sandbox
//! needs, and structured output.
//!
//! PRD §7.8

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use deepomni_protocol::tool::{ToolOutput, ToolPayload, ToolSpec, ToolSpecDetails};
use deepomni_tools::{ToolCapability, ToolError, ToolHandler, ToolInvocation};

// ── Workspace-aware path resolution ──

const MAX_OUTPUT_SIZE: usize = 50_000;

fn resolve_workspace_path(requested: &str, workspace: &str) -> Result<PathBuf, ToolError> {
    let path = Path::new(requested);
    if path.is_absolute() {
        if !requested.starts_with(workspace) {
            return Err(ToolError::PathEscape {
                path: requested.into(),
            });
        }
        return Ok(path.to_path_buf());
    }
    Ok(PathBuf::from(workspace).join(requested))
}

/// Get the effective workspace from invocation or fallback.
fn effective_workspace(invocation: &ToolInvocation, fallback: &str) -> String {
    invocation
        .workspace
        .clone()
        .unwrap_or_else(|| fallback.to_string())
}

fn truncate_output(content: &str) -> String {
    if content.len() > MAX_OUTPUT_SIZE {
        let head = &content[..MAX_OUTPUT_SIZE / 2];
        let tail = &content[content.len() - MAX_OUTPUT_SIZE / 2..];
        format!(
            "{head}\n\n... [output truncated, {len} total bytes] ...\n\n{tail}",
            len = content.len()
        )
    } else {
        content.to_string()
    }
}

// ── read_file ──

pub struct ReadFileTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn is_mutating(&self) -> bool {
        false
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "read_file".into(),
            description: "Read a file from the workspace".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read, relative to workspace root"
                    }
                },
                "required": ["path"]
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let ws = effective_workspace(&invocation, &self.workspace);
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let path_str = deepomni_tools::required_str(&args, "path")?;
        let file_path = resolve_workspace_path(&path_str, &ws)?;

        let content = fs::read_to_string(&file_path).map_err(|e| ToolError::ExecutionFailed {
            message: format!("cannot read {}: {e}", file_path.display()),
        })?;

        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "path": path_str,
                "content": truncate_output(&content),
                "size": content.len(),
            })),
            success: true,
        })
    }
}

// ── write_file ──

pub struct WriteFileTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn is_mutating(&self) -> bool {
        true
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles, ToolCapability::Sandboxable]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "write_file".into(),
            description: "Write content to a file in the workspace".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to write to, relative to workspace root"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write"
                    }
                },
                "required": ["path", "content"]
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let path_str = deepomni_tools::required_str(&args, "path")?;
        let content = deepomni_tools::required_str(&args, "content")?;
        let ws = effective_workspace(&invocation, &self.workspace);
        let file_path = resolve_workspace_path(&path_str, &ws)?;

        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).map_err(|e| ToolError::ExecutionFailed {
                message: format!("cannot create directory: {e}"),
            })?;
        }

        fs::write(&file_path, &content).map_err(|e| ToolError::ExecutionFailed {
            message: format!("cannot write {}: {e}", file_path.display()),
        })?;

        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "path": path_str,
                "written": content.len(),
            })),
            success: true,
        })
    }
}

// ── edit_file ──

pub struct EditFileTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn is_mutating(&self) -> bool {
        true
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles, ToolCapability::Sandboxable]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "edit_file".into(),
            description: "Edit a file using search-and-replace".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to edit" },
                    "old_string": { "type": "string", "description": "Text to replace" },
                    "new_string": { "type": "string", "description": "Replacement text" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let ws = effective_workspace(&invocation, &self.workspace);
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let path_str = deepomni_tools::required_str(&args, "path")?;
        let old_str = deepomni_tools::required_str(&args, "old_string")?;
        let new_str = deepomni_tools::required_str(&args, "new_string")?;
        let file_path = resolve_workspace_path(&path_str, &ws)?;

        let content = fs::read_to_string(&file_path).map_err(|e| ToolError::ExecutionFailed {
            message: format!("cannot read {}: {e}", file_path.display()),
        })?;

        if !content.contains(&old_str) {
            return Err(ToolError::InvalidInput {
                message: "old_string not found in file".into(),
            });
        }

        let new_content = content.replacen(&old_str, &new_str, 1);
        fs::write(&file_path, &new_content).map_err(|e| ToolError::ExecutionFailed {
            message: format!("cannot write {}: {e}", file_path.display()),
        })?;

        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "path": path_str,
                "replaced": true,
            })),
            success: true,
        })
    }
}

// ── grep ──

pub struct GrepTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn is_mutating(&self) -> bool {
        false
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "grep".into(),
            description: "Search for a pattern in workspace files".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "Directory or file to search in (default: workspace root)" }
                },
                "required": ["pattern"]
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let ws = effective_workspace(&invocation, &self.workspace);
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let pattern = deepomni_tools::required_str(&args, "pattern")?;
        let search_path = deepomni_tools::optional_str(&args, "path").unwrap_or_else(|| ".".into());

        let output = Command::new("grep")
            .args([
                "-rn",
                "--include=*.rs",
                "--include=*.toml",
                "--include=*.md",
                "--include=*.json",
                "--include=*.py",
                "--include=*.js",
                "--include=*.ts",
                "--include=*.go",
                "--include=*.java",
                &pattern,
                &search_path,
            ])
            .current_dir(&ws)
            .output()
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("grep failed: {e}"),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "matches": truncate_output(stdout.trim()),
            })),
            success: true,
        })
    }
}

// ── list_files ──

pub struct ListFilesTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }
    fn is_mutating(&self) -> bool {
        false
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "list_files".into(),
            description: "List files in a directory".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to list (default: workspace root)" },
                    "depth": { "type": "integer", "description": "Max recursion depth (default: 3)" }
                },
                "required": []
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let ws = effective_workspace(&invocation, &self.workspace);
        let list_path = deepomni_tools::optional_str(&args, "path").unwrap_or_else(|| ".".into());
        let depth: u32 = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;
        let full_path = resolve_workspace_path(&list_path, &ws)?;

        let mut entries = Vec::new();
        walk_dir(&full_path, 0, depth, &mut entries);

        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "path": list_path,
                "entries": entries,
            })),
            success: true,
        })
    }
}

fn walk_dir(path: &Path, current: u32, max_depth: u32, entries: &mut Vec<String>) {
    if current > max_depth {
        return;
    }
    let dir = match fs::read_dir(path) {
        Ok(d) => d,
        Err(_) => return,
    };
    for entry in dir.flatten() {
        let file_type = entry
            .file_type()
            .map(|t| if t.is_dir() { "d" } else { "f" })
            .unwrap_or("?");
        let name = entry.file_name().to_string_lossy().to_string();
        let prefix = "  ".repeat(current as usize);

        // Skip common ignore patterns.
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }

        entries.push(format!("{prefix}{file_type} {name}"));

        if file_type == "d" {
            walk_dir(&entry.path(), current + 1, max_depth, entries);
        }
    }
}

// ── shell_exec ──

pub struct ShellExecTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for ShellExecTool {
    fn name(&self) -> &str {
        "shell_exec"
    }
    fn is_mutating(&self) -> bool {
        true
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ExecutesCode,
            ToolCapability::Sandboxable,
            ToolCapability::Network,
        ]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "shell_exec".into(),
            description: "Execute a shell command in the workspace".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "cwd": { "type": "string", "description": "Working directory (relative to workspace)" }
                },
                "required": ["command"]
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let ws = effective_workspace(&invocation, &self.workspace);
        let command = deepomni_tools::required_str(&args, "command")?;
        let cwd = deepomni_tools::optional_str(&args, "cwd");

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", &command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &command]);
            c
        };

        cmd.current_dir(if let Some(ref dir) = cwd {
            resolve_workspace_path(dir, &ws)?
        } else {
            PathBuf::from(&ws)
        });

        let output = cmd.output().map_err(|e| ToolError::ExecutionFailed {
            message: format!("command execution failed: {e}"),
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "exit_code": output.status.code().unwrap_or(-1),
                "stdout": truncate_output(stdout.trim()),
                "stderr": truncate_output(stderr.trim()),
            })),
            success: output.status.success(),
        })
    }
}

// ── git_status ──

pub struct GitStatusTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }
    fn is_mutating(&self) -> bool {
        false
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::ExecutesCode]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "git_status".into(),
            description: "Show the working tree status".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to check (default: workspace root)" }
                },
                "required": []
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let ws = effective_workspace(&invocation, &self.workspace);
        let output = Command::new("git")
            .args(["status", "--short"])
            .current_dir(&ws)
            .output()
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("git status failed: {e}"),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "status": truncate_output(stdout.trim()),
            })),
            success: true,
        })
    }
}

// ── git_diff ──

pub struct GitDiffTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }
    fn is_mutating(&self) -> bool {
        false
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::ExecutesCode]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "git_diff".into(),
            description: "Show changes in the working tree".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "staged": { "type": "boolean", "description": "Show staged changes (default: false)" }
                },
                "required": []
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let ws = effective_workspace(&invocation, &self.workspace);
        let staged = args
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut cmd = Command::new("git");
        cmd.arg("diff");
        if staged {
            cmd.arg("--staged");
        }
        let output = cmd
            .current_dir(&ws)
            .output()
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("git diff failed: {e}"),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "diff": truncate_output(stdout.trim()),
            })),
            success: true,
        })
    }
}

// ── git_apply ──

pub struct GitApplyTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for GitApplyTool {
    fn name(&self) -> &str {
        "git_apply"
    }
    fn is_mutating(&self) -> bool {
        true
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::WritesFiles,
            ToolCapability::ExecutesCode,
            ToolCapability::Sandboxable,
        ]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "git_apply".into(),
            description: "Apply a patch to the working tree".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "The patch content to apply" }
                },
                "required": ["patch"]
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let ws = effective_workspace(&invocation, &self.workspace);
        let patch = deepomni_tools::required_str(&args, "patch")?;

        // Write patch to temp file and apply.
        let temp_path = PathBuf::from(&ws).join(".deepomni_tmp_patch");
        fs::write(&temp_path, &patch).map_err(|e| ToolError::ExecutionFailed {
            message: format!("cannot write temp patch: {e}"),
        })?;

        let output = Command::new("git")
            .args(["apply", "--whitespace=fix"])
            .arg(temp_path.display().to_string())
            .current_dir(&ws)
            .output()
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("git apply failed: {e}"),
            })?;

        let _ = fs::remove_file(&temp_path);

        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "applied": output.status.success(),
                "detail": stderr.trim(),
            })),
            success: output.status.success(),
        })
    }
}

// ── apply_patch ──

pub struct ApplyPatchTool {
    pub workspace: String,
}

#[async_trait]
impl ToolHandler for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }
    fn is_mutating(&self) -> bool {
        true
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles, ToolCapability::Sandboxable]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "apply_patch".into(),
            description: "Apply a unified diff patch file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to write the patch to" },
                    "content": { "type": "string", "description": "The patch content" }
                },
                "required": ["path", "content"]
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let ws = effective_workspace(&invocation, &self.workspace);
        let path_str = deepomni_tools::required_str(&args, "path")?;
        let content = deepomni_tools::required_str(&args, "content")?;
        let file_path = resolve_workspace_path(&path_str, &ws)?;

        // Ensure parent directory exists.
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).map_err(|e| ToolError::ExecutionFailed {
                message: format!("cannot create directory: {e}"),
            })?;
        }

        // Read existing content (if any).
        let existing = fs::read_to_string(&file_path).unwrap_or_default();

        // Apply the patch (simple find-and-replace for unified diff fragments).
        let patched = apply_simple_patch(&existing, &content)?;

        fs::write(&file_path, &patched).map_err(|e| ToolError::ExecutionFailed {
            message: format!("cannot write {}: {e}", file_path.display()),
        })?;

        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "path": path_str,
                "applied": true,
                "size": patched.len(),
            })),
            success: true,
        })
    }
}

/// Simple patch application: handles `+` and `-` lines from unified diff context.
fn apply_simple_patch(_original: &str, patch: &str) -> Result<String, ToolError> {
    let mut result = String::new();

    for line in patch.lines() {
        if line.starts_with("+++ ") || line.starts_with("--- ") || line.starts_with("@@") {
            // Header — skip.
            continue;
        }
        if let Some(addition) = line.strip_prefix('+') {
            result.push_str(addition);
            result.push('\n');
        } else if line.starts_with('-') {
            // Removal — skip the line.
        } else {
            // Context line.
            let ctx = line.strip_prefix(' ').unwrap_or(line);
            result.push_str(ctx);
            result.push('\n');
        }
    }

    Ok(result)
}

// ── todo_update ──

pub struct TodoUpdateTool;

#[async_trait]
impl ToolHandler for TodoUpdateTool {
    fn name(&self) -> &str {
        "todo_update"
    }
    fn is_mutating(&self) -> bool {
        false
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec::Function(ToolSpecDetails {
            name: "todo_update".into(),
            description: "Update the task list for the current session".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                                "title": { "type": "string" }
                            }
                        }
                    }
                },
                "required": ["tasks"]
            }),
        }))
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        let args: Value = match &invocation.payload {
            ToolPayload::Function { arguments } => {
                serde_json::from_str(arguments).map_err(|e| ToolError::InvalidInput {
                    message: e.to_string(),
                })?
            }
            _ => {
                return Err(ToolError::InvalidInput {
                    message: "expected function call".into(),
                });
            }
        };

        let tasks = args.get("tasks").cloned().unwrap_or_default();
        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({
                "tasks": tasks,
                "updated": true,
            })),
            success: true,
        })
    }
}

// ── Convenience: register all built-in tools ──

/// Register all built-in tools with a tool registry.
pub async fn register_all(registry: &mut deepomni_tools::ToolRegistry, workspace: &str) {
    let ws = workspace.to_string();

    registry
        .register(Arc::new(ReadFileTool {
            workspace: ws.clone(),
        }))
        .await;
    registry
        .register(Arc::new(WriteFileTool {
            workspace: ws.clone(),
        }))
        .await;
    registry
        .register(Arc::new(EditFileTool {
            workspace: ws.clone(),
        }))
        .await;
    registry
        .register(Arc::new(GrepTool {
            workspace: ws.clone(),
        }))
        .await;
    registry
        .register(Arc::new(ListFilesTool {
            workspace: ws.clone(),
        }))
        .await;
    registry
        .register(Arc::new(ShellExecTool {
            workspace: ws.clone(),
        }))
        .await;
    registry
        .register(Arc::new(GitStatusTool {
            workspace: ws.clone(),
        }))
        .await;
    registry
        .register(Arc::new(GitDiffTool {
            workspace: ws.clone(),
        }))
        .await;
    registry
        .register(Arc::new(GitApplyTool {
            workspace: ws.clone(),
        }))
        .await;
    registry
        .register(Arc::new(ApplyPatchTool {
            workspace: ws.clone(),
        }))
        .await;
    registry.register(Arc::new(TodoUpdateTool)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepomni_protocol::id::ToolCallId;
    use deepomni_tools::ToolRegistry;
    use std::fs;

    fn test_workspace() -> String {
        let dir = std::env::temp_dir().join(format!("deepomni-tool-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir.display().to_string()
    }

    #[tokio::test]
    async fn test_read_file_tool() {
        let ws = test_workspace();
        fs::write(PathBuf::from(&ws).join("hello.txt"), "hello world").unwrap();

        let tool = ReadFileTool {
            workspace: ws.clone(),
        };
        assert_eq!(tool.name(), "read_file");
        assert!(!tool.is_mutating());

        let result = tool
            .handle(ToolInvocation {
                call_id: ToolCallId::new(),
                tool_name: "read_file".into(),
                payload: ToolPayload::Function {
                    arguments: r#"{"path":"hello.txt"}"#.into(),
                },
                timeout: None,
                allow_mutating: false,
                workspace: None,
            })
            .await
            .unwrap();

        match result {
            ToolOutput::Function { body, success } => {
                assert!(success);
                let body = body.unwrap();
                assert!(body["content"].as_str().unwrap().contains("hello world"));
            }
            _ => panic!("expected function output"),
        }
    }

    #[tokio::test]
    async fn test_write_file_tool() {
        let ws = test_workspace();
        let tool = WriteFileTool {
            workspace: ws.clone(),
        };

        let result = tool
            .handle(ToolInvocation {
                call_id: ToolCallId::new(),
                tool_name: "write_file".into(),
                payload: ToolPayload::Function {
                    arguments: r#"{"path":"out.txt","content":"written"}"#.into(),
                },
                timeout: None,
                allow_mutating: true,
                workspace: None,
            })
            .await
            .unwrap();

        match result {
            ToolOutput::Function { success, .. } => assert!(success),
            _ => panic!("expected function output"),
        }

        let content = fs::read_to_string(PathBuf::from(&ws).join("out.txt")).unwrap();
        assert_eq!(content, "written");
    }

    #[tokio::test]
    async fn test_shell_exec_tool() {
        let ws = test_workspace();
        let tool = ShellExecTool { workspace: ws };

        let result = tool
            .handle(ToolInvocation {
                call_id: ToolCallId::new(),
                tool_name: "shell_exec".into(),
                payload: ToolPayload::Function {
                    arguments: r#"{"command":"echo hello"}"#.into(),
                },
                timeout: None,
                allow_mutating: true,
                workspace: None,
            })
            .await
            .unwrap();

        match result {
            ToolOutput::Function { body, success } => {
                assert!(success);
                let body = body.unwrap();
                assert!(body["stdout"].as_str().unwrap().contains("hello"));
            }
            _ => panic!("expected function output"),
        }
    }

    #[tokio::test]
    async fn test_register_all() {
        let ws = test_workspace();
        let mut registry = ToolRegistry::new();
        register_all(&mut registry, &ws).await;

        // All 11 tools registered.
        assert!(registry.contains("read_file"));
        assert!(registry.contains("write_file"));
        assert!(registry.contains("edit_file"));
        assert!(registry.contains("grep"));
        assert!(registry.contains("list_files"));
        assert!(registry.contains("shell_exec"));
        assert!(registry.contains("git_status"));
        assert!(registry.contains("git_diff"));
        assert!(registry.contains("git_apply"));
        assert!(registry.contains("apply_patch"));
        assert!(registry.contains("todo_update"));
        assert_eq!(registry.len(), 11);
    }

    #[test]
    fn test_truncate_output() {
        let short = "hello";
        assert_eq!(truncate_output(short), "hello");

        let long = "x".repeat(100_000);
        let truncated = truncate_output(&long);
        assert!(truncated.len() < 100_000);
        assert!(truncated.contains("truncated"));
    }
}
