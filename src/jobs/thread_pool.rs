use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadReuseConfig {
    pub enabled: bool,
    pub idle_timeout: Duration,
    pub max_turns: u64,
}

impl ThreadReuseConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var("CLAUDE_CODEX_PROXY_ENABLE_THREAD_REUSE")
                .ok()
                .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
                .unwrap_or(false),
            idle_timeout: std::env::var("CLAUDE_CODEX_PROXY_THREAD_IDLE_SECS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(1800)),
            max_turns: std::env::var("CLAUDE_CODEX_PROXY_THREAD_MAX_TURNS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(8),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadLease {
    pub thread_id: String,
}

#[derive(Debug, Clone)]
struct ManagedThread {
    thread_id: String,
    admitted_at: Instant,
    last_completed_at: Option<Instant>,
    turn_count: u64,
    busy: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadPool {
    threads: Arc<RwLock<HashMap<String, ManagedThread>>>,
}

impl ThreadPool {
    pub async fn register_admitted(&self, thread_id: &str, now: Instant) {
        self.threads.write().await.insert(
            thread_id.to_string(),
            ManagedThread {
                thread_id: thread_id.to_string(),
                admitted_at: now,
                last_completed_at: None,
                turn_count: 0,
                busy: false,
            },
        );
    }

    pub async fn checkout(
        &self,
        thread_id: &str,
        now: Instant,
        config: &ThreadReuseConfig,
    ) -> Option<ThreadLease> {
        if !config.enabled {
            return None;
        }

        let mut guard = self.threads.write().await;
        let managed = guard.get_mut(thread_id)?;
        let last_activity = managed.last_completed_at.unwrap_or(managed.admitted_at);
        let expired = now.saturating_duration_since(last_activity) > config.idle_timeout;
        let exhausted = managed.turn_count >= config.max_turns;

        if managed.busy || expired || exhausted {
            if expired || exhausted {
                guard.remove(thread_id);
            }
            return None;
        }

        managed.busy = true;
        Some(ThreadLease {
            thread_id: managed.thread_id.clone(),
        })
    }

    pub async fn release(&self, lease: &ThreadLease, completed: bool, now: Instant) {
        if let Some(managed) = self.threads.write().await.get_mut(&lease.thread_id) {
            managed.busy = false;
            if completed {
                managed.turn_count += 1;
                managed.last_completed_at = Some(now);
            }
        }
    }

    pub async fn invalidate(&self, thread_id: &str) {
        self.threads.write().await.remove(thread_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn admitted_thread_can_be_checked_out_and_released() {
        let pool = ThreadPool::default();
        let config = ThreadReuseConfig {
            enabled: true,
            idle_timeout: Duration::from_secs(600),
            max_turns: 8,
        };
        let now = Instant::now();

        pool.register_admitted("thread-1", now).await;
        let lease = pool
            .checkout("thread-1", now, &config)
            .await
            .expect("lease");
        assert_eq!(lease.thread_id, "thread-1");

        pool.release(&lease, true, now + Duration::from_secs(1))
            .await;

        let second = pool
            .checkout("thread-1", now + Duration::from_secs(2), &config)
            .await
            .expect("second lease");
        assert_eq!(second.thread_id, "thread-1");
    }

    #[tokio::test]
    async fn busy_thread_is_not_checked_out_twice() {
        let pool = ThreadPool::default();
        let config = ThreadReuseConfig {
            enabled: true,
            idle_timeout: Duration::from_secs(600),
            max_turns: 8,
        };
        let now = Instant::now();

        pool.register_admitted("thread-1", now).await;
        let _lease = pool
            .checkout("thread-1", now, &config)
            .await
            .expect("lease");
        assert!(pool.checkout("thread-1", now, &config).await.is_none());
    }

    #[tokio::test]
    async fn expired_thread_is_not_reused() {
        let pool = ThreadPool::default();
        let config = ThreadReuseConfig {
            enabled: true,
            idle_timeout: Duration::from_secs(5),
            max_turns: 8,
        };
        let now = Instant::now();

        pool.register_admitted("thread-1", now).await;
        assert!(pool
            .checkout("thread-1", now + Duration::from_secs(10), &config)
            .await
            .is_none());
    }
}
