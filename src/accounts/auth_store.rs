// src/accounts/auth_store.rs
//
// JWT metadata parsing, PlanType enum, auth file operations.
// Best-effort metadata extraction — never fails fatally.
//
// Security:
//   - NEVER log raw token values
//   - Auth files created with 0600 permissions
//   - Atomic write: temp → fsync → rename
//   - ID sanitization against path traversal

use std::path::{Path, PathBuf};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use serde::{Deserialize, Serialize};

// ─── PlanType ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PlanType {
    Team,
    Plus,
    Pro,
    Free,
    ApiKey,
    #[default]
    Unknown,
}

impl PlanType {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "team" => Self::Team,
            "plus" => Self::Plus,
            "pro" => Self::Pro,
            "free" => Self::Free,
            "api_key" | "apikey" => Self::ApiKey,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for PlanType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Team => write!(f, "team"),
            Self::Plus => write!(f, "plus"),
            Self::Pro => write!(f, "pro"),
            Self::Free => write!(f, "free"),
            Self::ApiKey => write!(f, "api_key"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

// ─── QuotaHealth ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum QuotaHealth {
    Available,
    RateLimited,
    CoolingDown,
    AuthExpired,
    Disabled,
    Unknown,
}

impl std::fmt::Display for QuotaHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available => write!(f, "available"),
            Self::RateLimited => write!(f, "rate_limited"),
            Self::CoolingDown => write!(f, "cooling_down"),
            Self::AuthExpired => write!(f, "auth_expired"),
            Self::Disabled => write!(f, "disabled"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

// ─── JWT Metadata ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct JwtMetadata {
    pub plan_type: PlanType,
    pub email: Option<String>,
    pub exp: Option<u64>,
    pub account_id: Option<String>,
    pub last_refresh_at: Option<i64>,
}

impl Default for JwtMetadata {
    fn default() -> Self {
        Self {
            plan_type: PlanType::Unknown,
            email: None,
            exp: None,
            account_id: None,
            last_refresh_at: None,
        }
    }
}

/// Parse JWT metadata from an auth.json file.
/// Best-effort: returns defaults on any failure, never panics.
pub fn parse_jwt_from_auth_file(auth_path: &Path) -> JwtMetadata {
    let content = match std::fs::read_to_string(auth_path) {
        Ok(c) => c,
        Err(e) => {
            log::debug!(
                "[auth_store] Cannot read auth file '{}': {}",
                auth_path.display(),
                e
            );
            return JwtMetadata::default();
        }
    };

    parse_jwt_from_auth_json(&content)
}

/// Parse JWT metadata from auth.json content string.
pub fn parse_jwt_from_auth_json(content: &str) -> JwtMetadata {
    let auth: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return JwtMetadata::default(),
    };

    // Try tokens.access_token first, then top-level access_token
    let access_token = auth
        .pointer("/tokens/access_token")
        .or_else(|| auth.get("access_token"))
        .and_then(|v| v.as_str());

    let token = match access_token {
        Some(t) => t,
        None => {
            // Check if it's API key mode
            if auth
                .get("OPENAI_API_KEY")
                .and_then(|v| v.as_str())
                .is_some()
            {
                return JwtMetadata {
                    plan_type: PlanType::ApiKey,
                    ..Default::default()
                };
            }
            return JwtMetadata::default();
        }
    };

    let mut meta = decode_jwt_payload(token);
    meta.last_refresh_at = auth
        .get("last_refresh")
        .and_then(|v| v.as_str())
        .and_then(parse_rfc3339_unix);
    meta
}

/// Decode JWT payload (base64url, no signature verification).
fn decode_jwt_payload(token: &str) -> JwtMetadata {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return JwtMetadata::default();
    }

    let payload_bytes = match base64_url_decode(parts[1]) {
        Ok(b) => b,
        Err(_) => return JwtMetadata::default(),
    };

    let claims: serde_json::Value = match serde_json::from_slice(&payload_bytes) {
        Ok(v) => v,
        Err(_) => return JwtMetadata::default(),
    };

    let plan_type = claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_plan_type")
        .and_then(|v| v.as_str())
        .map(PlanType::from_str_lossy)
        .unwrap_or(PlanType::Unknown);

    let email = claims
        .pointer("/https:~1~1api.openai.com~1profile/email")
        .and_then(|v| v.as_str())
        .map(String::from);

    let exp = claims.get("exp").and_then(|v| v.as_u64());

    let account_id = claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    JwtMetadata {
        plan_type,
        email,
        exp,
        account_id,
        last_refresh_at: None,
    }
}

