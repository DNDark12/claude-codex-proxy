// src/accounts/mod.rs
//
// Multi-account pool với round-robin rotation và health tracking.
//
// Config: accounts.toml (hoặc ACCOUNTS_CONFIG_PATH env var)
// Format xem accounts.toml.example
//
// Admin API (mount trong routes/api.rs):
//   GET  /api/accounts            → list all accounts + stats
//   POST /api/accounts            → add account at runtime
//   DELETE /api/accounts/:id      → remove account
//   PATCH /api/accounts/:id/toggle → enable/disable

pub mod auth_store;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use self::auth_store::{
    discover_codex_accounts, parse_jwt_from_auth_file, JwtMetadata, PlanType, QuotaHealth,
};

// ─── Config structs (deserialized from accounts.toml) ────────────────────────

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AccountsConfig {
    pub account: Vec<AccountConfig>,
    #[serde(default)]
    pub pool: PoolConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AccountConfig {
    /// Unique id, e.g. "account_1"
    pub id: String,
    /// Human label shown in UI
    pub label: Option<String>,
    /// Path to auth.json for this account
    pub auth_path: String,
    /// Whether this account starts enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PoolConfig {
    /// Error count threshold before marking account degraded
    #[serde(default = "default_error_threshold")]
    pub error_threshold: u32,
    /// How long (seconds) a degraded account stays in cooldown before retry
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            error_threshold: 3,
            cooldown_secs: 120,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_error_threshold() -> u32 {
    3
}
fn default_cooldown_secs() -> u64 {
    120
}

const MAX_RATE_LIMIT_TTL_SECS: u64 = 6 * 60 * 60;

fn default_accounts_config_path() -> PathBuf {
    std::env::var("ACCOUNTS_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("accounts.toml"))
}

// ─── Runtime state (in-memory, not persisted) ─────────────────────────────────

#[derive(Debug)]
struct AccountRuntimeState {
    // Health tracking
    total_requests: u64,
    error_count: u32,
    consecutive_errors: u32,
    last_used: Option<Instant>,
    last_error: Option<String>,
    degraded_until: Option<Instant>,

    // Rate-limit tracking (NEW)
    rate_limited_until: Option<Instant>,
    last_rate_limit_reset_secs: Option<u64>,
    #[allow(dead_code)]
    last_rate_limited_at: Option<Instant>,

    // Auth metadata from JWT (best-effort)
    plan_type: PlanType,
    email: Option<String>,
    token_expires_at: Option<u64>,
    #[allow(dead_code)]
    jwt_account_id: Option<String>,
    last_refresh_at: Option<i64>,
    auth_modified_at: Option<i64>,
    last_metadata_refresh: Option<Instant>,
    auth_fingerprint: Option<u64>,
}

impl AccountRuntimeState {
    fn new() -> Self {
        Self {
            total_requests: 0,
            error_count: 0,
            consecutive_errors: 0,
            last_used: None,
            last_error: None,
            degraded_until: None,
            rate_limited_until: None,
            last_rate_limit_reset_secs: None,
            last_rate_limited_at: None,
            plan_type: PlanType::Unknown,
            email: None,
            token_expires_at: None,
            jwt_account_id: None,
            last_refresh_at: None,
            auth_modified_at: None,
            last_metadata_refresh: None,
            auth_fingerprint: None,
        }
    }

    fn apply_jwt_metadata(&mut self, meta: JwtMetadata) {
        self.plan_type = meta.plan_type;
        self.email = meta.email;
        self.token_expires_at = meta.exp;
        self.jwt_account_id = meta.account_id;
        self.last_refresh_at = meta.last_refresh_at;
        self.last_metadata_refresh = Some(Instant::now());
    }

    fn is_rate_limited(&self, now: Instant) -> bool {
        self.rate_limited_until
            .map(|until| now < until)
            .unwrap_or(false)
    }

    fn is_degraded(&self, now: Instant) -> bool {
        self.degraded_until
            .map(|until| now < until)
            .unwrap_or(false)
    }

    fn is_token_expired(&self) -> bool {
        if let Some(exp) = self.token_expires_at {
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            exp < now_unix
        } else {
            false
        }
    }

    fn metadata_is_stale(&self) -> bool {
        self.last_metadata_refresh
            .map(|t| t.elapsed() > Duration::from_secs(3600))
            .unwrap_or(true)
    }
}

// ─── AccountEntry (config + runtime state) ────────────────────────────────────

#[derive(Debug)]
struct AccountEntry {
    config: AccountConfig,
    label: String,
    auth_path: PathBuf,
    state: AccountRuntimeState,
}

impl AccountEntry {
    fn from_config(cfg: &AccountConfig) -> Self {
        let auth_path = PathBuf::from(shellexpand::tilde(&cfg.auth_path).into_owned());
        let mut state = AccountRuntimeState::new();

        // Best-effort JWT metadata parse
        let meta = parse_jwt_from_auth_file(&auth_path);
        state.apply_jwt_metadata(meta);
        state.auth_fingerprint = auth_store::auth_file_fingerprint(&auth_path);
        state.auth_modified_at = auth_store::auth_file_modified_unix(&auth_path);

        Self {
            config: cfg.clone(),
            label: cfg.label.clone().unwrap_or_else(|| cfg.id.clone()),
            auth_path,
            state,
        }
    }

    fn refresh_auth_state_from_disk(&mut self) -> bool {
        let previous_fingerprint = self.state.auth_fingerprint;
        let next_fingerprint = auth_store::auth_file_fingerprint(&self.auth_path);
        let changed = next_fingerprint.is_some() && next_fingerprint != previous_fingerprint;

        self.state
            .apply_jwt_metadata(parse_jwt_from_auth_file(&self.auth_path));
        self.state.auth_fingerprint = next_fingerprint;
        self.state.auth_modified_at = auth_store::auth_file_modified_unix(&self.auth_path);
        changed
    }

    fn clear_runtime_penalties(&mut self) {
        self.state.error_count = 0;
        self.state.consecutive_errors = 0;
        self.state.last_error = None;
        self.state.degraded_until = None;
        self.state.rate_limited_until = None;
        self.state.last_rate_limit_reset_secs = None;
        self.state.last_rate_limited_at = None;
    }

    /// Account is fully available for request dispatch.
    fn is_fully_available(&self, now: Instant) -> bool {
        if !self.config.enabled {
            return false;
        }
        if self.state.is_token_expired() {
            return false;
        }
        if self.state.is_rate_limited(now) {
            return false;
        }
        if self.state.is_degraded(now) {
            return false;
        }
        true
    }

    fn identity_key(&self) -> Option<String> {
        identity_key(
            self.state.jwt_account_id.as_deref(),
            self.state.email.as_deref(),
        )
    }

    fn freshness_key(&self) -> (i64, i64, String) {
        (
            self.state
                .last_refresh_at
                .or(self.state.auth_modified_at)
                .unwrap_or_default(),
            self.state
                .auth_modified_at
                .or(self.state.last_refresh_at)
                .unwrap_or_default(),
            self.auth_path.display().to_string(),
        )
    }

    fn quota_health(&self, now: Instant) -> QuotaHealth {
        if !self.config.enabled {
            return QuotaHealth::Disabled;
        }
        if self.state.is_rate_limited(now) {
            return QuotaHealth::RateLimited;
        }
        if self.state.is_degraded(now) {
            return QuotaHealth::CoolingDown;
        }
        if self.state.is_token_expired() {
            return QuotaHealth::AuthExpired;
        }
        QuotaHealth::Available
    }

    fn to_status(&self, now: Instant) -> AccountStatus {
        let health = self.quota_health(now);

        let rate_limit_remaining_secs = if self.state.is_rate_limited(now) {
            self.state
                .rate_limited_until
                .map(|u| u.duration_since(now).as_secs())
        } else {
            None
        };

        let cooldown_remaining_secs = if self.state.is_degraded(now) {
            self.state
                .degraded_until
                .map(|u| u.duration_since(now).as_secs())
        } else {
            None
        };

        AccountStatus {
            enabled: self.config.enabled,
            plan_type: self.state.plan_type.clone(),
            email: self.state.email.clone(),
            health,
            total_requests: self.state.total_requests,
            error_count: self.state.error_count,
            consecutive_errors: self.state.consecutive_errors,
            last_used_secs_ago: self
                .state
                .last_used
                .map(|t| now.duration_since(t).as_secs()),
            rate_limit_remaining_secs,
            cooldown_remaining_secs,
            token_expires_at: self.state.token_expires_at,
            last_error: self.state.last_error.clone(),
        }
    }
}

fn identity_key(account_id: Option<&str>, email: Option<&str>) -> Option<String> {
    account_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("account:{value}"))
        .or_else(|| {
            email
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| format!("email:{}", value.to_ascii_lowercase()))
        })
}

fn discovered_identity_key(discovered: &auth_store::DiscoveredAccount) -> Option<String> {
    identity_key(
        discovered.account_id.as_deref(),
        discovered.email.as_deref(),
    )
}

fn discovered_freshness_key(discovered: &auth_store::DiscoveredAccount) -> (i64, i64, String) {
    (
        discovered
            .last_refresh_at
            .or(discovered.auth_modified_at)
            .unwrap_or_default(),
        discovered
            .auth_modified_at
            .or(discovered.last_refresh_at)
            .unwrap_or_default(),
        discovered.auth_path.clone(),
    )
}

fn has_better_identity_candidate(
    accounts: &[AccountEntry],
    current_idx: usize,
    now: Instant,
) -> bool {
    let Some(identity) = accounts[current_idx].identity_key() else {
        return false;
    };
    let current_freshness = accounts[current_idx].freshness_key();

    accounts.iter().enumerate().any(|(other_idx, other)| {
        other_idx != current_idx
            && other.is_fully_available(now)
            && other.identity_key().as_deref() == Some(identity.as_str())
            && other.freshness_key() > current_freshness
    })
}

fn build_synced_info(entry: &AccountEntry, source: String, action: &str) -> SyncedAccountInfo {
    SyncedAccountInfo {
        id: entry.config.id.clone(),
        label: entry.label.clone(),
        auth_path: entry.auth_path.display().to_string(),
        source,
        action: action.to_string(),
        email: entry.state.email.clone(),
        plan_type: entry.state.plan_type.clone(),
        token_expired: entry.state.is_token_expired(),
    }
}

fn mirror_auth_file(source: &Path, dest: &Path) -> anyhow::Result<bool> {
    let source_bytes = std::fs::read(source)?;
    if std::fs::read(dest).ok().as_deref() == Some(source_bytes.as_slice()) {
        return Ok(false);
    }

    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let tmp = dest.with_extension("json.tmp");
    std::fs::write(&tmp, &source_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, dest)?;
    Ok(true)
}

// ─── AccountStatus (API/UI response — fully serializable) ─────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct AccountStatus {
    pub enabled: bool,
    pub plan_type: PlanType,
    pub email: Option<String>,
    pub health: QuotaHealth,
    pub total_requests: u64,
    pub error_count: u32,
    pub consecutive_errors: u32,
    pub last_used_secs_ago: Option<u64>,
    pub rate_limit_remaining_secs: Option<u64>,
    pub cooldown_remaining_secs: Option<u64>,
    pub token_expires_at: Option<u64>,
    pub last_error: Option<String>,
}

