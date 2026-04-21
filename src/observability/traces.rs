use log::{info, warn};

use crate::surfaces::model::{MappingDecision, MappingStrategy};

/// Log every downgrade decision with surface context (P7-010).
pub fn log_mapping_decision(decision: &MappingDecision) {
    let downgraded = !matches!(
        decision.strategy,
        MappingStrategy::Native | MappingStrategy::MediatedNative
    );

    if downgraded {
        warn!(
            "surface_bridge: DOWNGRADE surface_id={} strategy={:?} fallback={:?} reason={:?} warnings={:?}",
            decision.surface_id,
            decision.strategy,
            decision.fallback_mode,
            decision.unsupported_reason,
            decision.warnings,
        );
    } else {
        info!(
            "surface_bridge: surface_id={} strategy={:?} target={:?}",
            decision.surface_id, decision.strategy, decision.target_backend,
        );
    }
}

/// Structured trace record for telemetry collection.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingTrace {
    pub surface_id: String,
    pub strategy: MappingStrategy,
    pub target_backend: String,
    pub downgraded: bool,
    pub warnings: Vec<String>,
    pub timestamp_unix: i64,
}

impl From<&MappingDecision> for MappingTrace {
    fn from(decision: &MappingDecision) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        Self {
            surface_id: decision.surface_id.clone(),
            strategy: decision.strategy,
            target_backend: decision.target_backend.clone(),
            downgraded: !matches!(
                decision.strategy,
                MappingStrategy::Native | MappingStrategy::MediatedNative
            ),
            warnings: decision.warnings.clone(),
            timestamp_unix: now,
        }
    }
}
