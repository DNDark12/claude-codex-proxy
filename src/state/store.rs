use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::app_server::session::BridgeSession;

#[derive(Debug, Clone, Default)]
pub struct StateStore {
    sessions: Arc<RwLock<HashMap<String, BridgeSession>>>,
}

impl StateStore {
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
            ordered.sort_by(|left, right| {
                left.1
                    .cmp(&right.1)
                    .then_with(|| left.0.cmp(&right.0))
            });

            for (session_id, _) in ordered.into_iter().take(sessions.len() - limit) {
                sessions.remove(&session_id);
            }
        }
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
        self.sessions.write().await.remove(session_id)
    }
}