// ─── Public account info (for API/UI) ────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct AccountInfo {
    pub id: String,
    pub label: String,
    pub auth_path: String,
    pub status: AccountStatus,
}

#[derive(Debug, Serialize, Clone)]
pub struct SyncedAccountInfo {
    pub id: String,
    pub label: String,
    pub auth_path: String,
    pub source: String,
    pub action: String,
    pub email: Option<String>,
    pub plan_type: PlanType,
    pub token_expired: bool,
}

// ─── AccountPool ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct AccountPool {
    accounts: RwLock<Vec<AccountEntry>>,
    counter: AtomicUsize,
    config: PoolConfig,
    config_path: Option<PathBuf>,
}

impl AccountPool {
    fn from_entries(
        entries: Vec<AccountEntry>,
        config: PoolConfig,
        config_path: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            accounts: RwLock::new(entries),
            counter: AtomicUsize::new(0),
            config,
            config_path,
        })
    }

    async fn persist_config(&self) -> anyhow::Result<()> {
        let Some(path) = self.config_path.as_ref() else {
            return Ok(());
        };
        let accounts = self.accounts.read().await;
        let snapshot = AccountsConfig {
            account: accounts.iter().map(|entry| entry.config.clone()).collect(),
            pool: self.config.clone(),
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let content = toml::to_string_pretty(&snapshot)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    fn next_generated_id(entries: &[AccountEntry]) -> String {
        let mut index = 1usize;
        loop {
            let candidate = if index == 1 {
                "default".to_string()
            } else {
                format!("account_{index}")
            };
            if !entries.iter().any(|entry| entry.config.id == candidate) {
                return candidate;
            }
            index += 1;
        }
    }

    /// Load từ accounts.toml. Trả lỗi nếu file không tồn tại hoặc parse fail.
    pub fn from_config_file(path: &str) -> anyhow::Result<Arc<Self>> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Cannot read accounts config '{}': {}", path, e))?;
        let cfg: AccountsConfig = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Invalid accounts.toml: {}", e))?;

        let entries = cfg.account.iter().map(AccountEntry::from_config).collect();
        log::info!(
            "[account_pool] Loaded {} account(s) from {}",
            cfg.account.len(),
            path
        );

        Ok(Self::from_entries(
            entries,
            cfg.pool,
            Some(PathBuf::from(path)),
        ))
    }

    /// Fallback: tạo pool với 1 account từ PROXY_AUTH_PATH (backward compat)
    pub fn single(auth_path: &str) -> Arc<Self> {
        let cfg = AccountConfig {
            id: "default".to_string(),
            label: Some("Default".to_string()),
            auth_path: auth_path.to_string(),
            enabled: true,
        };
        let entry = AccountEntry::from_config(&cfg);
        log::info!(
            "[account_pool] Single account: plan={}, email={}",
            entry.state.plan_type,
            entry.state.email.as_deref().unwrap_or("unknown")
        );
        Self::from_entries(
            vec![entry],
            PoolConfig::default(),
            Some(default_accounts_config_path()),
        )
    }

    /// Trả auth_path của account tiếp theo theo round-robin.
    /// Bỏ qua account disabled, đang cooldown, hoặc đang rate-limited.
    /// Trả None nếu không có account nào khả dụng (RC-1: caller returns 503).
    pub async fn next_auth_path(&self) -> Option<PathBuf> {
        let mut accounts = self.accounts.write().await;
        let now = Instant::now();
        let len = accounts.len();
        if len == 0 {
            return None;
        }

        // Tìm account fully available kế tiếp theo round-robin
        let start = self.counter.fetch_add(1, Ordering::Relaxed) % len;
        for i in 0..len {
            let idx = (start + i) % len;
            if accounts[idx].is_fully_available(now)
                && !has_better_identity_candidate(&accounts, idx, now)
            {
                accounts[idx].state.total_requests += 1;
                accounts[idx].state.last_used = Some(now);
                // Reset degraded state nếu đã hết cooldown
                if accounts[idx]
                    .state
                    .degraded_until
                    .map(|u| u <= now)
                    .unwrap_or(false)
                {
                    accounts[idx].state.degraded_until = None;
                    accounts[idx].state.consecutive_errors = 0;
                }
                // Reset rate-limit state nếu đã hết
                if accounts[idx]
                    .state
                    .rate_limited_until
                    .map(|u| u <= now)
                    .unwrap_or(false)
                {
                    accounts[idx].state.rate_limited_until = None;
                }
                let path = accounts[idx].auth_path.clone();
                log::debug!(
                    "[account_pool] Dispatching to account '{}' (idx {})",
                    accounts[idx].config.id,
                    idx
                );
                return Some(path);
            }
        }

        // RC-1: ALL accounts limited/cooldown/disabled → return None
        log::warn!("[account_pool] No available accounts — all disabled, limited, or in cooldown");
        None
    }

    /// Prefer a specific auth_path when it is still available; otherwise fall back
    /// to the normal round-robin candidate selection.
    pub async fn preferred_auth_path(&self, preferred: Option<&Path>) -> Option<PathBuf> {
        let mut accounts = self.accounts.write().await;
        let now = Instant::now();

        if let Some(preferred) = preferred {
            if let Some(idx) = accounts
                .iter()
                .position(|entry| entry.auth_path == preferred && entry.is_fully_available(now))
            {
                if has_better_identity_candidate(&accounts, idx, now) {
                    drop(accounts);
                    return self.next_auth_path().await;
                }
                let entry = &mut accounts[idx];
                entry.state.total_requests += 1;
                entry.state.last_used = Some(now);
                if entry
                    .state
                    .degraded_until
                    .map(|until| until <= now)
                    .unwrap_or(false)
                {
                    entry.state.degraded_until = None;
                    entry.state.consecutive_errors = 0;
                }
                if entry
                    .state
                    .rate_limited_until
                    .map(|until| until <= now)
                    .unwrap_or(false)
                {
                    entry.state.rate_limited_until = None;
                }
                return Some(entry.auth_path.clone());
            }
        }

        drop(accounts);
        self.next_auth_path().await
    }

    /// Soonest reset time across all enabled accounts (for 503 Retry-After).
    pub async fn soonest_reset_secs(&self) -> Option<u64> {
        let accounts = self.accounts.read().await;
        let now = Instant::now();
        accounts
            .iter()
            .filter(|a| a.config.enabled)
            .filter_map(|a| {
                let deadline = a.state.rate_limited_until.or(a.state.degraded_until)?;
                if deadline > now {
                    Some(deadline.duration_since(now).as_secs())
                } else {
                    Some(0)
                }
            })
            .min()
    }

    /// Báo cáo request thành công + lazy JWT metadata refresh
    pub async fn report_success(&self, auth_path: &PathBuf) {
        let mut accounts = self.accounts.write().await;
        if let Some(entry) = accounts.iter_mut().find(|a| &a.auth_path == auth_path) {
            entry.state.consecutive_errors = 0;
            entry.state.last_error = None;
            entry.state.degraded_until = None;
            entry.state.rate_limited_until = None;

            // Lazy refresh JWT metadata if stale (>1h)
            if entry.state.metadata_is_stale() {
                let _ = entry.refresh_auth_state_from_disk();
                log::debug!(
                    "[account_pool] Refreshed JWT metadata for '{}'",
                    entry.config.id
                );
            }
        }
    }

    /// Báo cáo lỗi cho account → có thể trigger degraded cooldown
    pub async fn report_error(&self, auth_path: &PathBuf, error: &str) {
        let mut accounts = self.accounts.write().await;
        let threshold = self.config.error_threshold;
        let cooldown = Duration::from_secs(self.config.cooldown_secs);

        if let Some(entry) = accounts.iter_mut().find(|a| &a.auth_path == auth_path) {
            entry.state.error_count += 1;
            entry.state.consecutive_errors += 1;
            entry.state.last_error = Some(error.chars().take(200).collect());

            if let Some(cooldown_secs) = infer_auth_failure_cooldown_secs(error) {
                let ttl = Duration::from_secs(cooldown_secs.min(MAX_RATE_LIMIT_TTL_SECS));
                entry.state.degraded_until = Some(Instant::now() + ttl);
                entry.state.consecutive_errors = 0;
                log::warn!(
                    "[account_pool] Account '{}' marked unavailable after auth failure. Holding for {}s.",
                    entry.config.id,
                    cooldown_secs
                );
                return;
            }

            if let Some(resets_in_secs) = infer_rate_limit_reset_secs(error) {
                let ttl = Duration::from_secs(resets_in_secs.min(MAX_RATE_LIMIT_TTL_SECS));
                entry.state.rate_limited_until = Some(Instant::now() + ttl);
                entry.state.last_rate_limit_reset_secs = Some(resets_in_secs);
                entry.state.last_rate_limited_at = Some(Instant::now());
                entry.state.consecutive_errors = 0;
                entry.state.degraded_until = None;
                log::warn!(
                    "[account_pool] Account '{}' detected quota/rate-limit from error payload. Holding for {}s.",
                    entry.config.id,
                    resets_in_secs
                );
                return;
            }

            if entry.state.consecutive_errors >= threshold {
                let until = Instant::now() + cooldown;
                entry.state.degraded_until = Some(until);
                log::warn!(
                    "[account_pool] Account '{}' degraded after {} consecutive errors. Cooldown {}s.",
                    entry.config.id,
                    entry.state.consecutive_errors,
                    cooldown.as_secs()
                );
            }
        }
    }

    /// Report rate-limit (429) for a specific account.
    pub async fn report_rate_limit(&self, auth_path: &PathBuf, resets_in_secs: u64) {
        let mut accounts = self.accounts.write().await;
        if let Some(entry) = accounts.iter_mut().find(|a| &a.auth_path == auth_path) {
            let ttl = Duration::from_secs(resets_in_secs.min(MAX_RATE_LIMIT_TTL_SECS));
            entry.state.rate_limited_until = Some(Instant::now() + ttl);
            entry.state.last_rate_limit_reset_secs = Some(resets_in_secs);
            entry.state.last_rate_limited_at = Some(Instant::now());
            log::warn!(
                "[account_pool] Account '{}' rate-limited for {}s",
                entry.config.id,
                resets_in_secs
            );
        }
    }

    /// List all accounts với status hiện tại
    pub async fn list(&self) -> Vec<AccountInfo> {
        let accounts = self.accounts.read().await;
        let now = Instant::now();
        accounts
            .iter()
            .map(|e| AccountInfo {
                id: e.config.id.clone(),
                label: e.label.clone(),
                auth_path: e.auth_path.to_string_lossy().to_string(),
                status: e.to_status(now),
            })
            .collect()
    }

    pub async fn account_id_for_auth_path(&self, path: &Path) -> Option<String> {
        let accounts = self.accounts.read().await;
        accounts
            .iter()
            .find(|entry| entry.auth_path == path)
            .map(|entry| entry.config.id.clone())
    }

    /// Add account tại runtime. Rejects duplicate id or auth_path.
    pub async fn add(&self, cfg: AccountConfig) -> anyhow::Result<()> {
        auth_store::sanitize_account_id(&cfg.id).map_err(|e| anyhow::anyhow!(e))?;

        let mut accounts = self.accounts.write().await;
        if accounts.iter().any(|a| a.config.id == cfg.id) {
            anyhow::bail!("Account id '{}' already exists", cfg.id);
        }

        let expanded_path = PathBuf::from(shellexpand::tilde(&cfg.auth_path).into_owned());
        if accounts.iter().any(|a| a.auth_path == expanded_path) {
            anyhow::bail!(
                "Auth path '{}' already in use by another account",
                cfg.auth_path
            );
        }

        let entry = AccountEntry::from_config(&cfg);
        log::info!(
            "[account_pool] Added account '{}' (plan={}, email={})",
            cfg.id,
            entry.state.plan_type,
            entry.state.email.as_deref().unwrap_or("unknown")
        );
        accounts.push(entry);
        drop(accounts);
        self.persist_config().await?;
        Ok(())
    }

    /// Remove account theo id
    pub async fn remove(&self, id: &str) -> anyhow::Result<()> {
        let mut accounts = self.accounts.write().await;
        let before = accounts.len();
        accounts.retain(|a| a.config.id != id);
        if accounts.len() == before {
            anyhow::bail!("Account '{}' not found", id);
        }
        log::info!("[account_pool] Removed account '{}'", id);
        drop(accounts);
        self.persist_config().await?;
        Ok(())
    }

    /// Toggle enable/disable theo id
    pub async fn toggle(&self, id: &str) -> anyhow::Result<bool> {
        let mut accounts = self.accounts.write().await;
        let entry = accounts
            .iter_mut()
            .find(|a| a.config.id == id)
            .ok_or_else(|| anyhow::anyhow!("Account '{}' not found", id))?;
        entry.config.enabled = !entry.config.enabled;
        let enabled = entry.config.enabled;
        log::info!(
            "[account_pool] Account '{}' is now {}",
            id,
            if enabled { "enabled" } else { "disabled" }
        );
        drop(accounts);
        self.persist_config().await?;
        Ok(enabled)
    }

    /// Reset error/degraded/rate-limit state của account
    pub async fn reset(&self, id: &str) -> anyhow::Result<()> {
        let mut accounts = self.accounts.write().await;
        let entry = accounts
            .iter_mut()
            .find(|a| a.config.id == id)
            .ok_or_else(|| anyhow::anyhow!("Account '{}' not found", id))?;
        entry.state.error_count = 0;
        entry.state.consecutive_errors = 0;
        entry.state.degraded_until = None;
        entry.state.rate_limited_until = None;
        entry.state.last_rate_limited_at = None;
        entry.state.last_rate_limit_reset_secs = None;
        entry.state.last_error = None;

        // Re-parse JWT metadata on reset
        let meta = parse_jwt_from_auth_file(&entry.auth_path);
        entry.state.apply_jwt_metadata(meta);

        log::info!("[account_pool] Reset account '{}'", id);
        Ok(())
    }

    pub async fn sync_discovered(&self) -> anyhow::Result<Vec<SyncedAccountInfo>> {
        let discovered = discover_codex_accounts();
        let mut accounts = self.accounts.write().await;
        let mut synced = Vec::new();

        for discovered_account in discovered {
            let discovered_path = PathBuf::from(&discovered_account.auth_path);
            let source = discovered_account.source.clone();
            let discovered_identity = discovered_identity_key(&discovered_account);
            let discovered_freshness = discovered_freshness_key(&discovered_account);
            let mut matched_any = false;

            let matching_indices: Vec<usize> = accounts
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    entry.auth_path == discovered_path
                        || discovered_identity.as_ref().is_some_and(|identity| {
                            entry.identity_key().as_deref() == Some(identity.as_str())
                        })
                })
                .map(|(idx, _)| idx)
                .collect();

            for idx in matching_indices {
                matched_any = true;
                let existing = &mut accounts[idx];
                let previous_email = existing.state.email.clone();
                let previous_path = existing.auth_path.clone();
                let previous_freshness = existing.freshness_key();
                let mut mirrored = false;

                if existing.auth_path != discovered_path
                    && discovered_freshness > previous_freshness
                {
                    match mirror_auth_file(&discovered_path, &existing.auth_path) {
                        Ok(changed) => mirrored = changed,
                        Err(error) => log::warn!(
                            "[account_pool] Failed to mirror refreshed auth from '{}' to '{}': {}",
                            discovered_path.display(),
                            existing.auth_path.display(),
                            error
                        ),
                    }
                }

                let auth_changed = existing.refresh_auth_state_from_disk();
                let path_changed = existing.auth_path != previous_path;
                if mirrored || auth_changed || path_changed {
                    existing.clear_runtime_penalties();
                }

                let action = if mirrored {
                    "mirrored"
                } else if path_changed {
                    "relinked"
                } else if auth_changed || previous_email != existing.state.email {
                    "updated"
                } else {
                    "unchanged"
                };

                synced.push(build_synced_info(existing, source.clone(), action));
            }

            if matched_any {
                continue;
            }

            let generated_id = Self::next_generated_id(&accounts);
            let label = discovered_account
                .email
                .as_ref()
                .and_then(|email| email.split('@').next())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| generated_id.clone());
            let cfg = AccountConfig {
                id: generated_id.clone(),
                label: Some(label.clone()),
                auth_path: discovered_account.auth_path.clone(),
                enabled: !discovered_account.token_expired,
            };
            let entry = AccountEntry::from_config(&cfg);
            synced.push(build_synced_info(&entry, source, "added"));
            accounts.push(entry);
        }

        drop(accounts);
        self.persist_config().await?;
        Ok(synced)
    }

    /// Check if any auth_path is already in use
    pub async fn has_auth_path(&self, path: &str) -> bool {
        let expanded = PathBuf::from(shellexpand::tilde(path).into_owned());
        let accounts = self.accounts.read().await;
        accounts.iter().any(|a| a.auth_path == expanded)
    }

    pub fn available_count_sync(&self) -> usize {
        self.accounts
            .try_read()
            .map(|a| {
                let now = Instant::now();
                a.iter().filter(|e| e.is_fully_available(now)).count()
            })
            .unwrap_or(0)
    }
}

