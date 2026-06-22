// The `Regex::new(...).unwrap()` calls in this module are on compile-time
// constant patterns that are always valid — unwrapping is correct here.
#![allow(clippy::unwrap_used)]

use serde::{Serialize, Serializer};
use std::fmt;
use regex::Regex;
use once_cell::sync::Lazy;

/// Wrapper type that redacts sensitive data in logs
///
/// Usage:
/// ```
/// let sensitive_data = "secret_value";
/// tracing::info!("Processing data: {:?}", Redacted(&sensitive_data));
/// // Logs: Processing data: [REDACTED]
/// ```
#[derive(Clone)]
pub struct Redacted<T>(pub T);

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl<T: Serialize> Serialize for Redacted<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str("[REDACTED]")
    }
}

/// Redact Stellar account addresses (show first 4 and last 4 chars)
///
/// Example: `GXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX`
/// becomes `GXXX...XXXX`
#[must_use]
pub fn redact_account(account: &str) -> String {
    if account.len() <= 8 {
        return "[REDACTED]".to_string();
    }
    format!("{}...{}", &account[..4], &account[account.len() - 4..])
}

/// Redact payment amounts (show only order of magnitude)
///
/// Example: `1234.56` becomes `~10^3`
#[must_use]
pub fn redact_amount(amount: f64) -> String {
    if amount <= 0.0 {
        return "~10^0".to_string();
    }
    let magnitude = amount.log10().floor() as i32;
    format!("~10^{magnitude}")
}

/// Redact transaction hash (show first 4 and last 4 chars)
#[must_use]
pub fn redact_hash(hash: &str) -> String {
    if hash.len() <= 8 {
        return "[REDACTED]".to_string();
    }
    format!("{}...{}", &hash[..4], &hash[hash.len() - 4..])
}

/// Redact user ID (show only prefix)
///
/// Example: `user_12345678` becomes `user_****`
#[must_use]
pub fn redact_user_id(user_id: &str) -> String {
    if let Some(pos) = user_id.find('_') {
        format!("{}****", &user_id[..=pos])
    } else if user_id.len() > 4 {
        format!("{}****", &user_id[..4])
    } else {
        "[REDACTED]".to_string()
    }
}

/// Redact email address (show only domain)
///
/// Example: `user@example.com` becomes `****@example.com`
#[must_use]
pub fn redact_email(email: &str) -> String {
    if let Some(pos) = email.find('@') {
        format!("****{}", &email[pos..])
    } else {
        "[REDACTED]".to_string()
    }
}

/// Redact IP address (show only first two octets)
///
/// Example: `192.168.1.100` becomes `192.168.*.*`
#[must_use]
pub fn redact_ip(ip: &str) -> String {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() == 4 {
        format!("{}.{}.*.*", parts[0], parts[1])
    } else if ip.contains(':') {
        // IPv6 - show only first segment
        let parts: Vec<&str> = ip.split(':').collect();
        if parts.is_empty() {
            "[REDACTED]".to_string()
        } else {
            format!("{}:****", parts[0])
        }
    } else {
        "[REDACTED]".to_string()
    }
}

/// Redact API key or token (show only first 4 chars)
#[must_use]
pub fn redact_token(token: &str) -> String {
    if token.len() > 4 {
        format!("{}****", &token[..4])
    } else {
        "[REDACTED]".to_string()
    }
}

/// Redact private key or seed phrase (completely hidden)
#[must_use]
pub fn redact_private_key(_key: &str) -> String {
    "[REDACTED_PRIVATE_KEY]".to_string()
}

/// Redact Stellar secret key (starts with 'S')
#[must_use]
pub fn redact_stellar_secret(secret: &str) -> String {
    if secret.starts_with('S') && secret.len() == 56 {
        format!("S****[REDACTED]")
    } else {
        "[REDACTED_SECRET]".to_string()
    }
}

/// Redact JWT token (show header info only)
#[must_use]
pub fn redact_jwt(token: &str) -> String {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() == 3 {
        format!("{}.[PAYLOAD_REDACTED].[SIGNATURE_REDACTED]", parts[0])
    } else {
        "[REDACTED_JWT]".to_string()
    }
}

/// Redact mnemonic/seed phrase (show word count only)
#[must_use] 
pub fn redact_mnemonic(words: &str) -> String {
    let word_count = words.split_whitespace().count();
    format!("[{}_WORD_MNEMONIC_REDACTED]", word_count)
}

/// Redact phone number (show country code only)
#[must_use]
pub fn redact_phone(phone: &str) -> String {
    if phone.starts_with('+') && phone.len() > 4 {
        format!("{}****", &phone[..3])
    } else {
        "[REDACTED_PHONE]".to_string()
    }
}

// Regex patterns for detecting sensitive data
static STELLAR_ACCOUNT_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"G[A-Z0-9]{55}").unwrap()
});

static STELLAR_SECRET_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"S[A-Z0-9]{55}").unwrap()
});

