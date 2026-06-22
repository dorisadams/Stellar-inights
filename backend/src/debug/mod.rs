//! Development-only diagnostic helpers.
//!
//! All public entry points are no-ops unless **both**:
//! 1. The binary was compiled with debug assertions enabled
//!    (i.e. a `cargo build` / `cargo run` that doesn't pass `--release`).
//! 2. The `SI_DEBUG` environment variable is set to a truthy value
//!    (`1`, `true`, `yes`, or `on`, case-insensitive).
//!
//! This mirrors the existing `cfg!(debug_assertions)` gating pattern used in
//! [`crate::error`] (see `error.rs`). Production builds (`cargo build --release`)
//! cannot accidentally expose request bodies or run timers because
//! [`is_debug_enabled`] short-circuits at compile time when
//! `debug_assertions` is off, and the dead-code-eliminator strips the
//! inspected branches from release binaries.
//!
//! # Usage
//!
//! Enable for a single run:
//!
//! ```text
//! SI_DEBUG=1 cargo run
//! ```
//!
//! Combine with the existing structured-logging convention:
//!
//! ```text
//! SI_DEBUG=1 RUST_LOG=stellar_insights=debug,debug=trace cargo run
//! ```
//!
//! # Module structure
//!
//! - [`is_debug_enabled`] — single source of truth for the gating decision.
//!   Useful in handlers/tests that need to branch on debug mode.
//! - [`DebugInspector`] — structured per-request inspection record. Compose
//!   with the builder pattern, then `.log()` it.
//! - [`inspect_request`] — convenience wrapper used at request entry
//!   boundaries.
//! - [`PerformanceTimer`] — Drop-based timer that records elapsed wall-clock
//!   time to the `stellar_insights::debug` tracing target when dropped.
//! - [`log_route_table`] — dev-only helper that prints the registered axum
//!   routes. No-op in release.
//!
//! Sensitive data must go through [`crate::logging::redaction::Redacted`]
//! before being placed in a [`DebugInspector`] — never log raw tokens or
//! secret keys, even in debug mode.

use serde::Serialize;
use std::time::Instant;

/// Truthy values accepted in the `SI_DEBUG` env var.
///
/// Kept lowercase to match the comparison done in [`is_debug_enabled`].
const TRUTHY: &[&str] = &["1", "true", "yes", "on"];

/// Single source of truth: debug mode is on **only** when
/// `debug_assertions` is enabled AND `SI_DEBUG` is set to a truthy value.
///
/// The `debug_assertions` check is evaluated at compile time, so a release
/// build cannot accidentally enable this regardless of the env var.
#[must_use]
pub fn is_debug_enabled() -> bool {
    if !cfg!(debug_assertions) {
        return false;
    }
    match std::env::var("SI_DEBUG") {
        Ok(v) => TRUTHY.contains(&v.to_ascii_lowercase().as_str()),
        Err(_) => false,
    }
}

/// Structured per-request inspection record.
///
/// Build with the builder methods, then call [`Self::log`] at a logging
/// boundary, or pass into [`crate::request_id::RequestId`] extension storage
/// to surface it on the response side.
#[derive(Debug, Clone, Serialize)]
pub struct DebugInspector {
    pub enabled: bool,
    request_id: Option<String>,
    method: Option<String>,
    path: Option<String>,
    note: Option<String>,
}

impl DebugInspector {
    /// Construct a new inspector. [`Self::enabled`] is set from
    /// [`is_debug_enabled`] at construction time and is then immutable — the
    /// gate is evaluated once per request to avoid TOCTOU surprises.
    #[must_use]
    pub fn new() -> Self {
        Self {
            enabled: is_debug_enabled(),
            request_id: None,
            method: None,
            path: None,
            note: None,
        }
    }

