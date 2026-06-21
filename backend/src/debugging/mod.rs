//! Backend debugging surface.
//!
//! Issue #104 requires actionable debug tooling across backend, frontend,
//! mobile, and docs. This module provides:
//!
//! * [`is_debug_enabled`] – environment gate that returns true only in
//!   development/test builds. Used by every helper in this module so
//!   there is **no** risk of leaking diagnostics in production.
//! * [`DiagnosticSnapshot`] – redacted forensic dump of the most common
//!   failure surfaces (cache state, queue health, recent log levels).
//! * [`debug_router`] – Axum router that exposes
//!   `/debug/health/detail`, `/debug/queue/status`, `/debug/cache/state`
//!   when debug mode is on. Mounting it in production is a programmer
//!   error and will fail loudly at startup.
//!
//! The [`developer_doc_url`] constant is the link rendered in error
//! responses so on-call engineers can self-serve.

pub mod state;

pub use state::{debug_router, is_debug_enabled, DiagnosticSnapshot};

/// Link to the consolidated debugging guide.
pub const DEVELOPER_DOC_URL: &str =
    "https://github.com/Stellar-Insightss/Stellar-inights/blob/main/docs/debugging-guide.md";
