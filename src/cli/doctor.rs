use anyhow::Result;
use serde::Serialize;

use super::{degraded_surfaces, detect_codex_binary, probe_app_server, DoctorArgs};
use crate::surfaces::{CompatibilityMatrix, SurfaceRegistry};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub codex_binary: super::CodexBinaryInfo,
    pub app_server: super::AppServerProbe,
    pub transport_mode: String,
    pub operation_mode: crate::surfaces::OperationMode,
    pub api_stability: crate::app_server::ApiStability,
    pub delegation_policy: crate::app_server::DelegationPolicy,
    pub supported_tiers: Vec<u8>,
    #[serde(default)]
    pub degraded_surfaces: Vec<DoctorSurfaceIssue>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorSurfaceIssue {
    pub surface_id: String,
    pub reasons: Vec<String>,
}

pub async fn run_doctor(args: &DoctorArgs) -> Result<DoctorReport> {
    let runtime = args.runtime.resolve();
    let codex_binary = detect_codex_binary().await;
    let app_server = probe_app_server(&runtime).await;
    let registry = SurfaceRegistry::new();
    let matrix = CompatibilityMatrix::new(&registry);
    let degraded = degraded_surfaces(&registry, &matrix, runtime.operation_mode)
        .into_iter()
        .map(|(surface_id, reasons)| DoctorSurfaceIssue {
            surface_id,
            reasons,
        })
        .collect();
    let mut supported_tiers = registry
        .all()
        .iter()
        .filter(|surface| {
            matrix
                .get(&surface.id, runtime.operation_mode)
                .map(|decision| decision.unsupported_reason.is_none())
                .unwrap_or(false)
        })
        .map(|surface| surface.tier)
        .collect::<Vec<_>>();
    supported_tiers.sort_unstable();
    supported_tiers.dedup();

    Ok(DoctorReport {
        codex_binary,
        app_server: app_server.clone(),
        transport_mode: if app_server.available {
            "app-server-stdio".to_string()
        } else if matches!(
            runtime.operation_mode,
            crate::surfaces::OperationMode::ResponsesOnly
        ) {
            "responses-only".to_string()
        } else {
            "degraded".to_string()
        },
        operation_mode: runtime.operation_mode,
        api_stability: runtime.api_stability,
        delegation_policy: runtime.delegation_policy,
        supported_tiers,
        degraded_surfaces: degraded,
    })
}
