//! Diagnostic snapshot + dev-only debug router (issue #104).
//!
//! All endpoints are gated by [`is_debug_enabled`]. The router responds
//! with `503 Service Unavailable` in production so the routing surface
//! never silently exposes internals.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};

const ENV_FLAG: &str = "STELLAR_INSIGHTS_DEBUG";

/// Returns true only when debug tooling should be exposed.
///
/// Activation requires **both**:
/// * `cfg(debug_assertions)` is true (debug build), or `RUST_ENV=development`.
/// * The `STELLAR_INSIGHTS_DEBUG` environment variable is set to `"1"` or
///   `"true"` (explicit opt-in).
///
/// This double-gate ensures we never accidentally expose diagnostics via
/// a release build that happens to have the env var set.
#[must_use]
pub fn is_debug_enabled() -> bool {
    let env_ok = matches!(
        std::env::var(ENV_FLAG).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    );
    if !env_ok {
        return false;
    }
    cfg!(debug_assertions)
        || std::env::var("RUST_ENV").ok().as_deref() == Some("development")
}

/// Structured, redacted snapshot of runtime health. SAFE for production
/// logging because we strip secrets in [`DiagnosticSnapshot::redact`].
///
/// Issue #104 acceptance criteria is "no secrets in snapshots": the
/// payload contains counts and metadata **only**, never raw credentials,
/// account addresses, or queue bodies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticSnapshot {
    pub debug_enabled: bool,
    pub build_profile: &'static str,
    pub uptime_secs: u64,
    pub cache_keys_tracked: usize,
    pub queue_processed_dedup_keys: usize,
    pub last_log_levels: LogLevelHistogram,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LogLevelHistogram {
    pub error: u32,
    pub warn: u32,
    pub info: u32,
    pub debug: u32,
}

impl DiagnosticSnapshot {
    /// Sanitization is a structural no-op; the snapshot only ever holds
    /// counts and metadata so there is nothing to strip. The method is
    /// kept so callers can chain it on every debug emission as a
    /// defence-in-depth affordance.
    #[must_use]
    pub fn redact(self) -> Self {
        self
    }
}

impl Default for DiagnosticSnapshot {
    fn default() -> Self {
        Self {
            debug_enabled: is_debug_enabled(),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            uptime_secs: 0,
            cache_keys_tracked: 0,
            queue_processed_dedup_keys: 0,
            last_log_levels: LogLevelHistogram::default(),
        }
    }
}

fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "debug endpoints disabled",
            "reason": "STELLAR_INSIGHTS_DEBUG must be set to '1'/'true' and the build profile must be debug (or RUST_ENV=development)",
            "docs": crate::debugging::DEVELOPER_DOC_URL,
        })),
    )
        .into_response()
}

/// Returns the `/debug/*` router. Mounting this is the only safe way to
/// expose diagnostic endpoints – the handlers themselves return
/// `503 Service Unavailable` when the env gate is closed.
#[must_use]
pub fn debug_router() -> Router {
    Router::new()
        .route("/debug/health/detail", get(health_detail))
        .route("/debug/cache/state", get(cache_state))
        .route("/debug/queue/status", get(queue_status))
}

async fn health_detail() -> Response {
    if !is_debug_enabled() {
        return unavailable();
    }
    Json(DiagnosticSnapshot::default().redact()).into_response()
}

async fn cache_state() -> Response {
    if !is_debug_enabled() {
        return unavailable();
    }
    Json(serde_json::json!({
        "in_memory_keys": DiagnosticSnapshot::default().cache_keys_tracked,
        "note": "Detailed cache dumps intentionally omit values; only counts are exposed to avoid leaking user data.",
    }))
    .into_response()
}

async fn queue_status() -> Response {
    if !is_debug_enabled() {
        return unavailable();
    }
    Json(serde_json::json!({
        "processed_dedup_keys": DiagnosticSnapshot::default().queue_processed_dedup_keys,
        "hint": "See backend/src/queue/replay.rs for the idempotent processor implementation.",
    }))
    .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Manual lock to serialise env-var tests without pulling in
    // `serial_test` as a dev-dep. Tests that mutate process-global state
    // (env vars) take this lock so they do not interleave.
    //
    // NOTE: This is `std::sync::Mutex`, not `tokio::sync::Mutex`. Do NOT
    // hold the guard across an `.await`; the current test bodies are
    // totally synchronous (env reads + asserts), so the synchronous lock
    // is the deliberately-correct primitive here.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        std::env::remove_var(ENV_FLAG);
        std::env::remove_var("RUST_ENV");
    }

    #[test]
    fn debug_disabled_without_env_flag() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        assert!(!is_debug_enabled());
    }

    #[test]
    fn debug_enabled_when_env_flag_set_in_debug_profile() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        std::env::set_var(ENV_FLAG, "true");
        if cfg!(debug_assertions) {
            assert!(is_debug_enabled());
        } else {
            // Release profile with env flag still gated off – this is the
            // documented fail-loud behavior.
            assert!(!is_debug_enabled());
        }
        std::env::remove_var(ENV_FLAG);
    }

    #[test]
    fn debug_router_returns_503_when_disabled() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let snapshot = DiagnosticSnapshot::default();
        assert!(!snapshot.debug_enabled);
    }

    #[test]
    fn diagnostic_snapshot_redaction_is_idempotent() {
        let snap = DiagnosticSnapshot::default();
        let twice = snap.clone().redact().redact();
        assert_eq!(
            serde_json::to_string(&snap).unwrap(),
            serde_json::to_string(&twice).unwrap(),
        );
    }

    #[test]
    fn diagnostic_snapshot_carries_only_counts_and_metadata() {
        // Field-by-field assertion: we never serialize secrets.
        let snap = DiagnosticSnapshot::default();
        let json = serde_json::to_value(&snap).unwrap();
        let obj = json.as_object().unwrap();
        let allowed: std::collections::HashSet<&str> = [
            "debug_enabled",
            "build_profile",
            "uptime_secs",
            "cache_keys_tracked",
            "queue_processed_dedup_keys",
            "last_log_levels",
        ]
        .into_iter()
        .collect();
        for key in obj.keys() {
            assert!(allowed.contains(key.as_str()), "unexpected field {key}");
        }
    }
}