fn infer_rate_limit_reset_secs(error: &str) -> Option<u64> {
    let parsed = serde_json::from_str::<Value>(error).ok();
    if let Some(parsed) = parsed {
        if let Some(seconds) = parsed
            .get("error")
            .and_then(|value| value.get("resets_in_seconds"))
            .and_then(parse_seconds_value)
            .or_else(|| {
                parsed
                    .get("resets_in_seconds")
                    .and_then(parse_seconds_value)
            })
        {
            return Some(seconds);
        }
    }

    let lower = error.to_ascii_lowercase();
    let has_rate_limit_signal = ["rate limit", "too many requests", "quota", "exceeded"]
        .iter()
        .any(|signal| lower.contains(signal));
    if !has_rate_limit_signal {
        return None;
    }

    parse_human_duration_secs(&lower).or(Some(30 * 60))
}

fn infer_auth_failure_cooldown_secs(error: &str) -> Option<u64> {
    let lower = error.to_ascii_lowercase();
    let auth_signals = [
        "refresh token was already used",
        "access token could not be refreshed",
        "log out and sign in again",
        "failed to refresh token",
        "authentication failed",
        "invalid_grant",
    ];

    auth_signals
        .iter()
        .any(|signal| lower.contains(signal))
        .then_some(MAX_RATE_LIMIT_TTL_SECS)
}