/// Base64 URL-safe decode (no padding).
fn base64_url_decode(input: &str) -> Result<Vec<u8>, String> {
    // Add padding if needed
    let padded = match input.len() % 4 {
        0 => input.to_string(),
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => return Err("invalid base64url length".to_string()),
    };

    // Replace URL-safe chars
    let standard = padded.replace('-', "+").replace('_', "/");

    // Use a simple base64 decoder
    base64_decode_standard(&standard)
}

/// Simple base64 standard decoder.
fn base64_decode_standard(input: &str) -> Result<Vec<u8>, String> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u8 = 0;

    for &byte in input.as_bytes() {
        if byte == b'=' {
            break;
        }
        let val = match TABLE.iter().position(|&b| b == byte) {
            Some(pos) => pos as u32,
            None => return Err(format!("invalid base64 character: {}", byte as char)),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }

    Ok(output)
}

fn parse_rfc3339_unix(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp())
}

// ─── ID Sanitization ──────────────────────────────────────────────────────────

/// Validate account ID for safety. Rejects path traversal attempts.
pub fn sanitize_account_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("account id cannot be empty".to_string());
    }
    if id.len() > 64 {
        return Err("account id too long (max 64 chars)".to_string());
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") || id.contains('\0') {
        return Err("account id contains forbidden characters (/, \\, .., null)".to_string());
    }
    // Only allow alphanumeric, dash, underscore
    if !id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "account id must contain only alphanumeric, dash, or underscore characters".to_string(),
        );
    }
    Ok(())
}

// ─── Auth File Operations ─────────────────────────────────────────────────────

/// Token data for creating auth files from paste.
/// Accepts either raw auth.json content or individual fields.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum TokenPaste {
    /// Raw auth.json content pasted as-is
    RawJson { raw_json: String },
    /// Individual token fields (legacy)
    Fields {
        access_token: String,
        refresh_token: String,
        account_id: String,
    },
}

impl TokenPaste {
    /// Convert to auth.json content string, ready to write to file.
    pub fn to_auth_json_content(&self) -> Result<String, String> {
        match self {
            TokenPaste::RawJson { raw_json } => {
                // Validate it's valid JSON with tokens
                let parsed: serde_json::Value =
                    serde_json::from_str(raw_json).map_err(|e| format!("Invalid JSON: {e}"))?;
                // Must have tokens.access_token
                if parsed
                    .pointer("/tokens/access_token")
                    .and_then(|v| v.as_str())
                    .is_none()
                {
                    return Err("Missing tokens.access_token in JSON".to_string());
                }
                Ok(serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| raw_json.clone()))
            }
            TokenPaste::Fields {
                access_token,
                refresh_token,
                account_id,
            } => {
                let auth_json = serde_json::json!({
                    "auth_mode": "chatgpt",
                    "OPENAI_API_KEY": null,
                    "tokens": {
                        "access_token": access_token,
                        "refresh_token": refresh_token,
                        "account_id": account_id,
                    },
                    "last_refresh": chrono::Utc::now().to_rfc3339(),
                });
                serde_json::to_string_pretty(&auth_json)
                    .map_err(|e| format!("JSON serialization error: {e}"))
            }
        }
    }

    /// Parse JWT metadata from the token content.
    pub fn parse_metadata(&self) -> JwtMetadata {
        match self.to_auth_json_content() {
            Ok(content) => parse_jwt_from_auth_json(&content),
            Err(_) => JwtMetadata::default(),
        }
    }
}

