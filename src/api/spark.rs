//! DGX Spark build-offload endpoint.
//!
//! A harness inside a mission workspace calls `POST /api/spark/offload` over the
//! host veth link (`10.88.0.1`) to run a Lean build on the DGX Spark instead of
//! the main box. The HOST holds the Spark credentials (arbiter token + SSH
//! target, from [`crate::config::Config`]), so workspaces never carry them.
//!
//! Flow: rsync the mission workspace to the Spark, submit the build to the
//! arbiter (`dgx-spark-arbiter`, which time-shares the Spark's unified memory
//! against vLLM/step37 by priority), poll to completion, rsync artifacts back,
//! return the result. Responds `503` when Spark config is absent so the
//! in-workspace `spark-build` wrapper transparently falls back to a local build.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::routes::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/offload", post(offload_build))
}

/// Secret used to sign the per-mission spark-offload capability token. Reuses
/// the same internal-action secret as Telegram action tokens.
fn spark_offload_secret() -> Option<String> {
    std::env::var("SANDBOXED_INTERNAL_ACTION_SECRET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("JWT_SECRET")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
}

/// Mint a per-mission, scope-bound capability token for spark offload. Unlike
/// the master `SANDBOXED_PROXY_SECRET`, a leaked token only authorizes spark
/// builds for THIS mission — it can't be replayed against the LLM proxy or any
/// other proxy route, nor against another mission's workspace. Returns `None`
/// when no signing secret is configured.
pub fn build_spark_offload_token(mission_id: Uuid) -> Option<String> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let secret = spark_offload_secret()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(b"spark-offload:");
    mac.update(mission_id.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

fn verify_spark_offload_token(mission_id: Uuid, token: &str) -> bool {
    let Some(expected) = build_spark_offload_token(mission_id) else {
        return false;
    };
    super::auth::constant_time_eq(&expected, token.trim())
}

/// Extract the bearer token from the `Authorization` header.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[derive(Deserialize)]
struct OffloadRequest {
    /// Mission this build belongs to. The bearer token must be the scoped
    /// capability token minted for exactly this mission, and `host_dir` must
    /// resolve to this mission's workspace directory.
    mission_id: Uuid,
    /// Host filesystem path of the mission workspace root, injected as
    /// `SPARK_WORKSPACE_HOST_DIR` and echoed back by the wrapper.
    host_dir: String,
    /// Build cwd relative to the workspace root (e.g. `"morpho-verity"`).
    #[serde(default)]
    rel: String,
    /// The build command, e.g. `"lake build"`.
    cmd: String,
    #[serde(default = "default_priority")]
    priority: String,
}

fn default_priority() -> String {
    "P0".to_string()
}

#[derive(Serialize)]
struct OffloadResponse {
    exit_code: i64,
    log: String,
}

/// Validate and canonicalize a caller-supplied mission workspace path.
///
/// Returns the resolved absolute path only when it is a real directory shaped
/// like `<root>/workspaces/mission-*` (the layout produced by
/// `workspace::mission_workspace_dir_for_root`, for both container and host
/// workspaces). `canonicalize` resolves symlinks and requires existence, so a
/// symlink whose target escapes that layout — or any path merely *containing*
/// the `/workspaces/mission-` substring — is rejected.
fn canonical_mission_host_dir(raw: &str) -> Result<String, &'static str> {
    // Cheap pre-filter before touching the filesystem.
    if !raw.starts_with('/') || raw.contains("..") {
        return Err("invalid host_dir");
    }
    let canon = std::fs::canonicalize(raw).map_err(|_| "invalid host_dir")?;
    if !canon.is_dir() {
        return Err("invalid host_dir");
    }
    let is_mission = canon
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("mission-"));
    let in_workspaces = canon
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "workspaces");
    if !is_mission || !in_workspaces {
        return Err("invalid host_dir");
    }
    canon
        .to_str()
        .map(|s| s.to_string())
        .ok_or("invalid host_dir")
}

/// Run a host subprocess, returning (success, combined output).
async fn run(args: &[&str]) -> (bool, String) {
    match tokio::process::Command::new(args[0])
        .args(&args[1..])
        .output()
        .await
    {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), s)
        }
        Err(e) => (false, e.to_string()),
    }
}