fn parse_seconds_value(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
}

fn parse_human_duration_secs(text: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut found = false;
    let mut pending_number: Option<u64> = None;

    for raw in text.split_whitespace() {
        let token = raw
            .trim_matches(|c: char| !c.is_ascii_alphanumeric())
            .to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }

        if let Ok(number) = token.parse::<u64>() {
            pending_number = Some(number);
            continue;
        }

        let mut digits = String::new();
        let mut unit = String::new();
        for ch in token.chars() {
            if ch.is_ascii_digit() && unit.is_empty() {
                digits.push(ch);
            } else if ch.is_ascii_alphabetic() {
                unit.push(ch);
            }
        }

        if !digits.is_empty() && !unit.is_empty() {
            if let Ok(number) = digits.parse::<u64>() {
                if let Some(multiplier) = duration_unit_multiplier(&unit) {
                    total = total.saturating_add(number.saturating_mul(multiplier));
                    found = true;
                    pending_number = None;
                    continue;
                }
            }
        }

        if let Some(number) = pending_number {
            if let Some(multiplier) = duration_unit_multiplier(&token) {
                total = total.saturating_add(number.saturating_mul(multiplier));
                found = true;
                pending_number = None;
            }
        }
    }

    if found && total > 0 {
        Some(total)
    } else {
        None
    }
}

