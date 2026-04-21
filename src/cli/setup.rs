use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use super::{detect_claude_config_paths, detect_codex_binary, probe_app_server, SetupArgs};
use crate::cli::env::build_env_snippet;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupReport {
    pub codex_binary: super::CodexBinaryInfo,
    pub app_server: super::AppServerProbe,
    pub config_snippet: crate::cli::env::EnvSnippet,
    #[serde(default)]
    pub written_paths: Vec<String>,
}

pub async fn run_setup(args: &SetupArgs) -> Result<SetupReport> {
    let runtime = args.runtime.resolve();
    let codex_binary = detect_codex_binary().await;
    let app_server = probe_app_server(&runtime).await;
    let config_snippet = build_env_snippet(&runtime, None).await;

    let written_paths = if args.write_config {
        let cwd = std::env::current_dir().context("failed to determine current directory")?;
        write_config_snippet(&cwd, &config_snippet)?
    } else {
        Vec::new()
    };

    Ok(SetupReport {
        codex_binary,
        app_server,
        config_snippet,
        written_paths,
    })
}

fn write_config_snippet(cwd: &Path, snippet: &crate::cli::env::EnvSnippet) -> Result<Vec<String>> {
    let mut written = Vec::new();
    let paths = detect_claude_config_paths(cwd);

    for path in paths {
        if path.file_name().and_then(|name| name.to_str()) != Some("settings.json") {
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }

        let mut value = if path.exists() {
            serde_json::from_str::<Value>(
                &fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?,
            )
            .with_context(|| format!("failed to parse {}", path.display()))?
        } else {
            json!({
                "$schema": "https://json.schemastore.org/claude-code-settings.json"
            })
        };

        if !value.is_object() {
            anyhow::bail!("{} is not a JSON object", path.display());
        }

        let object = value.as_object_mut().expect("object");
        let env = object
            .entry("env")
            .or_insert_with(|| Value::Object(Default::default()));
        let env_object = env.as_object_mut().context("env field is not an object")?;
        env_object.insert(
            "ANTHROPIC_API_KEY".to_string(),
            Value::String(snippet.anthropic_api_key.clone()),
        );
        env_object.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            Value::String(snippet.anthropic_base_url.clone()),
        );
        env_object.insert(
            "ANTHROPIC_MODEL".to_string(),
            Value::String(snippet.anthropic_model.clone()),
        );

        fs::write(&path, serde_json::to_string_pretty(&value)?)
            .with_context(|| format!("failed to write {}", path.display()))?;
        written.push(path.display().to_string());
    }

    Ok(written)
}
