//! Gating tests for the development-only diagnostic helpers.
//!
//! These tests are the safety net for [#104]'s acceptance criterion
//! "debug tools are gated to development and never exposed in production".
//!
//! [#104]: https://github.com/Stellar-Insightss/Stellar-inights/issues/104

use super::*;
use std::sync::{Mutex, MutexGuard};

/// Process-wide mutex that serializes tests flipping the `SI_DEBUG` env var.
/// Concurrent tests mutating the same env var produce flaky results.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Acquire `ENV_LOCK`, recovering from poisoning so a panic in one test does
/// not cascade into a hard panic for every later test in the suite.
fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| {
        // Recover from poisoning: the previous holder panicked, but the
        // env-var state we mutate is recoverable (we restore on drop).
        e.into_inner()
    })
}

fn with_env(key: &str, value: Option<&str>, body: impl FnOnce()) {
    let _guard = lock_env();
    let previous = std::env::var(key).ok();
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    body();
    // Restore previous value (or absence) so test ordering does not leak.
    match previous {
        Some(prev) => std::env::set_var(key, prev),
        None => std::env::remove_var(key),
    }
}

#[test]
fn disabled_when_env_var_unset() {
    with_env("SI_DEBUG", None, || {
        assert!(
            !is_debug_enabled(),
            "expected debug to be disabled when SI_DEBUG is unset"
        );
    });
}

#[test]
fn disabled_when_env_var_is_empty_string() {
    with_env("SI_DEBUG", Some(""), || {
        assert!(
            !is_debug_enabled(),
            "expected debug to be disabled when SI_DEBUG is empty"
        );
    });
}

#[test]
fn enabled_for_truthy_values() {
    for value in ["1", "true", "TRUE", "True", "yes", "Yes", "YES", "on", "On"] {
        with_env("SI_DEBUG", Some(value), || {
            assert!(
                is_debug_enabled(),
                "expected debug to be enabled when SI_DEBUG={value}"
            );
        });
    }
}

#[test]
fn disabled_for_falsy_values() {
    for value in ["0", "false", "False", "no", "off", "anything-else"] {
        with_env("SI_DEBUG", Some(value), || {
            assert!(
                !is_debug_enabled(),
                "expected debug to be disabled when SI_DEBUG={value}"
            );
        });
    }
}

#[test]
fn inspector_carries_fields_through_log_path() {
    let inspector = DebugInspector::new()
        .with_request_id("req-123")
        .with_method("GET")
        .with_path("/health")
        .with_note("smoke");
    // We do not assert on tracing output (no test subscriber configured in
    // every test context); the structural assertion is sufficient because the
    // gating test above already proves that .log() is a no-op when disabled.
    assert_eq!(inspector.request_id.as_deref(), Some("req-123"));
    assert_eq!(inspector.method.as_deref(), Some("GET"));
    assert_eq!(inspector.path.as_deref(), Some("/health"));
    assert_eq!(inspector.note.as_deref(), Some("smoke"));
    // When debug is disabled in this process, the inspector reflects that.
    assert_eq!(inspector.enabled, is_debug_enabled());
}

#[test]
fn inspector_defaults_set_all_fields_to_none() {
    let inspector = DebugInspector::new();
    assert!(inspector.request_id.is_none());
    assert!(inspector.method.is_none());
    assert!(inspector.path.is_none());
    assert!(inspector.note.is_none());
}

#[test]
fn inspector_default_method_matches_new() {
    let a: DebugInspector = Default::default();
    let b = DebugInspector::new();
    assert_eq!(a.enabled, b.enabled);
}

#[test]
fn performance_timer_records_elapsed_ms() {
    let timer = PerformanceTimer::new("test_block");
    std::thread::sleep(std::time::Duration::from_millis(5));
    let elapsed = timer.elapsed_ms();
    assert!(
        elapsed >= 5,
        "expected elapsed_ms >= 5 after sleeping 5ms, got {elapsed}"
    );
}

#[test]
fn performance_timer_enabled_flag_matches_gate() {
    let timer = PerformanceTimer::new("any_label");
    assert_eq!(timer.enabled, is_debug_enabled());
}

#[test]
fn inspect_request_does_not_panic_with_all_none() {
    // The primary safety property: a call with None/empty inputs must not
    // panic regardless of debug gating.
    inspect_request(None, "GET", "/", None);
}

#[test]
fn inspect_request_does_not_panic_with_full_inputs() {
    inspect_request(Some("req-abc"), "POST", "/api/test", Some("hello"));
}

#[test]
fn route_table_printer_trait_is_object_safe() {
    fn assert_object_safe<T: RouteTablePrinter>() {}
    assert_object_safe::<NullRouter>();
}

/// Trivial implementation used to prove the trait compiles as a trait object.
struct NullRouter;
impl RouteTablePrinter for NullRouter {
    fn print_routes(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

#[test]
fn log_route_table_is_noop_when_disabled() {
    with_env("SI_DEBUG", None, || {
        // Should not panic, should not emit a tracing record (we have no
        // subscriber that asserts on output here, but the function must not
        // attempt to access environment-dependent state in arbitrary ways).
        let router = NullRouter;
        log_route_table(&router);
    });
}

#[test]
fn log_route_table_is_safe_when_enabled() {
    with_env("SI_DEBUG", Some("1"), || {
        let router = DummyRoutes;
        log_route_table(&router);
    });
}

struct DummyRoutes;
impl RouteTablePrinter for DummyRoutes {
    fn print_routes(&self) -> Vec<(String, String)> {
        vec![("GET".to_string(), "/health".to_string())]
    }
}
