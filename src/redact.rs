//! Secret redaction module
//!
//! Automatically redacts sensitive fields from events and artifacts.

use regex::Regex;
use serde_json::Value;

/// Fields that should always be redacted
const SENSITIVE_FIELDS: &[&str] = &[
    "authorization",
    "cookie",
    "api_key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "private_key",
    "privatekey",
    "access_key",
    "accesskey",
    "credential",
    "credentials",
    "session_id",
    "sessionid",
    "auth_token",
    "authtoken",
    "bearer",
    "x-api-key",
    "x-auth-token",
];

// Patterns for detecting secrets in strings
lazy_static::lazy_static! {
    static ref JWT_PATTERN: Regex = Regex::new(
        r"eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*"
    ).unwrap();

    static ref API_KEY_PATTERNS: Vec<Regex> = vec![
        // AWS Access Key IDs
        Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        // AWS Secret Keys (40-char base64 after known prefixes)
        Regex::new(r"(?i)(?:aws_secret_access_key|secret_key)\s*[:=]\s*[A-Za-z0-9+/]{40}").unwrap(),
        // GitHub personal access tokens
        Regex::new(r"ghp_[A-Za-z0-9]{36}").unwrap(),
        // Slack tokens
        Regex::new(r"xox[bprs]-[A-Za-z0-9\-]+").unwrap(),
    ];
}

/// Redacts secrets from a JSON value
pub fn redact_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                let key_lower = key.to_lowercase();
                if SENSITIVE_FIELDS.iter().any(|f| key_lower.contains(f)) {
                    *val = Value::String("[REDACTED]".to_string());
                } else {
                    redact_json(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                redact_json(item);
            }
        }
        Value::String(s) => {
            *s = redact_string(s);
        }
        _ => {}
    }
}

/// Redacts secrets from a string
pub fn redact_string(s: &str) -> String {
    let mut result = s.to_string();

    // Redact JWTs
    result = JWT_PATTERN
        .replace_all(&result, "[REDACTED-JWT]")
        .to_string();

    // Redact API keys
    for pattern in API_KEY_PATTERNS.iter() {
        result = pattern.replace_all(&result, "[REDACTED-KEY]").to_string();
    }

    // Redact Bearer tokens
    if result.to_lowercase().starts_with("bearer ") {
        result = "Bearer [REDACTED]".to_string();
    }

    // Redact Basic auth
    if result.to_lowercase().starts_with("basic ") {
        result = "Basic [REDACTED]".to_string();
    }

    result
}

/// Redacts secrets from raw bytes (attempts JSON parsing first)
pub fn redact_bytes(data: &[u8]) -> Vec<u8> {
    // Try to parse as JSON
    if let Ok(mut value) = serde_json::from_slice::<Value>(data) {
        redact_json(&mut value);
        if let Ok(redacted) = serde_json::to_vec(&value) {
            return redacted;
        }
    }

    // Try to parse as string
    if let Ok(s) = std::str::from_utf8(data) {
        let redacted = redact_string(s);
        return redacted.into_bytes();
    }

    // Binary data - return as-is
    data.to_vec()
}

/// Check if a field name indicates sensitive content
pub fn is_sensitive_field(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    SENSITIVE_FIELDS.iter().any(|f| name_lower.contains(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_json() {
        let mut value = serde_json::json!({
            "name": "test",
            "api_key": "super-secret-123",
            "nested": {
                "password": "hunter2",
                "normal": "value"
            },
            "authorization": "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
        });

        redact_json(&mut value);

        assert_eq!(value["api_key"], "[REDACTED]");
        assert_eq!(value["nested"]["password"], "[REDACTED]");
        assert_eq!(value["name"], "test");
        assert_eq!(value["nested"]["normal"], "value");
        assert_eq!(value["authorization"], "[REDACTED]");
    }

    #[test]
    fn test_redact_string_jwt() {
        let s = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc123";
        let redacted = redact_string(s);
        assert!(
            redacted.contains("[REDACTED-JWT]"),
            "JWT should be redacted, got: {}",
            redacted
        );
    }

    #[test]
    fn test_redact_bearer_token() {
        let s = "Bearer some-opaque-token-here";
        let redacted = redact_string(s);
        assert_eq!(redacted, "Bearer [REDACTED]");
    }

    #[test]
    fn test_redact_basic_auth() {
        let s = "Basic dXNlcjpwYXNz";
        let redacted = redact_string(s);
        assert_eq!(redacted, "Basic [REDACTED]");
    }

    #[test]
    fn test_redact_aws_key() {
        let s = "my key is AKIAIOSFODNN7EXAMPLE";
        let redacted = redact_string(s);
        assert!(
            redacted.contains("[REDACTED-KEY]"),
            "AWS key should be redacted, got: {}",
            redacted
        );
    }

    #[test]
    fn test_redact_github_pat() {
        // GitHub PATs are ghp_ followed by 36 alphanumeric chars
        let s = "token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";
        let redacted = redact_string(s);
        assert!(
            redacted.contains("[REDACTED-KEY]"),
            "GitHub PAT should be redacted, got: {}",
            redacted
        );
    }

    /// SHA-256 hashes must NOT be redacted (was broken by old [a-f0-9]{32,} pattern)
    #[test]
    fn test_no_false_positive_sha256() {
        let sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let redacted = redact_string(sha);
        assert_eq!(redacted, sha, "SHA-256 hash must not be redacted");
    }

    /// UUIDs must NOT be redacted
    #[test]
    fn test_no_false_positive_uuid() {
        let uuid = "550e8400e29b41d4a716446655440000";
        let redacted = redact_string(uuid);
        assert_eq!(redacted, uuid, "UUID must not be redacted");
    }

    /// Normal long base64 strings must NOT be redacted
    #[test]
    fn test_no_false_positive_base64() {
        let b64 = "VGhpcyBpcyBhIHBlcmZlY3RseSBub3JtYWwgYmFzZTY0IHN0cmluZyB0aGF0IHNob3VsZCBub3QgYmUgcmVkYWN0ZWQ=";
        let redacted = redact_string(b64);
        assert_eq!(redacted, b64, "Normal base64 content must not be redacted");
    }

    #[test]
    fn test_redact_json_nested_array() {
        let mut value = serde_json::json!({
            "items": [
                {"token": "secret123"},
                {"name": "safe"}
            ]
        });
        redact_json(&mut value);
        assert_eq!(value["items"][0]["token"], "[REDACTED]");
        assert_eq!(value["items"][1]["name"], "safe");
    }

    #[test]
    fn test_is_sensitive_field() {
        assert!(is_sensitive_field("api_key"));
        assert!(is_sensitive_field("Authorization"));
        assert!(is_sensitive_field("X-API-Key"));
        assert!(is_sensitive_field("SESSION_ID"));
        assert!(is_sensitive_field("my_secret_value"));
        assert!(!is_sensitive_field("name"));
        assert!(!is_sensitive_field("event_id"));
        assert!(!is_sensitive_field("hash_self"));
    }

    #[test]
    fn test_redact_bytes_json() {
        let json = br#"{"api_key":"secret","name":"ok"}"#;
        let redacted = redact_bytes(json);
        let parsed: Value = serde_json::from_slice(&redacted).unwrap();
        assert_eq!(parsed["api_key"], "[REDACTED]");
        assert_eq!(parsed["name"], "ok");
    }

    #[test]
    fn test_redact_bytes_binary_passthrough() {
        let binary = vec![0xFF, 0xFE, 0x00, 0x01];
        let redacted = redact_bytes(&binary);
        assert_eq!(
            redacted, binary,
            "Binary data should pass through unchanged"
        );
    }
}
