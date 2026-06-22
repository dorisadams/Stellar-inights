// In test builds, .unwrap() and .expect() are idiomatic — suppress the lints
// crate-wide so individual test modules don't need per-module allows.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod admin_audit_log;
pub mod alerts;
pub mod analytics;
pub mod api;
pub mod debugging;
pub mod api_analytics_middleware;
pub mod api_contract;
pub mod api_deprecation_middleware;
pub mod api_v1_middleware;
pub mod debugging; // Dev-only debug endpoints + diagnostic snapshots (issue #104)
pub mod deprecation_middleware;
pub mod distributed_lock;
pub mod monitor;

pub mod auth;
pub mod auth_middleware;
pub mod backup;
pub mod broadcast;
pub mod cache;
pub mod cache_invalidation;
// cache_middleware removed in favor of cache helper APIs
pub mod crypto;
pub mod cursor_pagination;
pub mod database;
pub mod debug;

pub mod db;
pub mod email;
pub mod env_config;
pub mod error;
pub mod features;
pub mod field_selection;
pub mod handlers; // Core handlers (pool_metrics, health_check, ingestion_status)
pub mod health_check_enhanced; // Enhanced health check with mobile support
pub mod http_cache; // HTTP caching layer (ETag/conditional responses)
pub mod ingestion;
pub mod ip_whitelist_middleware;
pub mod jobs;
pub mod logging;
pub mod ml;
pub mod models;
pub mod muxed;
pub mod request_signing_middleware;

pub mod multi_network;
pub mod network;
pub mod observability;
pub mod openapi;
pub mod pagination;
pub mod payload_limit;
pub mod queue; // Idempotent offline-sync queue processor (issue #93)
pub mod queue;
pub mod rate_limit;
pub mod replay;
pub mod request_id;
pub mod services;
pub mod shutdown;
pub mod snapshot;
pub mod state;
pub mod validation;
pub mod vault;
pub mod webhooks;
pub mod websocket;
pub mod websocket_trace;

pub mod middleware;
pub mod rpc;