/// Create auth.json file from pasted token data.
/// Uses atomic write (temp → fsync → rename) with 0600 permissions.
pub fn create_auth_file_from_token(id: &str, token: &TokenPaste) -> Result<PathBuf, anyhow::Error> {
    sanitize_account_id(id).map_err(|e| anyhow::anyhow!(e))?;

    let content = token
        .to_auth_json_content()
        .map_err(|e| anyhow::anyhow!(e))?;

    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
    let dir = PathBuf::from(&home)
        .join(".codex")
        .join("proxy-accounts")
        .join(id);
    std::fs::create_dir_all(&dir)?;

    let target = dir.join("auth.json");
    let tmp = dir.join(".auth.json.tmp");

    // Atomic write: temp → fsync → rename
    std::fs::write(&tmp, &content)?;

    // Set permissions 0600 (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }

    std::fs::rename(&tmp, &target)?;

    log::info!(
        "[auth_store] Created auth file for account '{}' at '{}'",
        id,
        target.display()
    );

    Ok(target)
}

/// Check if token paste feature is enabled via env.
pub fn is_token_paste_enabled() -> bool {
    std::env::var("ENABLE_TOKEN_PASTE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn auth_file_fingerprint(auth_path: &Path) -> Option<u64> {
    let content = std::fs::read(auth_path).ok()?;
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    Some(hasher.finish())
}

pub fn auth_file_modified_unix(auth_path: &Path) -> Option<i64> {
    let metadata = std::fs::metadata(auth_path).ok()?;
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_secs()).ok()
}

// ─── Discovery ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct DiscoveredAccount {
    pub source: String,
    pub auth_path: String,
    pub plan_type: PlanType,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub last_refresh_at: Option<i64>,
    pub auth_modified_at: Option<i64>,
    pub token_expired: bool,
    pub already_in_pool: bool,
}

/// Scan known locations for Codex auth files.
/// Non-fatal: logs warnings, never panics.
pub fn discover_codex_accounts() -> Vec<DiscoveredAccount> {
    let mut candidates = Vec::new();

    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            log::warn!("[discovery] HOME not set, cannot discover accounts");
            return candidates;
        }
    };

    // 1. Check PROXY_AUTH_PATH
    if let Ok(path) = std::env::var("PROXY_AUTH_PATH") {
        let expanded = if path.starts_with("~/") {
            path.replacen('~', &home, 1)
        } else {
            path.clone()
        };
        if Path::new(&expanded).exists() {
            add_candidate(&mut candidates, "PROXY_AUTH_PATH", &expanded);
        }
    }

    // 2. Check $CODEX_HOME/auth.json
    let codex_home = std::env::var("CODEX_HOME").unwrap_or_else(|_| format!("{home}/.codex"));
    let codex_auth = PathBuf::from(&codex_home).join("auth.json");
    if codex_auth.exists() && !has_path(&candidates, &codex_auth) {
        add_candidate(
            &mut candidates,
            "CODEX_HOME",
            &codex_auth.display().to_string(),
        );
    }

    // 3. Fallback: ~/.codex/auth.json (if CODEX_HOME wasn't the same)
    let default_auth = PathBuf::from(&home).join(".codex/auth.json");
    if default_auth.exists() && !has_path(&candidates, &default_auth) {
        add_candidate(
            &mut candidates,
            "default",
            &default_auth.display().to_string(),
        );
    }

    add_candidates_from_child_auth_dirs(
        &mut candidates,
        "proxy_accounts",
        &PathBuf::from(&codex_home).join("proxy-accounts"),
    );
    add_candidates_from_child_auth_dirs(
        &mut candidates,
        "multi_auth_projects",
        &PathBuf::from(&codex_home)
            .join("multi-auth")
            .join("projects"),
    );

    if candidates.is_empty() {
        log::info!(
            "[discovery] No auth files found. If using OS keyring, use add-by-token or export to file."
        );
    } else {
        log::info!(
            "[discovery] Found {} auth file candidate(s)",
            candidates.len()
        );
    }

    candidates
}

fn add_candidate(candidates: &mut Vec<DiscoveredAccount>, source: &str, path: &str) {
    let meta = parse_jwt_from_auth_file(Path::new(path));
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let token_expired = meta.exp.map(|exp| exp < now_unix).unwrap_or(false);

    candidates.push(DiscoveredAccount {
        source: source.to_string(),
        auth_path: path.to_string(),
        plan_type: meta.plan_type,
        email: meta.email,
        account_id: meta.account_id,
        last_refresh_at: meta.last_refresh_at,
        auth_modified_at: auth_file_modified_unix(Path::new(path)),
        token_expired,
        already_in_pool: false, // caller sets this
    });

    log::debug!(
        "[discovery] Candidate from {}: '{}' (expired={})",
        source,
        path,
        token_expired
    );
}

