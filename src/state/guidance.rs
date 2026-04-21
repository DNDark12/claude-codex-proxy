use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Guidance layer state storage (P6-003).
#[derive(Debug, Clone, Default)]
pub struct GuidanceStore {
    layers: Arc<RwLock<HashMap<String, GuidanceLayer>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuidanceLayer {
    pub name: String,
    pub source: GuidanceSource,
    pub content: String,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceSource {
    AgentsMd,
    ClaudeMd,
    ConfigToml,
    UserProvided,
}

impl GuidanceStore {
    pub async fn insert(&self, layer: GuidanceLayer) {
        self.layers.write().await.insert(layer.name.clone(), layer);
    }

    pub async fn get(&self, name: &str) -> Option<GuidanceLayer> {
        self.layers.read().await.get(name).cloned()
    }

    pub async fn list_active(&self) -> Vec<GuidanceLayer> {
        self.layers
            .read()
            .await
            .values()
            .filter(|l| l.active)
            .cloned()
            .collect()
    }
}

