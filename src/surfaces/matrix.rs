use std::collections::HashMap;

use super::model::{
    FallbackMode, MappingDecision, MappingStrategy, OperationMode, SurfaceBucket,
    SurfaceDescriptor, UnsupportedReason,
};
use super::registry::SurfaceRegistry;

#[derive(Debug, Clone)]
pub struct CompatibilityMatrix {
    decisions: HashMap<(String, OperationMode), MappingDecision>,
}

impl CompatibilityMatrix {
    pub fn new(registry: &SurfaceRegistry) -> Self {
        let mut decisions = HashMap::new();

        for surface in registry.all() {
            for mode in [
                OperationMode::StrictAppServer,
                OperationMode::AutoHybrid,
                OperationMode::ResponsesOnly,
            ] {
                decisions.insert((surface.id.clone(), mode), build_decision(surface, mode));
            }
        }

        Self { decisions }
    }

    pub fn get(&self, surface_id: &str, mode: OperationMode) -> Option<&MappingDecision> {
        self.decisions.get(&(surface_id.to_string(), mode))
    }

    pub fn all_for_mode(&self, mode: OperationMode) -> Vec<&MappingDecision> {
        let mut out: Vec<_> = self
            .decisions
            .iter()
            .filter_map(|((_, decision_mode), decision)| {
                (*decision_mode == mode).then_some(decision)
            })
            .collect();
        out.sort_by(|a, b| a.surface_id.cmp(&b.surface_id));
        out
    }
}

impl From<&SurfaceRegistry> for CompatibilityMatrix {
    fn from(registry: &SurfaceRegistry) -> Self {
        Self::new(registry)
    }
}

fn build_decision(surface: &SurfaceDescriptor, mode: OperationMode) -> MappingDecision {
    if matches!(
        surface.bucket,
        SurfaceBucket::HostAdminUx | SurfaceBucket::OutOfScope
    ) {
        return MappingDecision {
            surface_id: surface.id.clone(),
            target_backend: "none".to_string(),
            target_surface: None,
            strategy: MappingStrategy::UnsupportedExplicit,
            fallback_mode: FallbackMode::DropWithObservability,
            requires_mode: mode,
            unsupported_reason: Some(UnsupportedReason::DeprecatedSourceSurface),
            warnings: vec!["Surface excluded from bridge backlog".to_string()],
        };
    }

    if matches!(surface.bucket, SurfaceBucket::PlatformSpecific)
        && !surface.availability_gate.is_satisfied()
    {
        return MappingDecision {
            surface_id: surface.id.clone(),
            target_backend: "codex_app_server".to_string(),
            target_surface: None,
            strategy: MappingStrategy::UnsupportedExplicit,
            fallback_mode: FallbackMode::HardError,
            requires_mode: mode,
            unsupported_reason: Some(UnsupportedReason::PlatformSpecificGap),
            warnings: vec!["Platform or plugin gate not satisfied".to_string()],
        };
    }

    match mode {
        OperationMode::StrictAppServer | OperationMode::AutoHybrid => MappingDecision {
            surface_id: surface.id.clone(),
            target_backend: "codex_app_server".to_string(),
            target_surface: Some(surface.source_name.clone()),
            strategy: surface.strategy,
            fallback_mode: surface.fallback_mode,
            requires_mode: mode,
            unsupported_reason: if surface.strategy == MappingStrategy::UnsupportedExplicit {
                Some(UnsupportedReason::MissingBackendPrimitive)
            } else {
                None
            },
            warnings: if surface.availability_gate.is_satisfied() {
                Vec::new()
            } else {
                vec!["Availability gate not fully satisfied".to_string()]
            },
        },
        OperationMode::ResponsesOnly => {
            let can_degrade = surface.tier == 0
                || matches!(
                    surface.source_name.as_str(),
                    "WebFetch" | "WebSearch" | "NotebookRead"
                );
            MappingDecision {
                surface_id: surface.id.clone(),
                target_backend: "codex_responses_api".to_string(),
                target_surface: can_degrade.then(|| surface.source_name.clone()),
                strategy: if can_degrade {
                    surface.strategy
                } else {
                    MappingStrategy::UnsupportedExplicit
                },
                fallback_mode: if can_degrade {
                    FallbackMode::SoftWarningAndContinue
                } else if surface.fallback_mode == FallbackMode::DropWithObservability {
                    FallbackMode::DropWithObservability
                } else {
                    FallbackMode::HardError
                },
                requires_mode: mode,
                unsupported_reason: (!can_degrade).then_some(UnsupportedReason::StateModelMismatch),
                warnings: if can_degrade {
                    vec!["Responses-only mode loses thread-native state".to_string()]
                } else {
                    vec!["Surface requires app-server-native state".to_string()]
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surfaces::registry::SurfaceRegistry;

    #[test]
    fn host_admin_surfaces_drop_with_observability() {
        let registry = SurfaceRegistry::new();
        let matrix = CompatibilityMatrix::new(&registry);
        let decision = matrix
            .get("command.help", OperationMode::StrictAppServer)
            .expect("decision");
        assert_eq!(decision.fallback_mode, FallbackMode::DropWithObservability);
    }
}