fn has_path(candidates: &[DiscoveredAccount], path: &Path) -> bool {
    let s = path.display().to_string();
    candidates.iter().any(|c| c.auth_path == s)
}

fn add_candidates_from_child_auth_dirs(
    candidates: &mut Vec<DiscoveredAccount>,
    source_prefix: &str,
    root: &Path,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let auth_path = entry.path().join("auth.json");
        if auth_path.exists() && !has_path(candidates, &auth_path) {
            let source = format!("{}:{}", source_prefix, entry.file_name().to_string_lossy());
            add_candidate(candidates, &source, &auth_path.display().to_string());
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn plan_type_from_str_known_values() {
        assert_eq!(PlanType::from_str_lossy("team"), PlanType::Team);
        assert_eq!(PlanType::from_str_lossy("Team"), PlanType::Team);
        assert_eq!(PlanType::from_str_lossy("plus"), PlanType::Plus);
        assert_eq!(PlanType::from_str_lossy("pro"), PlanType::Pro);
        assert_eq!(PlanType::from_str_lossy("free"), PlanType::Free);
        assert_eq!(PlanType::from_str_lossy("api_key"), PlanType::ApiKey);
    }

    #[test]
    fn plan_type_unknown_fallback() {
        assert_eq!(PlanType::from_str_lossy("enterprise"), PlanType::Unknown);
        assert_eq!(PlanType::from_str_lossy(""), PlanType::Unknown);
    }

    #[test]
    fn sanitize_id_valid() {
        assert!(sanitize_account_id("work").is_ok());
        assert!(sanitize_account_id("account-1").is_ok());
        assert!(sanitize_account_id("my_account").is_ok());
        assert!(sanitize_account_id("abc123").is_ok());
    }

    #[test]
    fn sanitize_id_rejects_traversal() {
        assert!(sanitize_account_id("../etc").is_err());
        assert!(sanitize_account_id("a/b").is_err());
        assert!(sanitize_account_id("a\\b").is_err());
        assert!(sanitize_account_id("a\0b").is_err());
    }

    #[test]
    fn sanitize_id_rejects_empty_and_long() {
        assert!(sanitize_account_id("").is_err());
        assert!(sanitize_account_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn sanitize_id_rejects_special_chars() {
        assert!(sanitize_account_id("hello world").is_err());
        assert!(sanitize_account_id("test@user").is_err());
        assert!(sanitize_account_id("test.user").is_err());
    }

    #[test]
    fn jwt_parse_api_key_mode() {
        let content = r#"{"OPENAI_API_KEY": "sk-test", "tokens": null}"#;
        let meta = parse_jwt_from_auth_json(content);
        assert_eq!(meta.plan_type, PlanType::ApiKey);
    }

    #[test]
    fn jwt_parse_empty_json() {
        let meta = parse_jwt_from_auth_json("{}");
        assert_eq!(meta.plan_type, PlanType::Unknown);
        assert!(meta.email.is_none());
    }

    #[test]
    fn jwt_parse_invalid_json() {
        let meta = parse_jwt_from_auth_json("not json");
        assert_eq!(meta.plan_type, PlanType::Unknown);
    }

    #[test]
    fn jwt_decode_real_structure() {
        // Construct a minimal JWT with the expected claims
        let header = base64_url_encode(b"{\"alg\":\"RS256\",\"typ\":\"JWT\"}");
        let payload = base64_url_encode(
            br#"{"exp":9999999999,"https://api.openai.com/auth":{"chatgpt_plan_type":"team","chatgpt_account_id":"acc-123"},"https://api.openai.com/profile":{"email":"test@example.com"}}"#,
        );
        let token = format!("{header}.{payload}.fake_signature");
        let content = format!(
            r#"{{"tokens":{{"access_token":"{token}","refresh_token":"rt_test","account_id":"acc-123"}}}}"#
        );

        let meta = parse_jwt_from_auth_json(&content);
        assert_eq!(meta.plan_type, PlanType::Team);
        assert_eq!(meta.email.as_deref(), Some("test@example.com"));
        assert_eq!(meta.exp, Some(9999999999));
        assert_eq!(meta.account_id.as_deref(), Some("acc-123"));
    }

    #[test]
    fn jwt_parse_last_refresh_timestamp() {
        let header = base64_url_encode(b"{\"alg\":\"RS256\",\"typ\":\"JWT\"}");
        let payload = base64_url_encode(
            br#"{"exp":9999999999,"https://api.openai.com/auth":{"chatgpt_plan_type":"team","chatgpt_account_id":"acc-123"},"https://api.openai.com/profile":{"email":"test@example.com"}}"#,
        );
        let token = format!("{header}.{payload}.fake_signature");
        let content = format!(
            r#"{{"last_refresh":"2026-04-22T08:00:00Z","tokens":{{"access_token":"{token}","refresh_token":"rt_test","account_id":"acc-123"}}}}"#
        );

        let meta = parse_jwt_from_auth_json(&content);
        let expected = chrono::DateTime::parse_from_rfc3339("2026-04-22T08:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(meta.last_refresh_at, Some(expected));
    }

    #[test]
    fn base64_url_roundtrip() {
        let original = b"hello world";
        let encoded = base64_url_encode(original);
        let decoded = base64_url_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    #[allow(clippy::manual_assert)]
    fn discover_includes_proxy_account_files() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_dir =
            std::env::temp_dir().join(format!("discover-proxy-{}", uuid::Uuid::new_v4()));
        let codex_home = temp_dir.join(".codex");
        let account_dir = codex_home.join("proxy-accounts").join("account_b");
        std::fs::create_dir_all(&account_dir).unwrap();
        std::fs::write(
            account_dir.join("auth.json"),
            r#"{"OPENAI_API_KEY":"sk-test","tokens":null}"#,
        )
        .unwrap();

        let previous_home = std::env::var("HOME").ok();
        let previous_codex_home = std::env::var("CODEX_HOME").ok();
        std::env::set_var("HOME", temp_dir.display().to_string());
        std::env::set_var("CODEX_HOME", codex_home.display().to_string());

        let discovered = discover_codex_accounts();

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

        let expected = account_dir.join("auth.json").display().to_string();
        assert!(
            discovered
                .iter()
                .any(|account| account.auth_path == expected),
            "expected discovery to include proxy account auth file"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    #[allow(clippy::manual_assert)]
    fn discover_includes_multi_auth_project_files() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_dir =
            std::env::temp_dir().join(format!("discover-multi-auth-{}", uuid::Uuid::new_v4()));
        let codex_home = temp_dir.join(".codex");
        let project_dir = codex_home.join("multi-auth").join("projects").join("demo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("auth.json"),
            r#"{"OPENAI_API_KEY":"sk-demo","tokens":null}"#,
        )
        .unwrap();

        let previous_home = std::env::var("HOME").ok();
        let previous_codex_home = std::env::var("CODEX_HOME").ok();
        std::env::set_var("HOME", temp_dir.display().to_string());
        std::env::set_var("CODEX_HOME", codex_home.display().to_string());

        let discovered = discover_codex_accounts();

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

        let expected = project_dir.join("auth.json").display().to_string();
        assert!(
            discovered
                .iter()
                .any(|account| account.auth_path == expected),
            "expected discovery to include multi-auth project auth file"
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    fn base64_url_encode(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::new();
        for chunk in input.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
            let combined = (b0 << 16) | (b1 << 8) | b2;
            result.push(TABLE[((combined >> 18) & 0x3F) as usize] as char);
            result.push(TABLE[((combined >> 12) & 0x3F) as usize] as char);
            if chunk.len() > 1 {
                result.push(TABLE[((combined >> 6) & 0x3F) as usize] as char);
            }
            if chunk.len() > 2 {
                result.push(TABLE[(combined & 0x3F) as usize] as char);
            }
        }
        result.replace('+', "-").replace('/', "_")
    }
}