async fn offload_build(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<OffloadRequest>,
) -> axum::response::Response {
    // Host-privileged endpoint (rsyncs host paths, SSHes to the Spark). Auth is
    // a per-mission, scope-bound capability token — NOT the master proxy secret
    // — so a token leaked from a workspace can't be replayed against the LLM
    // proxy or another mission. The legitimate caller is the in-workspace
    // `spark-build` wrapper, handed the token minted for its own mission.
    let Some(token_in) = bearer_token(&headers) else {
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };
    if !verify_spark_offload_token(req.mission_id, &token_in) {
        return (StatusCode::UNAUTHORIZED, "invalid spark offload token").into_response();
    }

    // All three must be set, else tell the caller to build locally.
    let (Some(url), Some(token), Some(ssh)) = (
        state.config.spark_arbiter_url.as_deref(),
        state.config.spark_arbiter_token.as_deref(),
        state.config.spark_ssh_target.as_deref(),
    ) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "spark offload not configured",
        )
            .into_response();
    };

    // Security: confine host_dir to a genuine mission workspace directory.
    // A substring check (`contains("/workspaces/mission-")`) is not real
    // containment — an attacker-controlled path or a symlink like
    // `<workspace>/workspaces/mission-x -> /etc` would pass it, and the
    // download rsync below WRITES into host_dir (arbitrary host write as root).
    // So canonicalize (resolving symlinks, requiring existence) and assert the
    // resolved path is `<root>/workspaces/mission-*` — the layout every
    // container and host workspace uses. Using the canonical path for the rsync
    // also removes the argv flag-smuggling surface (it can never start with `-`).
    let host_dir = match canonical_mission_host_dir(&req.host_dir) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    if req.cmd.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "cmd required").into_response();
    }

    let name = host_dir.rsplit('/').next().unwrap_or("build").to_string();
    // The token authenticates a specific mission; the workspace dir is named
    // `mission-<short>`. Reject any attempt to point a mission's token at a
    // different mission's workspace (the rsync below WRITES into host_dir).
    let expected_name = format!("mission-{}", &req.mission_id.to_string()[..8]);
    if name != expected_name {
        return (
            StatusCode::FORBIDDEN,
            "host_dir does not belong to this mission",
        )
            .into_response();
    }
    let user = ssh.split('@').next().unwrap_or("th0rgal");
    let remote_rel = format!(".spark-builds/{}", name);
    let remote_cwd = if req.rel.is_empty() {
        format!("/home/{}/{}", user, remote_rel)
    } else {
        format!(
            "/home/{}/{}/{}",
            user,
            remote_rel,
            req.rel.trim_matches('/')
        )
    };

    // 1. Sync the workspace up to the Spark.
    let up = run(&[
        "rsync",
        "-az",
        "--delete",
        "--exclude",
        ".git",
        "-e",
        "ssh",
        "--",
        &format!("{}/", host_dir),
        &format!("{}:{}/", ssh, remote_rel),
    ])
    .await;
    if !up.0 {
        tracing::warn!("spark offload: rsync up failed: {}", up.1);
        return (
            StatusCode::BAD_GATEWAY,
            format!("rsync up failed: {}", up.1),
        )
            .into_response();
    }

    // 2. Submit the build to the arbiter.
    let client = &state.http_client;
    let submit = client
        .post(format!("{}/build", url))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "priority": req.priority, "cmd": req.cmd, "cwd": remote_cwd,
        }))
        .send()
        .await;
    let jid = match submit {
        Ok(r) => match r.json::<serde_json::Value>().await {
            Ok(v) => v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string()),
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("arbiter submit parse: {e}"),
                )
                    .into_response()
            }
        },
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("arbiter unreachable: {e}")).into_response()
        }
    };
    let Some(jid) = jid else {
        return (StatusCode::BAD_GATEWAY, "arbiter returned no job id").into_response();
    };

    // 3. Poll to completion (cap ~60 min — Lean builds can be long).
    let mut log = String::new();
    let mut exit_code = -1i64;
    let mut done = false;
    for _ in 0..1200 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let st = client
            .get(format!("{}/build/{}", url, jid))
            .bearer_auth(token)
            .send()
            .await
            .ok();
        let Some(v) = st else { continue };
        let Ok(v) = v.json::<serde_json::Value>().await else {
            continue;
        };
        log = v
            .get("log_tail")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        match v.get("status").and_then(|x| x.as_str()) {
            Some("done") | Some("failed") => {
                exit_code = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(-1);
                done = true;
                break;
            }
            _ => continue,
        }
    }
    if !done {
        return (StatusCode::GATEWAY_TIMEOUT, "build timed out on spark").into_response();
    }

    // 4. Sync artifacts (.olean etc.) back into the workspace.
    let _back = run(&[
        "rsync",
        "-az",
        "-e",
        "ssh",
        "--",
        &format!("{}:{}/", ssh, remote_rel),
        &format!("{}/", host_dir),
    ])
    .await;

    Json(OffloadResponse { exit_code, log }).into_response()
}
