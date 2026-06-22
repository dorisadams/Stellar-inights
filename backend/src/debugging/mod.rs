/// Debug diagnostics module.
///
/// All public endpoints in this module are **strictly gated to development
/// builds** (`APP_ENV != "production"`). They must never be wired into the
/// production router without going through `guard_dev_only`.
pub mod endpoints;

pub use endpoints::{debug_routes, guard_dev_only};

#[cfg(test)]
mod tests {
    use super::endpoints::is_dev_environment;

    #[test]
    fn dev_env_defaults_to_false_when_unset() {
        // Without APP_ENV set, treat as production-safe (disabled).
        unsafe { std::env::remove_var("APP_ENV") };
        assert!(!is_dev_environment(), "unset APP_ENV must not enable debug endpoints");
    }

    #[test]
    fn dev_env_true_for_development() {
        unsafe { std::env::set_var("APP_ENV", "development") };
        assert!(is_dev_environment());
        unsafe { std::env::remove_var("APP_ENV") };
    }

    #[test]
    fn dev_env_true_for_test() {
        unsafe { std::env::set_var("APP_ENV", "test") };
        assert!(is_dev_environment());
        unsafe { std::env::remove_var("APP_ENV") };
    }

    #[test]
    fn dev_env_false_for_production() {
        unsafe { std::env::set_var("APP_ENV", "production") };
        assert!(!is_dev_environment(), "production must never enable debug endpoints");
        unsafe { std::env::remove_var("APP_ENV") };
    }

    #[test]
    fn dev_env_false_for_staging() {
        unsafe { std::env::set_var("APP_ENV", "staging") };
        assert!(!is_dev_environment(), "staging must not enable debug endpoints");
        unsafe { std::env::remove_var("APP_ENV") };
    }
}
