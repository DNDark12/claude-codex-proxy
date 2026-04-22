use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::fs;

use crate::app_server::BridgeSession;

pub async fn load_sessions(path: &Path) -> Result<HashMap<String, BridgeSession>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content = fs::read_to_string(path).await?;
    let mut sessions = HashMap::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let session: BridgeSession = serde_json::from_str(line)?;
        sessions.insert(session.bridge_session_id.clone(), session);
    }
    Ok(sessions)
}

pub async fn save_sessions(
    path: &PathBuf,
    sessions: &HashMap<String, BridgeSession>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut entries = sessions.values().cloned().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.bridge_session_id.cmp(&right.bridge_session_id));

    let body = entries
        .into_iter()
        .map(|session| serde_json::to_string(&session))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");

    fs::write(path, format!("{body}\n")).await?;
    Ok(())
}