static EMAIL_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap()
});

static JWT_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]*\.[A-Za-z0-9_-]*").unwrap()
});

static API_KEY_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b[A-Za-z0-9_-]{32,}\b").unwrap()
});

/// Automatically redact sensitive data from any string
#[must_use]
pub fn auto_redact_string(input: &str) -> String {
    let mut result = input.to_string();
    
    // Redact Stellar accounts
    result = STELLAR_ACCOUNT_REGEX.replace_all(&result, "G****[REDACTED]").to_string();
    
    // Redact Stellar secret keys
    result = STELLAR_SECRET_REGEX.replace_all(&result, "S****[REDACTED_SECRET]").to_string();
    
    // Redact email addresses
    result = EMAIL_REGEX.replace_all(&result, "****@[REDACTED]").to_string();
    
    // Redact JWT tokens
    result = JWT_REGEX.replace_all(&result, "[REDACTED_JWT]").to_string();
    
    // Redact potential API keys (long random strings)
    result = API_KEY_REGEX.replace_all(&result, "[REDACTED_KEY]").to_string();
    
    result
}

/// Redact sensitive fields from JSON-like structures
#[must_use]
pub fn redact_sensitive_fields(json_str: &str) -> String {
    let sensitive_fields = [
        "password", "secret", "token", "key", "private", "seed", "mnemonic", 
        "credential", "auth", "signature", "jwt", "bearer", "access_token",
        "refresh_token", "client_secret", "api_key", "stellar_secret"
    ];
    
    let mut result = json_str.to_string();
    
    for field in &sensitive_fields {
        // Match field:"value" or field: "value" patterns
        let pattern = format!(r#""{}":\s*"[^"]*""#, field);
        let regex = Regex::new(&pattern).unwrap_or_else(|_| {
            // Fallback to case-insensitive simple match
            Regex::new(&format!(r"(?i){}.*", field)).unwrap()
        });
        result = regex.replace_all(&result, &format!(r#""{}":"[REDACTED]""#, field)).to_string();
    }
    
    // Apply auto string redaction too
    auto_redact_string(&result)
}

/// Comprehensive redaction for structured log data
pub trait SecureDisplay {
    fn secure_display(&self) -> String;
}

impl SecureDisplay for String {
    fn secure_display(&self) -> String {
        auto_redact_string(self)
    }
}

impl SecureDisplay for &str {
    fn secure_display(&self) -> String {
        auto_redact_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_account() {
        let account = "GXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX";
        let redacted = redact_account(account);
        assert_eq!(redacted, "GXXX...XXXX");
    }

    #[test]
    fn test_redact_account_short() {
        let account = "SHORT";
        let redacted = redact_account(account);
        assert_eq!(redacted, "[REDACTED]");
    }

    #[test]
    fn test_redact_amount() {
        assert_eq!(redact_amount(1234.56), "~10^3");
        assert_eq!(redact_amount(50.0), "~10^1");
        assert_eq!(redact_amount(0.5), "~10^-1");
    }

    #[test]
    fn test_redact_hash() {
        let hash = "abcdef1234567890abcdef1234567890";
        let redacted = redact_hash(hash);
        assert_eq!(redacted, "abcd...7890");
    }

    #[test]
    fn test_redact_user_id() {
        assert_eq!(redact_user_id("user_12345678"), "user_****");
        assert_eq!(redact_user_id("12345678"), "1234****");
    }

    #[test]
    fn test_redact_email() {
        assert_eq!(redact_email("user@example.com"), "****@example.com");
        assert_eq!(redact_email("invalid"), "[REDACTED]");
    }

    #[test]
    fn test_redact_ip() {
        assert_eq!(redact_ip("192.168.1.100"), "192.168.*.*");
        assert_eq!(redact_ip("2001:db8::1"), "2001:****");
    }

    #[test]
    fn test_redact_token() {
        assert_eq!(redact_token("abcdef1234567890"), "abcd****");
        assert_eq!(redact_token("abc"), "[REDACTED]");
    }

    #[test]
    fn test_redacted_wrapper() {
        let secret = "my_secret_value";
        let redacted = Redacted(secret);
        assert_eq!(format!("{:?}", redacted), "[REDACTED]");
        assert_eq!(format!("{}", redacted), "[REDACTED]");
    }

    #[test]
    fn test_redact_stellar_secret() {
        assert_eq!(
            redact_stellar_secret("SXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
            "S****[REDACTED]"
        );
        assert_eq!(redact_stellar_secret("invalid"), "[REDACTED_SECRET]");
    }

    #[test]
    fn test_redact_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert_eq!(
            redact_jwt(jwt),
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.[PAYLOAD_REDACTED].[SIGNATURE_REDACTED]"
        );
    }

    #[test]
    fn test_auto_redact_string() {
        let input = "Account: GXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX and secret: SXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX";
        let result = auto_redact_string(input);
        assert!(result.contains("G****[REDACTED]"));
        assert!(result.contains("S****[REDACTED_SECRET]"));
    }

    #[test]
    fn test_redact_sensitive_fields() {
        let json = r#"{"username":"john","password":"secret123","token":"abc123"}"#;
        let result = redact_sensitive_fields(json);
        assert!(result.contains(r#""password":"[REDACTED]""#));
        assert!(result.contains(r#""token":"[REDACTED]""#));
        assert!(result.contains("username"));
    }

    #[test]
    fn test_redact_mnemonic() {
        let mnemonic = "abandon ability able about above absent absorb abstract absurd abuse access accident";
        assert_eq!(redact_mnemonic(mnemonic), "[12_WORD_MNEMONIC_REDACTED]");
    }

    #[test]
    fn test_redact_phone() {
        assert_eq!(redact_phone("+1234567890"), "+12****");
        assert_eq!(redact_phone("1234567890"), "[REDACTED_PHONE]");
    }

    #[test]
    fn test_secure_display_trait() {
        let sensitive = "GXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX";
        assert!(sensitive.secure_display().contains("G****[REDACTED]"));
    }

    // ---- log-pipeline redaction tests (SEC-LOG) ----------------------------
    // These tests simulate passing sensitive values through the redaction
    // helpers before they reach a log sink and assert no plaintext leaks.

    #[test]
    fn test_redact_does_not_leak_stellar_secret_in_request_log() {
        let secret = "SXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX";
        let redacted = redact_stellar_secret(secret);
        assert!(
            !redacted.contains(secret),
            "Stellar secret must not appear in redacted output"
        );
        assert!(
            redacted.starts_with("S****"),
            "Redacted secret should start with S****"
        );
    }

    #[test]
    fn test_redact_does_not_leak_jwt_payload() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJ1c2VyIjoiYWRtaW4ifQ.signature";
        let redacted = redact_jwt(jwt);
        // The payload and signature parts must not appear verbatim
        assert!(
            !redacted.contains("eyJ1c2VyIjoiYWRtaW4ifQ"),
            "JWT payload must not appear in redacted output"
        );
        assert!(
            !redacted.contains("signature"),
            "JWT signature must not appear in redacted output"
        );
        assert!(
            redacted.contains("[PAYLOAD_REDACTED]"),
            "Redacted JWT should contain payload placeholder"
        );
    }

    #[test]
    fn test_redact_does_not_leak_api_key_in_error_log() {
        let error_msg = "Request failed: api_key=super_secret_api_key_1234567890abcdef";
        let redacted = auto_redact_string(error_msg);
        assert!(
            !redacted.contains("super_secret_api_key_1234567890abcdef"),
            "API key must not appear in redacted error log output"
        );
    }

    #[test]
    fn test_redact_sensitive_fields_covers_access_token() {
        let payload = r#"{"username":"alice","access_token":"tok_abcdef1234567890","data":"ok"}"#;
        let redacted = redact_sensitive_fields(payload);
        assert!(
            !redacted.contains("tok_abcdef1234567890"),
            "access_token value must not appear in redacted output"
        );
        assert!(
            redacted.contains("username"),
            "Non-sensitive field 'username' should remain"
        );
    }

    #[test]
    fn test_redact_sensitive_fields_covers_client_secret() {
        let payload = r#"{"client_id":"app123","client_secret":"very_secret_value_xyz"}"#;
        let redacted = redact_sensitive_fields(payload);
        assert!(
            !redacted.contains("very_secret_value_xyz"),
            "client_secret must not appear in redacted output"
        );
        assert!(
            redacted.contains("client_id"),
            "Non-sensitive client_id field should remain"
        );
    }

    #[test]
    fn test_auto_redact_strips_emails_from_error_context() {
        let log_line = "User not found: admin@stellar-insights.com (404)";
        let redacted = auto_redact_string(log_line);
        assert!(
            !redacted.contains("admin@stellar-insights.com"),
            "Email address must not appear in redacted log"
        );
        assert!(
            redacted.contains("****@[REDACTED]"),
            "Redacted email placeholder should be present"
        );
    }

    #[test]
    fn test_redact_private_key_always_fully_hidden() {
        let key = "ed25519_private_key_data_abcdef1234567890";
        let redacted = redact_private_key(key);
        assert_eq!(
            redacted, "[REDACTED_PRIVATE_KEY]",
            "Private key must always be fully redacted"
        );
        assert!(!redacted.contains("ed25519"), "Key material must not leak");
    }

    #[test]
    fn test_redact_mnemonic_hides_word_count_not_words() {
        let mnemonic = "word1 word2 word3 word4 word5 word6 word7 word8 word9 word10 word11 word12";
        let redacted = redact_mnemonic(mnemonic);
        assert!(
            !redacted.contains("word1"),
            "Mnemonic words must not appear in redacted output"
        );
        assert!(
            redacted.contains("12"),
            "Redacted mnemonic should indicate word count"
        );
    }
}
