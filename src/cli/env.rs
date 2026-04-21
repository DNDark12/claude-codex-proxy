use serde::Serialize;

use super::{discover_public_models, EnvArgs, EnvShell, RuntimeConfig};

#[derive(Debug, Clone, Serialize)]
pub struct EnvSnippet {
    #[serde(rename = "ANTHROPIC_API_KEY")]
    pub anthropic_api_key: String,
    #[serde(rename = "ANTHROPIC_BASE_URL")]
    pub anthropic_base_url: String,
    #[serde(rename = "ANTHROPIC_MODEL")]
    pub anthropic_model: String,
}

pub async fn render_env(args: &EnvArgs) -> anyhow::Result<String> {
    let runtime = args.runtime.resolve();
    let snippet = build_env_snippet(&runtime, args.model.clone()).await;
    Ok(match args.shell {
        Some(shell) => format_shell(&snippet, shell),
        None => serde_json::to_string_pretty(&snippet)?,
    })
}

pub async fn build_env_snippet(
    runtime: &RuntimeConfig,
    model_override: Option<String>,
) -> EnvSnippet {
    let model = if let Some(model) = model_override {
        model
    } else {
        discover_public_models(runtime)
            .await
            .into_iter()
            .next()
            .unwrap_or_else(|| "gpt-5.2-codex".to_string())
    };

    EnvSnippet {
        anthropic_api_key: "dummy".to_string(),
        anthropic_base_url: format!("http://127.0.0.1:{}", runtime.port),
        anthropic_model: model,
    }
}

pub fn format_shell(snippet: &EnvSnippet, shell: EnvShell) -> String {
    match shell {
        EnvShell::Bash | EnvShell::Zsh => format!(
            "export ANTHROPIC_API_KEY={}\nexport ANTHROPIC_BASE_URL={}\nexport ANTHROPIC_MODEL={}",
            shell_quote(&snippet.anthropic_api_key),
            shell_quote(&snippet.anthropic_base_url),
            shell_quote(&snippet.anthropic_model),
        ),
        EnvShell::Fish => format!(
            "set -x ANTHROPIC_API_KEY {}\nset -x ANTHROPIC_BASE_URL {}\nset -x ANTHROPIC_MODEL {}",
            shell_quote(&snippet.anthropic_api_key),
            shell_quote(&snippet.anthropic_base_url),
            shell_quote(&snippet.anthropic_model),
        ),
        EnvShell::Powershell => format!(
            "$env:ANTHROPIC_API_KEY = \"{}\"\n$env:ANTHROPIC_BASE_URL = \"{}\"\n$env:ANTHROPIC_MODEL = \"{}\"",
            snippet.anthropic_api_key,
            snippet.anthropic_base_url,
            snippet.anthropic_model,
        ),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_zsh_exports() {
        let rendered = format_shell(
            &EnvSnippet {
                anthropic_api_key: "dummy".to_string(),
                anthropic_base_url: "http://127.0.0.1:8080".to_string(),
                anthropic_model: "gpt-5.2-codex".to_string(),
            },
            EnvShell::Zsh,
        );

        assert!(rendered.contains("export ANTHROPIC_API_KEY='dummy'"));
        assert!(rendered.contains("export ANTHROPIC_BASE_URL='http://127.0.0.1:8080'"));
        assert!(rendered.contains("export ANTHROPIC_MODEL='gpt-5.2-codex'"));
    }
}
