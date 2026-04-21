use std::collections::HashSet;

const HIGH_SUFFIX: &str = "-high";
const XHIGH_SUFFIX: &str = "-xhigh";
const EXTRA_HIGH_SUFFIXES: &[&str] = &["-extra-high", "-extra_high", "-extrahigh"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelProfile {
    pub requested_model: String,
    pub backend_model: String,
    pub effort: Option<String>,
}

pub fn resolve_model_profile(model: &str) -> ResolvedModelProfile {
    if let Some(base) = model.strip_suffix(XHIGH_SUFFIX) {
        return ResolvedModelProfile {
            requested_model: model.to_string(),
            backend_model: base.to_string(),
            effort: Some("xhigh".to_string()),
        };
    }

    for suffix in EXTRA_HIGH_SUFFIXES {
        if let Some(base) = model.strip_suffix(suffix) {
            return ResolvedModelProfile {
                requested_model: model.to_string(),
                backend_model: base.to_string(),
                effort: Some("xhigh".to_string()),
            };
        }
    }

    if let Some(base) = model.strip_suffix(HIGH_SUFFIX) {
        return ResolvedModelProfile {
            requested_model: model.to_string(),
            backend_model: base.to_string(),
            effort: Some("high".to_string()),
        };
    }

    ResolvedModelProfile {
        requested_model: model.to_string(),
        backend_model: model.to_string(),
        effort: None,
    }
}

pub fn expand_public_models<I, S>(models: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for model in models {
        let resolved = resolve_model_profile(model.as_ref());
        push_model(&mut out, &mut seen, &resolved.backend_model);
        push_model(
            &mut out,
            &mut seen,
            &format!("{}{}", resolved.backend_model, HIGH_SUFFIX),
        );
        push_model(
            &mut out,
            &mut seen,
            &format!("{}{}", resolved.backend_model, XHIGH_SUFFIX),
        );
    }

    out
}

fn push_model(out: &mut Vec<String>, seen: &mut HashSet<String>, model: &str) {
    if seen.insert(model.to_string()) {
        out.push(model.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_canonical_reasoning_model_aliases() {
        let resolved = resolve_model_profile("gpt-5.2-codex-xhigh");

        assert_eq!(resolved.backend_model, "gpt-5.2-codex");
        assert_eq!(resolved.effort.as_deref(), Some("xhigh"));
    }

    #[test]
    fn resolves_extra_high_alias_variants() {
        let resolved = resolve_model_profile("gpt-5.2-codex-extra-high");

        assert_eq!(resolved.backend_model, "gpt-5.2-codex");
        assert_eq!(resolved.effort.as_deref(), Some("xhigh"));
    }

    #[test]
    fn expands_public_models_with_reasoning_profiles() {
        let models = expand_public_models([
            "gpt-5.2-codex".to_string(),
            "gpt-5.2-codex".to_string(),
        ]);

        assert_eq!(
            models,
            vec![
                "gpt-5.2-codex".to_string(),
                "gpt-5.2-codex-high".to_string(),
                "gpt-5.2-codex-xhigh".to_string(),
            ]
        );
    }
}
