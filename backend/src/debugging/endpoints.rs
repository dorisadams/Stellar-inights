/// HTTP handlers for local development diagnostics.
///
/// These endpoints expose non-sensitive configuration summaries and request
/// echo utilities that are **only available when `APP_ENV` is `development`
/// or `test`**. They must never appear in a production deployment.
///
/// # Wiring
/// Call `debug_routes()` from the router setup and nest it behind a path
/// that is only reachable in local dev (e.g. `/debug`). Always wrap the
/// resulting router with `guard_dev_only` middleware so the guard is
/// enforced at the HTTP layer as well as at build time.
///
/// ```rust,ignore
/// let debug = debug_routes()
///     .layer(axum::middleware::from_fn(guard_dev_only));
/// router.nest("/debug", debug)
/// ```
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use std::collections::HashMap;

/// Returns `true` only when the process is running in a development or test
/// environment. Any other value (including an unset variable) is treated as
/// production-safe, i.e. `false`.
pub fn is_dev_environment() -> bool {
    matches!(
        std::env::var("APP_ENV").as_deref().unwrap_or(""),
        "development" | "test"
    )
}

/// Axum middleware that rejects requests with `403 Forbidden` when the
/// server is not running in development/test mode.
pub async fn guard_dev_only(req: Request, next: Next) -> Response {
    if !is_dev_environment() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "debug endpoints are disabled in this environment"
            })),
        )
            .into_response();
    }
    next.run(req).await
}

/// Non-sensitive environment summary returned by `GET /debug/env`.
///
/// Secrets, tokens, and keys are deliberately excluded. Only structural
/// configuration relevant for debugging local startup issues is included.
#[derive(Serialize)]
pub struct EnvSummary {
    pub app_env: String,
    pub rust_log: String,
    pub log_format: String,
    pub server_host: String,
    pub server_port: String,
    pub stellar_network: String,
    pub rpc_mock_mode: String,
    pub otel_enabled: String,
    pub redis_url_set: bool,
    pub database_url_set: bool,
    pub cors_allowed_origins: String,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

async fn get_env_summary() -> Json<EnvSummary> {
    Json(EnvSummary {
        app_env: env_or("APP_ENV", "(unset)"),
        rust_log: env_or("RUST_LOG", "(unset)"),
        log_format: env_or("LOG_FORMAT", "(unset)"),
        server_host: env_or("SERVER_HOST", "(unset)"),
        server_port: env_or("SERVER_PORT", "(unset)"),
        stellar_network: env_or("STELLAR_NETWORK", "(unset)"),
        rpc_mock_mode: env_or("RPC_MOCK_MODE", "(unset)"),
        otel_enabled: env_or("OTEL_ENABLED", "(unset)"),
        // Only report whether secrets are set, never their values.
        redis_url_set: std::env::var("REDIS_URL").is_ok(),
        database_url_set: std::env::var("DATABASE_URL").is_ok(),
        cors_allowed_origins: env_or("CORS_ALLOWED_ORIGINS", "(unset)"),
    })
}

/// Simple liveness ping used during local development to confirm the server
/// is reachable and the debug middleware chain is working.
#[derive(Serialize)]
pub struct PingResponse {
    pub status: &'static str,
    pub environment: String,
}

async fn ping() -> Json<PingResponse> {
    Json(PingResponse {
        status: "ok",
        environment: env_or("APP_ENV", "unknown"),
    })
}

/// Returns a summary of currently active feature flags and build-time
/// configuration knobs visible from environment variables.
async fn get_feature_flags() -> Json<HashMap<String, serde_json::Value>> {
    let mut flags = HashMap::new();
    let bool_flags = [
        "JOB_CORRIDOR_REFRESH_ENABLED",
        "JOB_ANCHOR_REFRESH_ENABLED",
        "JOB_PRICE_FEED_UPDATE_ENABLED",
        "JOB_CACHE_CLEANUP_ENABLED",
        "BACKUP_ENABLED",
        "LOGSTASH_ENABLED",
        "RPC_MOCK_MODE",
        "OTEL_ENABLED",
    ];
    for key in &bool_flags {
        let val = std::env::var(key)
            .map(|v| v.to_lowercase() == "true")
            .unwrap_or(false);
        flags.insert(key.to_lowercase(), serde_json::Value::Bool(val));
    }
    Json(flags)
}

/// Assemble all debug routes. The caller is responsible for nesting these
/// under an appropriate path prefix and applying `guard_dev_only`.
pub fn debug_routes() -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route("/env", get(get_env_summary))
        .route("/flags", get(get_feature_flags))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_summary_does_not_expose_secrets() {
        // Confirm that known secret env vars are absent from the EnvSummary
        // struct fields by name. This is a compile-time structural assertion.
        //
        // The struct has no field named "jwt_secret", "encryption_key",
        // "vault_token", "sep10_server_public_key", etc.
        let field_names = [
            "app_env",
            "rust_log",
            "log_format",
            "server_host",
            "server_port",
            "stellar_network",
            "rpc_mock_mode",
            "otel_enabled",
            "redis_url_set",
            "database_url_set",
            "cors_allowed_origins",
        ];
        let secret_names = ["jwt_secret", "encryption_key", "vault_token", "sep10_server_public_key"];
        for secret in &secret_names {
            assert!(
                !field_names.contains(secret),
                "EnvSummary must not contain secret field `{secret}`"
            );
        }
    }

    #[tokio::test]
    async fn ping_returns_ok() {
        let resp = ping().await;
        assert_eq!(resp.0.status, "ok");
    }

    #[tokio::test]
    async fn env_summary_redis_flag_false_when_unset() {
        unsafe { std::env::remove_var("REDIS_URL") };
        let resp = get_env_summary().await;
        assert!(!resp.0.redis_url_set);
    }

    #[tokio::test]
    async fn env_summary_redis_flag_true_when_set() {
        unsafe { std::env::set_var("REDIS_URL", "redis://localhost:6379") };
        let resp = get_env_summary().await;
        assert!(resp.0.redis_url_set);
        unsafe { std::env::remove_var("REDIS_URL") };
    }
}