fn duration_unit_multiplier(unit: &str) -> Option<u64> {
    match unit {
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(60 * 60),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60),
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1),
        _ => None,
    }
}

// ─── Load helper ─────────────────────────────────────────────────────────────

/// Quyết định dùng pool từ file, auto-discovery, hay single-account fallback.
/// Priority: ACCOUNTS_CONFIG_PATH > accounts.toml > auto-discover > PROXY_AUTH_PATH
pub fn load_pool() -> Arc<AccountPool> {
    // 1. Explicit env var
    if let Ok(path) = std::env::var("ACCOUNTS_CONFIG_PATH") {
        match AccountPool::from_config_file(&path) {
            Ok(pool) => return pool,
            Err(e) => log::error!("[account_pool] Failed to load '{}': {}", path, e),
        }
    }

    // 2. Default accounts.toml in CWD
    if std::path::Path::new("accounts.toml").exists() {
        match AccountPool::from_config_file("accounts.toml") {
            Ok(pool) => return pool,
            Err(e) => {
                log::warn!(
                    "[account_pool] accounts.toml found but failed to parse: {}",
                    e
                )
            }
        }
    }

    // 3. Auto-discover from known Codex auth locations
    let discovered = discover_codex_accounts();
    if !discovered.is_empty() {
        let entries: Vec<AccountEntry> = discovered
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let id = if i == 0 {
                    "default".to_string()
                } else {
                    format!("account_{}", i + 1)
                };
                let label = d
                    .email
                    .as_ref()
                    .map(|e| e.split('@').next().unwrap_or(&id).to_string())
                    .unwrap_or_else(|| id.clone());
                let cfg = AccountConfig {
                    id,
                    label: Some(label),
                    auth_path: d.auth_path.clone(),
                    enabled: !d.token_expired,
                };
                AccountEntry::from_config(&cfg)
            })
            .collect();

        log::info!(
            "[account_pool] Auto-discovered {} account(s)",
            entries.len()
        );
        for e in &entries {
            log::info!(
                "[account_pool]   → '{}' plan={} email={} expired={}",
                e.config.id,
                e.state.plan_type,
                e.state.email.as_deref().unwrap_or("unknown"),
                e.state.is_token_expired()
            );
        }

        return AccountPool::from_entries(
            entries,
            PoolConfig::default(),
            Some(default_accounts_config_path()),
        );
    }

    // 4. Fallback: single account từ PROXY_AUTH_PATH
    let auth_path =
        std::env::var("PROXY_AUTH_PATH").unwrap_or_else(|_| "~/.codex/auth.json".to_string());
    log::info!(
        "[account_pool] Using single-account mode with auth_path={}",
        auth_path
    );
    AccountPool::single(&auth_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn test_pool() -> Arc<AccountPool> {
        Arc::new(AccountPool {
            accounts: RwLock::new(vec![
                AccountEntry {
                    config: AccountConfig {
                        id: "account_1".to_string(),
                        label: Some("Account 1".to_string()),
                        auth_path: "/tmp/account-1/auth.json".to_string(),
                        enabled: true,
                    },
                    label: "Account 1".to_string(),
                    auth_path: PathBuf::from("/tmp/account-1/auth.json"),
                    state: AccountRuntimeState::new(),
                },
                AccountEntry {
                    config: AccountConfig {
                        id: "account_2".to_string(),
                        label: Some("Account 2".to_string()),
                        auth_path: "/tmp/account-2/auth.json".to_string(),
                        enabled: true,
                    },
                    label: "Account 2".to_string(),
                    auth_path: PathBuf::from("/tmp/account-2/auth.json"),
                    state: AccountRuntimeState::new(),
                },
            ]),
            counter: AtomicUsize::new(0),
            config: PoolConfig::default(),
            config_path: None,
        })
    }

    fn base64_url_encode(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut output = String::new();
        let mut index = 0;
        while index < input.len() {
            let b0 = input[index];
            let b1 = if index + 1 < input.len() {
                input[index + 1]
            } else {
                0
            };
            let b2 = if index + 2 < input.len() {
                input[index + 2]
            } else {
                0
            };

            output.push(TABLE[(b0 >> 2) as usize] as char);
            output.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            if index + 1 < input.len() {
                output.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
            }
            if index + 2 < input.len() {
                output.push(TABLE[(b2 & 0x3f) as usize] as char);
            }
            index += 3;
        }
        output
    }

    fn make_auth_json(
        account_id: &str,
        email: &str,
        access_marker: &str,
        refresh_marker: &str,
        last_refresh: &str,
    ) -> String {
        let header = base64_url_encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = base64_url_encode(
            format!(
                r#"{{"exp":9999999999,"https://api.openai.com/auth":{{"chatgpt_plan_type":"team","chatgpt_account_id":"{account_id}"}},"https://api.openai.com/profile":{{"email":"{email}"}},"jti":"{access_marker}"}}"#
            )
            .as_bytes(),
        );
        let token = format!("{header}.{payload}.sig");
        format!(
            r#"{{"last_refresh":"{last_refresh}","tokens":{{"access_token":"{token}","refresh_token":"{refresh_marker}","account_id":"{account_id}"}}}}"#
        )
    }

    #[tokio::test]
    async fn preferred_available_auth_path_is_kept() {
        let pool = test_pool();
        let selected = pool
            .preferred_auth_path(Some(std::path::Path::new("/tmp/account-1/auth.json")))
            .await
            .expect("selected path");
        assert_eq!(selected, PathBuf::from("/tmp/account-1/auth.json"));
    }

    #[tokio::test]
    async fn preferred_disabled_auth_path_falls_back_to_next_available() {
        let pool = test_pool();
        pool.toggle("account_1").await.expect("toggle account");

        let selected = pool
            .preferred_auth_path(Some(std::path::Path::new("/tmp/account-1/auth.json")))
            .await
            .expect("selected path");
        assert_eq!(selected, PathBuf::from("/tmp/account-2/auth.json"));
    }

    #[tokio::test]
    async fn preferred_rate_limited_auth_path_falls_back_to_next_available() {
        let pool = test_pool();
        let auth_path = PathBuf::from("/tmp/account-1/auth.json");
        pool.report_error(&auth_path, "Quota exceeded. Try again in 5h")
            .await;

        let selected = pool
            .preferred_auth_path(Some(std::path::Path::new("/tmp/account-1/auth.json")))
            .await
            .expect("selected path");
        assert_eq!(selected, PathBuf::from("/tmp/account-2/auth.json"));
    }

    #[tokio::test]
    async fn expired_token_is_not_selected() {
        let pool = test_pool();
        {
            let mut accounts = pool.accounts.write().await;
            accounts[0].state.token_expires_at = Some(1);
        }

        let selected = pool
            .preferred_auth_path(Some(std::path::Path::new("/tmp/account-1/auth.json")))
            .await
            .expect("selected path");
        assert_eq!(selected, PathBuf::from("/tmp/account-2/auth.json"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn sync_discovered_persists_new_accounts() {
        let _guard = env_lock().lock().expect("lock env");
        let temp_dir = std::env::temp_dir().join(format!("accounts-sync-{}", uuid::Uuid::new_v4()));
        let codex_home = temp_dir.join(".codex");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(
            codex_home.join("auth.json"),
            r#"{"OPENAI_API_KEY":"sk-test","tokens":null}"#,
        )
        .unwrap();
        let config_path = temp_dir.join("accounts.toml");

        let previous_home = std::env::var("HOME").ok();
        let previous_codex_home = std::env::var("CODEX_HOME").ok();
        std::env::set_var("HOME", temp_dir.display().to_string());
        std::env::set_var("CODEX_HOME", codex_home.display().to_string());

        let pool =
            AccountPool::from_entries(Vec::new(), PoolConfig::default(), Some(config_path.clone()));
        let synced = pool.sync_discovered().await.unwrap();
        assert_eq!(synced.len(), 1);
        assert_eq!(pool.list().await.len(), 1);
        assert!(config_path.exists());

        let reloaded = AccountPool::from_config_file(config_path.to_str().unwrap()).unwrap();
        assert_eq!(reloaded.list().await.len(), 1);

        if let Some(home) = previous_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(code_home) = previous_codex_home {
            std::env::set_var("CODEX_HOME", code_home);
        } else {
            std::env::remove_var("CODEX_HOME");
        }
        let _ = std::fs::remove_file(config_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn parses_human_duration_variants() {
        assert_eq!(parse_human_duration_secs("try again in 5h"), Some(18_000));
        assert_eq!(
            parse_human_duration_secs("retry in 4 hours 30 minutes"),
            Some(16_200)
        );
        assert_eq!(parse_human_duration_secs("wait 75s"), Some(75));
    }

    #[tokio::test]
    async fn auth_refresh_failure_forces_fallback_to_next_available_account() {
        let pool = test_pool();
        let auth_path = PathBuf::from("/tmp/account-1/auth.json");
        pool.report_error(
            &auth_path,
            "Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.",
        )
        .await;

        let selected = pool
            .preferred_auth_path(Some(std::path::Path::new("/tmp/account-1/auth.json")))
            .await
            .expect("selected path");
        assert_eq!(selected, PathBuf::from("/tmp/account-2/auth.json"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn sync_discovered_clears_penalties_when_auth_file_changes() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_dir =
            std::env::temp_dir().join(format!("accounts-refresh-{}", uuid::Uuid::new_v4()));
        let codex_home = temp_dir.join(".codex");
        std::fs::create_dir_all(&codex_home).unwrap();
        let auth_path = codex_home.join("auth.json");
        std::fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"token-a","refresh_token":"refresh-a","account_id":"acc-a"}}"#,
        )
        .unwrap();

        let previous_home = std::env::var("HOME").ok();
        let previous_codex_home = std::env::var("CODEX_HOME").ok();
        std::env::set_var("HOME", temp_dir.display().to_string());
        std::env::set_var("CODEX_HOME", codex_home.display().to_string());

        let pool = AccountPool::from_entries(
            vec![AccountEntry::from_config(&AccountConfig {
                id: "default".to_string(),
                label: Some("Default".to_string()),
                auth_path: auth_path.display().to_string(),
                enabled: true,
            })],
            PoolConfig::default(),
            None,
        );

        pool.report_error(&auth_path, "Quota exceeded. Try again in 5h")
            .await;
        assert!(pool
            .preferred_auth_path(Some(auth_path.as_path()))
            .await
            .is_none());

        std::fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"token-b","refresh_token":"refresh-b","account_id":"acc-b"}}"#,
        )
        .unwrap();
        let synced = pool.sync_discovered().await.unwrap();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0].action, "updated");

        let selected = pool
            .preferred_auth_path(Some(auth_path.as_path()))
            .await
            .expect("selected path");
        assert_eq!(selected, auth_path);

        if let Some(home) = previous_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(code_home) = previous_codex_home {
            std::env::set_var("CODEX_HOME", code_home);
        } else {
            std::env::remove_var("CODEX_HOME");
        }
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn next_auth_path_skips_stale_duplicate_identity() {
        let pool = test_pool();
        {
            let mut accounts = pool.accounts.write().await;
            accounts[0].state.jwt_account_id = Some("acc-shared".to_string());
            accounts[0].state.email = Some("shared@example.com".to_string());
            accounts[0].state.last_refresh_at = Some(1_000);
            accounts[0].state.auth_modified_at = Some(1_000);
            accounts[1].state.jwt_account_id = Some("acc-shared".to_string());
            accounts[1].state.email = Some("shared@example.com".to_string());
            accounts[1].state.last_refresh_at = Some(2_000);
            accounts[1].state.auth_modified_at = Some(2_000);
        }

        let selected = pool
            .preferred_auth_path(Some(std::path::Path::new("/tmp/account-1/auth.json")))
            .await
            .expect("selected path");
        assert_eq!(selected, PathBuf::from("/tmp/account-2/auth.json"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn sync_discovered_mirrors_fresher_alias_into_existing_identity() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_dir =
            std::env::temp_dir().join(format!("accounts-alias-sync-{}", uuid::Uuid::new_v4()));
        let codex_home = temp_dir.join(".codex");
        let legacy_dir = temp_dir.join("legacy-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::create_dir_all(&legacy_dir).unwrap();

        let discovered_auth_path = codex_home.join("auth.json");
        let stale_auth_path = legacy_dir.join("auth.json");
        std::fs::write(
            &discovered_auth_path,
            make_auth_json(
                "acc-shared",
                "shared@example.com",
                "fresh-access",
                "fresh-refresh",
                "2026-04-22T10:00:00Z",
            ),
        )
        .unwrap();
        std::fs::write(
            &stale_auth_path,
            make_auth_json(
                "acc-shared",
                "shared@example.com",
                "stale-access",
                "stale-refresh",
                "2026-04-21T10:00:00Z",
            ),
        )
        .unwrap();

        let previous_home = std::env::var("HOME").ok();
        let previous_codex_home = std::env::var("CODEX_HOME").ok();
        std::env::set_var("HOME", temp_dir.display().to_string());
        std::env::set_var("CODEX_HOME", codex_home.display().to_string());

        let pool = AccountPool::from_entries(
            vec![AccountEntry::from_config(&AccountConfig {
                id: "account_legacy".to_string(),
                label: Some("Legacy".to_string()),
                auth_path: stale_auth_path.display().to_string(),
                enabled: true,
            })],
            PoolConfig::default(),
            None,
        );

        pool.report_error(
            &stale_auth_path,
            "Your access token could not be refreshed because your refresh token was already used.",
        )
        .await;

        let synced = pool.sync_discovered().await.unwrap();
        assert!(synced.iter().any(|item| item.action == "mirrored"));

        let mirrored_content = std::fs::read_to_string(&stale_auth_path).unwrap();
        assert!(mirrored_content.contains("fresh-refresh"));
        assert!(pool
            .preferred_auth_path(Some(stale_auth_path.as_path()))
            .await
            .is_some());

        if let Some(home) = previous_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(code_home) = previous_codex_home {
            std::env::set_var("CODEX_HOME", code_home);
        } else {
            std::env::remove_var("CODEX_HOME");
        }
        let _ = std::fs::remove_dir_all(temp_dir);
    }
}
