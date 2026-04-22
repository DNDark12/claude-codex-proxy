use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::app_server::session::BridgeSession;

#[derive(Debug, Clone)]
pub struct StateStore {
    sessions: Arc<RwLock<HashMap<String, BridgeSession>>>,
    jsonl_path: Option<PathBuf>,
}

impl Default for StateStore {
    fn default() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            jsonl_path: None,
        }
    }
}

impl StateStore {
    pub async fn with_jsonl_path(path: PathBuf) -> Result<Self> {
        let sessions = crate::state::persistence::load_sessions(&path).await?;
        Ok(Self {
            sessions: Arc::new(RwLock::new(sessions)),
            jsonl_path: Some(path),
        })
    }

    async fn persist(&self) {
        if let Some(path) = &self.jsonl_path {
            let snapshot = self.sessions.read().await.clone();
            let _ = crate::state::persistence::save_sessions(path, &snapshot).await;
        }
    }

    pub async fn insert_session(&self, session: BridgeSession) {
        let mut sessions = self.sessions.write().await;
        sessions.insert(session.bridge_session_id.clone(), session);

        let limit = std::env::var("CLAUDE_CODEX_PROXY_MAX_SESSIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(256);

        if sessions.len() > limit {
            let mut ordered = sessions
                .iter()
                .map(|(id, session)| (id.clone(), session.thread.created_at_unix))
                .collect::<Vec<_>>();
            ordered.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));

            for (session_id, _) in ordered.into_iter().take(sessions.len() - limit) {
                sessions.remove(&session_id);
            }
        }
        drop(sessions);
        self.persist().await;
    }

    pub async fn get_session(&self, session_id: &str) -> Option<BridgeSession> {
        self.sessions.read().await.get(session_id).cloned()
    }

    pub async fn list_sessions(&self) -> Vec<BridgeSession> {
        let mut sessions: Vec<_> = self.sessions.read().await.values().cloned().collect();
        sessions.sort_by(|a, b| a.bridge_session_id.cmp(&b.bridge_session_id));
        sessions
    }

    pub async fn remove_session(&self, session_id: &str) -> Option<BridgeSession> {
        let removed = self.sessions.write().await.remove(session_id);
        if removed.is_some() {
            self.persist().await;
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::{
        ApiStability, BridgeSession, BridgeThread, DelegationPolicy, TransportKind,
    };
    use crate::mapping::approvals::{ApprovalPolicy, SandboxConfig};
    use crate::surfaces::OperationMode;

    #[tokio::test]
    async fn store_reloads_sessions_from_disk() {
        let path = std::env::temp_dir().join(format!("sessions-{}.jsonl", uuid::Uuid::new_v4()));
        let _ = tokio::fs::remove_file(&path).await;

        let store = StateStore::with_jsonl_path(path.clone()).await.unwrap();
        store
            .insert_session(BridgeSession {
                bridge_session_id: "session-1".to_string(),
                claude_session_id: None,
                account_id: None,
                account_auth_path: None,
                last_assistant_message: None,
                thread: BridgeThread {
                    thread_id: "thread-1".to_string(),
                    bridge_session_id: "session-1".to_string(),
                    cwd: "/tmp/project".to_string(),
                    project_root: Some("/tmp/project".to_string()),
                    approval_policy: ApprovalPolicy::OnRequest,
                    sandbox_config: SandboxConfig::WorkspaceWrite,
                    created_at_unix: 123,
                    turn_count: 2,
                },
                transport: TransportKind::Stdio,
                operation_mode: OperationMode::AutoHybrid,
                api_stability: ApiStability::Stable,
                delegation_policy: DelegationPolicy::ExplicitOnly,
                active_guidance_layers: vec!["base".to_string()],
                active_skills: vec!["review".to_string()],
                active_jobs: vec!["job-1".to_string()],
                state_version: 1,
            })
            .await;

        let reloaded = StateStore::with_jsonl_path(path.clone()).await.unwrap();
        let session = reloaded.get_session("session-1").await.unwrap();
        assert_eq!(session.thread.thread_id, "thread-1");
        assert_eq!(session.active_jobs, vec!["job-1".to_string()]);

        let _ = tokio::fs::remove_file(path).await;
    }
}
