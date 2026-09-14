use crate::logger::dirs;
use crate::models::{ModelOption, PermissionMode};
use anyhow::{anyhow, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

pub const DEFAULT_CONTEXT_WINDOW: i64 = 258_000;

/// Reads `~/.hermes/config.yaml` for the current provider/model pair.
pub fn read_model_config() -> (String, String) {
    let config = hermes_config_path();
    let Ok(text) = std::fs::read_to_string(&config) else {
        return (String::new(), String::new());
    };
    let (approval, _) = read_yaml_section(&text, "model");
    (
        yaml_value(&approval, "provider"),
        yaml_value(&approval, "default"),
    )
}

/// Reads `~/.hermes/provider_models_cache.json`, inserting the current model
/// if the cache does not list it.
pub fn read_model_options(current_provider: &str, current_model: &str) -> Vec<ModelOption> {
    #[derive(serde::Deserialize)]
    struct ProviderCache {
        #[serde(default)]
        models: Vec<String>,
    }

    let mut options: Vec<ModelOption> = Vec::new();
    let path = dirs::home().join(".hermes/provider_models_cache.json");
    if let Ok(data) = std::fs::read(&path) {
        if let Ok(decoded) = serde_json::from_slice::<BTreeMap<String, ProviderCache>>(&data) {
            let mut providers: Vec<&String> = decoded.keys().collect();
            providers.sort_by(|lhs, rhs| provider_sort(lhs, rhs, current_provider));
            for provider in providers {
                for model in &decoded[provider].models {
                    options.push(ModelOption {
                        provider: provider.clone(),
                        model: model.clone(),
                    });
                }
            }
        }
    }

    if !current_provider.is_empty()
        && !current_model.is_empty()
        && !options
            .iter()
            .any(|o| o.provider == current_provider && o.model == current_model)
    {
        options.insert(
            0,
            ModelOption {
                provider: current_provider.to_string(),
                model: current_model.to_string(),
            },
        );
    }
    options
}

fn provider_sort(lhs: &str, rhs: &str, current: &str) -> std::cmp::Ordering {
    if lhs == current && rhs != current {
        return std::cmp::Ordering::Less;
    }
    if rhs == current && lhs != current {
        return std::cmp::Ordering::Greater;
    }
    lhs.to_lowercase().cmp(&rhs.to_lowercase())
}

/// Reads the context window limit for provider/model from models_dev_cache.json.
pub fn read_context_window_tokens(provider: &str, model: &str) -> i64 {
    let path = dirs::home().join(".hermes/models_dev_cache.json");
    let Ok(data) = std::fs::read(&path) else {
        return DEFAULT_CONTEXT_WINDOW;
    };
    let Ok(root) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return DEFAULT_CONTEXT_WINDOW;
    };

    fn extract(value: &serde_json::Value, provider: &str, model: &str) -> Option<i64> {
        let models = value.get(provider)?.get("models")?;
        models.get(model)?.get("limit")?.get("context")?.as_i64()
    }

    if let Some(limit) = extract(&root, provider, model) {
        return limit;
    }
    if let Some(root_obj) = root.as_object() {
        for (_key, value) in root_obj {
            if let Some(models) = value.get("models").and_then(|m| m.as_object()) {
                if let Some(info) = models.get(model) {
                    if let Some(context) = info.get("limit").and_then(|l| l.get("context")) {
                        if let Some(n) = context.as_i64() {
                            return n;
                        }
                    }
                }
            }
        }
    }
    DEFAULT_CONTEXT_WINDOW
}

/// Current permission mode from `~/.hermes/config.yaml`.
pub fn read_permission_mode() -> PermissionMode {
    let config = hermes_config_path();
    let Ok(text) = std::fs::read_to_string(&config) else {
        return PermissionMode::FullAccess;
    };
    let (approvals, _) = read_yaml_section(&text, "approvals");
    let (agent, _) = read_yaml_section(&text, "agent");
    let disabled = yaml_value(&agent, "disabled_toolsets");
    if disabled.contains("terminal")
        || disabled.contains("code_execution")
        || disabled.contains("computer_use")
    {
        return PermissionMode::RestrictedTools;
    }
    let approval_mode = yaml_value(&approvals, "mode");
    if matches!(approval_mode.as_str(), "ask" | "prompt" | "on") {
        return PermissionMode::AskBeforeRisky;
    }
    PermissionMode::FullAccess
}

/// Applies a permission mode through `hermes config set` (same keys as Swift).
pub fn set_permission_mode(mode: PermissionMode) -> Result<()> {
    match mode {
        PermissionMode::FullAccess => {
            run_config_set("approvals.mode", "off")?;
            run_config_set("agent.disabled_toolsets", "[]")?;
        }
        PermissionMode::AskBeforeRisky => {
            run_config_set("approvals.mode", "ask")?;
            run_config_set("agent.disabled_toolsets", "[]")?;
        }
        PermissionMode::RestrictedTools => {
            run_config_set("approvals.mode", "ask")?;
            run_config_set(
                "agent.disabled_toolsets",
                "[terminal,file,code_execution,computer_use]",
            )?;
        }
    }
    Ok(())
}

pub fn select_model(option: &ModelOption) -> Result<()> {
    run_config_set("model.provider", &option.provider)?;
    run_config_set("model.default", &option.model)
}

fn run_config_set(key: &str, value: &str) -> Result<()> {
    let (executable, prefix) = hermes_command();
    let output = Command::new(executable)
        .args(prefix)
        .args(["config", "set", key, value])
        .output()?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(if message.is_empty() {
            "hermes config set failed".to_string()
        } else {
            message
        }));
    }
    Ok(())
}

/// Locate the hermes CLI, preferring well-known install locations.
pub fn hermes_executable_path() -> Option<PathBuf> {
    let home = dirs::home();
    let candidates = [
        home.join(".local/bin/hermes"),
        PathBuf::from("/opt/homebrew/bin/hermes"),
        PathBuf::from("/usr/local/bin/hermes"),
    ];
    candidates.into_iter().find(|p| is_executable(p))
}

fn hermes_command() -> (PathBuf, Vec<String>) {
    match hermes_executable_path() {
        Some(path) => (path, Vec::new()),
        None => (PathBuf::from("/usr/bin/env"), vec!["hermes".to_string()]),
    }
}

fn is_executable(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn hermes_config_path() -> PathBuf {
    dirs::home().join(".hermes/config.yaml")
}

/// Minimal YAML reader: returns the key->value map of one top-level section.
fn read_yaml_section(text: &str, section: &str) -> (BTreeMap<String, String>, bool) {
    let mut values = BTreeMap::new();
    let mut in_section = false;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if raw_line == format!("{section}:") {
            in_section = true;
            continue;
        }
        if in_section && !raw_line.starts_with(' ') && !raw_line.starts_with('\t') {
            break;
        }
        if !in_section {
            continue;
        }
        if let Some(colon) = line.find(':') {
            let key = line[..colon].trim().to_string();
            let value = line[colon + 1..].trim();
            let value = value.trim_matches(|c| c == '"' || c == '\'').to_string();
            values.insert(key, value);
        }
    }
    (values, in_section)
}

fn yaml_value(section: &BTreeMap<String, String>, key: &str) -> String {
    section.get(key).cloned().unwrap_or_default()
}
