// src/routes/api.rs
//
// Admin REST API cho account pool management.
// Mount tại /api/accounts
//
// Endpoints:
//   GET  /api/accounts              → list all + stats
//   POST /api/accounts              → add account (by path or token)
//   DELETE /api/accounts/:id        → remove account
//   PATCH /api/accounts/:id/toggle  → enable/disable
//   POST /api/accounts/:id/reset    → clear error/degraded state
//   POST /api/accounts/validate     → validate auth without adding
//   GET  /api/accounts/discover     → scan known locations for auth files
//   POST /api/accounts/import       → import discovered account
//   POST /api/accounts/sync         → scan and upsert discovered accounts

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use warp::{Filter, Rejection, Reply};

use crate::accounts::auth_store;
use crate::accounts::{AccountConfig, AccountPool};

#[derive(Debug, Deserialize)]
pub struct AddAccountRequest {
    pub id: String,
    pub label: Option<String>,
    #[serde(default)]
    pub auth_path: Option<String>,
    #[serde(default)]
    pub token: Option<auth_store::TokenPaste>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct ValidateRequest {
    #[serde(default)]
    pub auth_path: Option<String>,
    #[serde(default)]
    pub token: Option<auth_store::TokenPaste>,
}

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    pub auth_path: String,
    pub id: String,
    pub label: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct ApiOk<T: Serialize> {
    ok: bool,
    data: T,
}

#[derive(Debug, Serialize)]
struct ApiErr {
    ok: bool,
    error: String,
}

fn ok<T: Serialize + Send>(data: T) -> impl Reply {
    warp::reply::json(&ApiOk { ok: true, data })
}

fn err_reply(
    msg: impl ToString,
    status: warp::http::StatusCode,
) -> warp::reply::WithStatus<warp::reply::Json> {
    warp::reply::with_status(
        warp::reply::json(&ApiErr {
            ok: false,
            error: msg.to_string(),
        }),
        status,
    )
}

pub fn api_routes(
    pool: Arc<AccountPool>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    let pool = warp::any().map(move || pool.clone());

    // GET /api/accounts
    let list = warp::path!("api" / "accounts")
        .and(warp::get())
        .and(pool.clone())
        .and_then(handle_list);

    // POST /api/accounts/validate (must be before /api/accounts POST)
    let validate = warp::path!("api" / "accounts" / "validate")
        .and(warp::post())
        .and(warp::body::json::<ValidateRequest>())
        .and_then(handle_validate);

    // GET /api/accounts/discover
    let discover = warp::path!("api" / "accounts" / "discover")
        .and(warp::get())
        .and(pool.clone())
        .and_then(handle_discover);

    // POST /api/accounts/import
    let import = warp::path!("api" / "accounts" / "import")
        .and(warp::post())
        .and(warp::body::json::<ImportRequest>())
        .and(pool.clone())
        .and_then(handle_import);

    let sync = warp::path!("api" / "accounts" / "sync")
        .and(warp::post())
        .and(pool.clone())
        .and_then(handle_sync);

    // POST /api/accounts
    let add = warp::path!("api" / "accounts")
        .and(warp::post())
        .and(warp::body::json::<AddAccountRequest>())
        .and(pool.clone())
        .and_then(handle_add);

    // DELETE /api/accounts/:id
    let remove = warp::path!("api" / "accounts" / String)
        .and(warp::delete())
        .and(pool.clone())
        .and_then(handle_remove);

    // PATCH /api/accounts/:id/toggle
    let toggle = warp::path!("api" / "accounts" / String / "toggle")
        .and(warp::patch())
        .and(pool.clone())
        .and_then(handle_toggle);

    // POST /api/accounts/:id/reset
    let reset = warp::path!("api" / "accounts" / String / "reset")
        .and(warp::post())
        .and(pool.clone())
        .and_then(handle_reset);

    // Order matters: specific paths first
    validate
        .or(discover)
        .or(import)
        .or(sync)
        .or(list)
        .or(add)
        .or(remove)
        .or(toggle)
        .or(reset)
}

async fn handle_list(pool: Arc<AccountPool>) -> Result<impl Reply, Rejection> {
    let accounts = pool.list().await;
    Ok(ok(accounts))
}

async fn handle_add(
    req: AddAccountRequest,
    pool: Arc<AccountPool>,
) -> Result<impl Reply, Rejection> {
    // Validate id
    if let Err(e) = auth_store::sanitize_account_id(&req.id) {
        return Ok(err_reply(e, warp::http::StatusCode::BAD_REQUEST));
    }

    // Determine auth_path: from direct path or from token paste
    let auth_path = if let Some(path) = req.auth_path {
        path
    } else if let Some(token) = req.token {
        // Check feature gate
        if !auth_store::is_token_paste_enabled() {
            return Ok(err_reply(
                "Token paste is disabled. Set ENABLE_TOKEN_PASTE=true to enable.",
                warp::http::StatusCode::FORBIDDEN,
            ));
        }

        // Validate token first
        let meta = token.parse_metadata();
        if meta.plan_type == auth_store::PlanType::Unknown && meta.exp.is_none() {
            return Ok(err_reply(
                "Invalid access_token: cannot parse JWT",
                warp::http::StatusCode::BAD_REQUEST,
            ));
        }
        if let Some(exp) = meta.exp {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if exp < now {
                return Ok(err_reply(
                    "Access token is expired",
                    warp::http::StatusCode::BAD_REQUEST,
                ));
            }
        }

        // Create auth file (atomic, 0600)
        match auth_store::create_auth_file_from_token(&req.id, &token) {
            Ok(path) => path.display().to_string(),
            Err(e) => {
                return Ok(err_reply(
                    format!("Failed to create auth file: {e}"),
                    warp::http::StatusCode::INTERNAL_SERVER_ERROR,
                ));
            }
        }
    } else {
        return Ok(err_reply(
            "Either auth_path or token must be provided",
            warp::http::StatusCode::BAD_REQUEST,
        ));
    };

    let cfg = AccountConfig {
        id: req.id,
        label: req.label,
        auth_path,
        enabled: req.enabled,
    };
    match pool.add(cfg).await {
        Ok(_) => Ok(warp::reply::with_status(
            warp::reply::json(&ApiOk {
                ok: true,
                data: "added",
            }),
            warp::http::StatusCode::CREATED,
        )),
        Err(e) => Ok(err_reply(e, warp::http::StatusCode::BAD_REQUEST)),
    }
}

async fn handle_validate(req: ValidateRequest) -> Result<impl Reply, Rejection> {
    let meta = if let Some(path) = &req.auth_path {
        let expanded = shellexpand::tilde(path);
        let p = std::path::Path::new(expanded.as_ref());
        if !p.exists() {
            return Ok(ok(serde_json::json!({
                "valid": false,
                "error": "File not found"
            })));
        }
        auth_store::parse_jwt_from_auth_file(p)
    } else if let Some(token) = &req.token {
        token.parse_metadata()
    } else {
        return Ok(ok(serde_json::json!({
            "valid": false,
            "error": "Either auth_path or token must be provided"
        })));
    };

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let expired = meta.exp.map(|exp| exp < now_unix).unwrap_or(false);

    Ok(ok(serde_json::json!({
        "valid": meta.plan_type != auth_store::PlanType::Unknown || meta.exp.is_some(),
        "plan_type": meta.plan_type,
        "email": meta.email,
        "token_expires_at": meta.exp,
        "token_expired": expired,
        "account_id": meta.account_id,
    })))
}

async fn handle_discover(pool: Arc<AccountPool>) -> Result<impl Reply, Rejection> {
    let mut discovered = auth_store::discover_codex_accounts();

    // Mark which ones are already in pool
    for d in &mut discovered {
        d.already_in_pool = pool.has_auth_path(&d.auth_path).await;
    }

    Ok(ok(discovered))
}

async fn handle_import(
    req: ImportRequest,
    pool: Arc<AccountPool>,
) -> Result<impl Reply, Rejection> {
    if let Err(e) = auth_store::sanitize_account_id(&req.id) {
        return Ok(err_reply(e, warp::http::StatusCode::BAD_REQUEST));
    }

    let cfg = AccountConfig {
        id: req.id,
        label: req.label,
        auth_path: req.auth_path,
        enabled: true,
    };
    match pool.add(cfg).await {
        Ok(_) => Ok(warp::reply::with_status(
            warp::reply::json(&ApiOk {
                ok: true,
                data: "imported",
            }),
            warp::http::StatusCode::CREATED,
        )),
        Err(e) => Ok(err_reply(e, warp::http::StatusCode::BAD_REQUEST)),
    }
}

async fn handle_sync(pool: Arc<AccountPool>) -> Result<warp::reply::Response, Rejection> {
    match pool.sync_discovered().await {
        Ok(accounts) => Ok(warp::reply::json(&ApiOk {
            ok: true,
            data: accounts,
        })
        .into_response()),
        Err(error) => Ok(err_reply(
            format!("Failed to sync discovered accounts: {error}"),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
        .into_response()),
    }
}

async fn handle_remove(id: String, pool: Arc<AccountPool>) -> Result<impl Reply, Rejection> {
    match pool.remove(&id).await {
        Ok(_) => Ok(warp::reply::json(&ApiOk {
            ok: true,
            data: "removed",
        })),
        Err(e) => Ok(warp::reply::json(&ApiErr {
            ok: false,
            error: e.to_string(),
        })),
    }
}

async fn handle_toggle(id: String, pool: Arc<AccountPool>) -> Result<impl Reply, Rejection> {
    match pool.toggle(&id).await {
        Ok(enabled) => Ok(warp::reply::json(&ApiOk {
            ok: true,
            data: enabled,
        })),
        Err(e) => Ok(warp::reply::json(&ApiErr {
            ok: false,
            error: e.to_string(),
        })),
    }
}

async fn handle_reset(id: String, pool: Arc<AccountPool>) -> Result<impl Reply, Rejection> {
    match pool.reset(&id).await {
        Ok(_) => Ok(warp::reply::json(&ApiOk {
            ok: true,
            data: "reset",
        })),
        Err(e) => Ok(warp::reply::json(&ApiErr {
            ok: false,
            error: e.to_string(),
        })),
    }
}
