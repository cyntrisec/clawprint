//! Secret redaction module
//!
//! Automatically redacts sensitive fields from events and artifacts.

use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;

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

/// Patterns for detecting secrets in strings
lazy_static::lazy_static! {
    static ref JWT_PATTERN: Regex = Regex::new(
        r"eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*"
    ).unwrap();
    
    static ref API_KEY_PATTERNS: Vec<Regex> = vec![
        // AWS keys
        Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        // Generic hex keys (32+ chars)
        Regex::new(r"[a-f0-9]{32,}").unwrap(),
        // Base64 long strings (likely keys)
        Regex::new(r"[A-Za-z0-9+/]{40,}={0,2}").unwrap(),
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
    result = JWT_PATTERN.replace_all(&result, "[REDACTED-JWT]").to_string();
    
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
    fn test_redact_string() {
        let s = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.test.signature";
        let redacted = redact_string(s);
        assert!(redacted.contains("[REDACTED-JWT]") || redacted.contains("[REDACTED]"));
    }

    #[test]
    fn test_is_sensitive_field() {
        assert!(is_sensitive_field("api_key"));
        assert!(is_sensitive_field("Authorization"));
        assert!(is_sensitive_field("X-API-Key"));
        assert!(!is_sensitive_field("name"));
    }
}
