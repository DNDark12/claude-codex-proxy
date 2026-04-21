use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::session::ApiStability;
use super::transport_stdio::StdioTransport;
use crate::mapping::approvals::ConfigRequirements;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandshakeState {
    pub user_agent: String,
    pub api_stability: ApiStability,
    pub requirements: Option<ConfigRequirements>,
}

#[derive(Debug, Deserialize)]
struct InitializeResponse {
    #[serde(rename = "userAgent")]
    user_agent: String,
}

#[derive(Debug, Deserialize)]
struct ConfigRequirementsResponse {
    requirements: Option<ConfigRequirements>,
}

pub async fn perform_handshake(
    transport: &StdioTransport,
    api_stability: ApiStability,
) -> Result<HandshakeState> {
    let initialize: InitializeResponse = transport
        .request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "claude-codex-proxy",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": matches!(api_stability, ApiStability::Experimental),
                }
            }),
        )
        .await?;
    transport.notify("initialized", json!({})).await?;
    let requirements: ConfigRequirementsResponse = transport
        .request("configRequirements/read", json!({}))
        .await?;

    Ok(HandshakeState {
        user_agent: initialize.user_agent,
        api_stability,
        requirements: requirements.requirements,
    })
}
