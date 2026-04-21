use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::mapping::approvals::{ApprovalPolicy, SandboxConfig};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfile {
    pub name: String,
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxConfig,
}

#[derive(Debug, Clone, Default)]
pub struct PermissionStore {
    profiles: Arc<RwLock<HashMap<String, PermissionProfile>>>,
}

impl PermissionStore {
    pub async fn insert(&self, profile: PermissionProfile) {
        self.profiles
            .write()
            .await
            .insert(profile.name.clone(), profile);
    }

    pub async fn get(&self, name: &str) -> Option<PermissionProfile> {
        self.profiles.read().await.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<PermissionProfile> {
        let mut profiles: Vec<_> = self.profiles.read().await.values().cloned().collect();
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
        profiles
    }
}
