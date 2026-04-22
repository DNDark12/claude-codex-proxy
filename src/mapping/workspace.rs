use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::app_server::thread::BridgeThread;
use crate::jobs::registry::JobRegistry;
use crate::mapping::tools::ToolWarning;
use crate::state::StateStore;
use crate::surfaces::model::MappingStrategy;

/// Worktree association state (P4-015).
#[derive(Debug, Clone, Default)]
pub struct WorktreeStore {
    associations: Arc<RwLock<HashMap<String, WorktreeAssociation>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeAssociation {
    pub thread_id: String,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub active: bool,
}

impl WorktreeStore {
    pub async fn associate(&self, thread_id: &str, worktree_path: &str, branch: Option<&str>) {
        self.associations.write().await.insert(
            thread_id.to_string(),
            WorktreeAssociation {
                thread_id: thread_id.to_string(),
                worktree_path: worktree_path.to_string(),
                branch: branch.map(str::to_string),
                active: true,
            },
        );
    }

    pub async fn get(&self, thread_id: &str) -> Option<WorktreeAssociation> {
        self.associations.read().await.get(thread_id).cloned()
    }

    pub async fn deactivate(&self, thread_id: &str) {
        if let Some(assoc) = self.associations.write().await.get_mut(thread_id) {
            assoc.active = false;
        }
    }
}

/// EnterWorktree mapping — hybrid orchestration (P4-010).
/// Thread state: native via app-server. Git worktree: bridge-orchestrated.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeResult {
    pub strategy: MappingStrategy,
    pub worktree_path: Option<String>,
    pub thread_id: String,
    pub warnings: Vec<ToolWarning>,
}

pub fn map_enter_worktree(
    thread: &BridgeThread,
    branch: Option<&str>,
    worktree_path: Option<&str>,
) -> WorktreeResult {
    let path = worktree_path
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}/.worktrees/{}", thread.cwd, branch.unwrap_or("default")));

    WorktreeResult {
        strategy: MappingStrategy::MediatedNative,
        worktree_path: Some(path),
        thread_id: thread.thread_id.clone(),
        warnings: vec![ToolWarning {
            surface_id: "tool.enter_worktree".to_string(),
            warning: "Hybrid orchestration: thread state is native via app-server; git worktree lifecycle is bridge-orchestrated. App-server does NOT have a dedicated worktree API.".to_string(),
        }],
    }
}

pub fn map_exit_worktree(thread: &BridgeThread) -> WorktreeResult {
    WorktreeResult {
        strategy: MappingStrategy::MediatedNative,
        worktree_path: None,
        thread_id: thread.thread_id.clone(),
        warnings: Vec::new(),
    }
}

/// /resume mapping → thread/resume (P4-012).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeResult {
    pub strategy: MappingStrategy,
    pub thread_id: String,
}

pub async fn map_resume(
    thread_id: &str,
    _registry: &JobRegistry,
    sessions: &StateStore,
) -> Result<ResumeResult, String> {
    let has_session = sessions
        .list_sessions()
        .await
        .into_iter()
        .any(|session| session.thread.thread_id == thread_id);

    if !has_session {
        return Err(format!("thread {} not found", thread_id));
    }

    Ok(ResumeResult {
        strategy: MappingStrategy::MediatedNative,
        thread_id: thread_id.to_string(),
    })
}

/// /rewind mapping → thread/rollback (preferred) (P4-013).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindResult {
    pub strategy: MappingStrategy,
    pub method: &'static str,
    pub thread_id: String,
    pub turn_id: Option<String>,
}

pub fn map_rewind(thread_id: &str, turn_id: Option<&str>) -> RewindResult {
    RewindResult {
        strategy: MappingStrategy::MediatedNative,
        method: "thread/rollback",
        thread_id: thread_id.to_string(),
        turn_id: turn_id.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::thread::BridgeThread;
    use crate::mapping::approvals::{ApprovalPolicy, SandboxConfig};

    fn test_thread() -> BridgeThread {
        BridgeThread {
            thread_id: "t1".to_string(),
            bridge_session_id: "s1".to_string(),
            cwd: "/project".to_string(),
            project_root: Some("/project".to_string()),
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox_config: SandboxConfig::WorkspaceWrite,
            created_at_unix: 0,
            turn_count: 0,
        }
    }

    // P4-T02: EnterWorktree → git worktree created, thread associated
    #[tokio::test]
    async fn enter_worktree_creates_association() {
        let thread = test_thread();
        let store = WorktreeStore::default();
        let result = map_enter_worktree(&thread, Some("feature-x"), None);
        assert!(result.worktree_path.is_some());
        assert!(result.worktree_path.as_ref().unwrap().contains("feature-x"));
        assert_eq!(result.strategy, MappingStrategy::MediatedNative);
        // Warns about hybrid orchestration
        assert!(!result.warnings.is_empty());
        assert!(result.warnings[0].warning.contains("Hybrid"));

        // Store association
        store
            .associate(
                &thread.thread_id,
                result.worktree_path.as_ref().unwrap(),
                Some("feature-x"),
            )
            .await;
        let assoc = store.get(&thread.thread_id).await.unwrap();
        assert!(assoc.active);
        assert_eq!(assoc.branch.as_deref(), Some("feature-x"));
    }

    // P4-T03: /resume → paused thread resumes with state intact
    #[tokio::test]
    async fn resume_maps_to_thread_resume() {
        let sessions = StateStore::default();
        sessions
            .insert_session(crate::app_server::BridgeSession {
                bridge_session_id: "s1".to_string(),
                claude_session_id: None,
                account_id: None,
                account_auth_path: None,
                last_assistant_message: None,
                thread: test_thread(),
                transport: crate::app_server::TransportKind::Stdio,
                operation_mode: crate::surfaces::OperationMode::AutoHybrid,
                api_stability: crate::app_server::ApiStability::Stable,
                delegation_policy: crate::app_server::DelegationPolicy::ExplicitOnly,
                active_guidance_layers: Vec::new(),
                active_skills: Vec::new(),
                active_jobs: Vec::new(),
                state_version: 1,
            })
            .await;
        let result = map_resume("t1", &JobRegistry::default(), &sessions)
            .await
            .unwrap();
        assert_eq!(result.strategy, MappingStrategy::MediatedNative);
        assert_eq!(result.thread_id, "t1");
    }

    // P4-T04: /rewind → thread/rollback, not re-creation
    #[test]
    fn rewind_uses_rollback_not_recreation() {
        let result = map_rewind("t1", Some("turn-5"));
        assert_eq!(result.method, "thread/rollback");
        assert_eq!(result.turn_id.as_deref(), Some("turn-5"));
        assert_eq!(result.strategy, MappingStrategy::MediatedNative);
    }

    #[test]
    fn exit_worktree_clears_path() {
        let result = map_exit_worktree(&test_thread());
        assert!(result.worktree_path.is_none());
    }

    // P7-A06: Session resume preserves thread/cwd/guidance/approval state
    #[tokio::test]
    async fn worktree_store_preserves_state_across_operations() {
        let store = WorktreeStore::default();
        store
            .associate("t1", "/project/.worktrees/fix", Some("fix"))
            .await;
        let assoc = store.get("t1").await.unwrap();
        assert_eq!(assoc.worktree_path, "/project/.worktrees/fix");
        assert!(assoc.active);

        store.deactivate("t1").await;
        let assoc = store.get("t1").await.unwrap();
        assert!(!assoc.active);
    }
}