    #[must_use]
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = Some(method.into());
        self
    }

    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Emit the inspector record via `tracing::info!`. No-op when
    /// [`Self::enabled`] is `false`.
    pub fn log(&self) {
        if !self.enabled {
            return;
        }
        tracing::info!(
            target: "stellar_insights::debug",
            request_id = self.request_id.as_deref().unwrap_or("-"),
            method = self.method.as_deref().unwrap_or("-"),
            path = self.path.as_deref().unwrap_or("-"),
            note = self.note.as_deref().unwrap_or("-"),
            "debug_inspector"
        );
    }
}

impl Default for DebugInspector {
    fn default() -> Self {
        Self::new()
    }
}

/// Drop-based performance timer — logs elapsed wall-clock time when dropped.
///
/// Use the [`debug_timer!`] macro at the top of a scope; the timer logs
/// automatically when the scope exits (normal or panic unwind).
///
/// # Caveats
///
/// - The timer records the *outer* scope's elapsed time, not just the synchronous
///   block it's declared in. For async spans, prefer a
///   `tokio::time::Instant` captured at the span start.
/// - The label is `&'static str` to keep the API cheap — pass a literal.
pub struct PerformanceTimer {
    label: &'static str,
    started_at: Instant,
    enabled: bool,
}

impl PerformanceTimer {
    #[must_use]
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            started_at: Instant::now(),
            enabled: is_debug_enabled(),
        }
    }

    /// Read the elapsed ms without consuming the timer.
    #[must_use]
    pub fn elapsed_ms(&self) -> u128 {
        self.started_at.elapsed().as_millis()
    }
}

impl Drop for PerformanceTimer {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        tracing::info!(
            target: "stellar_insights::debug",
            label = self.label,
            elapsed_ms = self.elapsed_ms() as u64,
            "performance_timer"
        );
    }
}

/// Convenience macro to drop a [`PerformanceTimer`] at the end of the scope
/// it's declared in. Use at the top of a function or block.
///
/// ```ignore
/// use stellar_insights::debug::debug_timer;
///
/// fn handle() {
///     let _t = debug_timer!("handle_request");
///     // ... work ...
/// } // elapsed time logged here if SI_DEBUG=1
/// ```
#[macro_export]
macro_rules! debug_timer {
    ($label:expr) => {
        $crate::debug::PerformanceTimer::new($label)
    };
}

/// Inspect an inbound request. No-op when debug mode is off.
///
/// Prefer the builder form ([`DebugInspector::new`]) when you need to attach
/// extra fields beyond what's available here.
pub fn inspect_request(
    request_id: Option<&str>,
    method: &str,
    path: &str,
    note: Option<&str>,
) {
    // `with_method`/`with_path` accept `impl Into<String>`, so passing the
    // `&str` directly avoids the extra `String` allocation above.
    let mut inspector = DebugInspector::new().with_method(method).with_path(path);
    if let Some(id) = request_id {
        inspector = inspector.with_request_id(id);
    }
    if let Some(n) = note {
        inspector = inspector.with_note(n);
    }
    inspector.log();
}

/// Print the registered axum routes at info level. Dev-only — no-op in release.
///
/// Pass any type with a `pub fn routes(&self) -> axum::Router`. This is a
/// duck-typed wrapper so callers don't need to pull axum into the public API.
pub fn log_route_table<R>(_router: &R)
where
    R: RouteTablePrinter,
{
    if !is_debug_enabled() {
        return;
    }
    tracing::info!(
        target: "stellar_insights::debug",
        routes = ?_router.print_routes(),
        "route_table"
    );
}

/// Trait used by [`log_route_table`] to avoid leaking axum into the public
/// API. Implement this for your app state struct with the routes your
/// service exposes.
pub trait RouteTablePrinter {
    /// Should return a list of `(method, path)` tuples suitable for
    /// debug logging. Implementations are expected to filter out internal
    /// routes and exclude any path containing a literal `*` placeholder.
    fn print_routes(&self) -> Vec<(String, String)>;
}

#[cfg(test)]
mod tests;
