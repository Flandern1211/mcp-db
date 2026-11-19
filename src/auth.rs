use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};

pub fn generate_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("mcpdb_{}", URL_SAFE_NO_PAD.encode(bytes))
}

pub fn generate_token_id() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    format!("tok_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// One-time admin login token. The plaintext is only ever shown once by the CLI.
pub fn generate_admin_login_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("mcpdb_admin_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Long-lived, revocable admin key. It never expires, but only its hash is stored.
pub fn generate_admin_key() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("mcpdb_adminkey_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Random URL-safe secret used for admin session ids and CSRF values.
pub fn generate_secret() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

pub fn parse_bearer(header: &str) -> Option<&str> {
    let value = header.trim();
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
        Some(token.trim())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_hash_is_stable() {
        let first = generate_token();
        let second = generate_token();
        assert_ne!(first, second);
        assert!(first.starts_with("mcpdb_"));
        assert_eq!(hash_token(&first), hash_token(&first));
        assert_ne!(hash_token(&first), hash_token(&second));
        assert_eq!(hash_token(&first).len(), 64);
    }

    #[test]
    fn admin_tokens_use_a_separate_prefix() {
        let first = generate_admin_login_token();
        let second = generate_admin_login_token();
        assert!(first.starts_with("mcpdb_admin_"));
        assert_ne!(first, second);
        assert_eq!(hash_token(&first).len(), 64);
        assert_ne!(generate_secret(), generate_secret());
    }

    #[test]
    fn parses_only_bearer_headers() {
        assert_eq!(parse_bearer("Bearer abc"), Some("abc"));
        assert_eq!(parse_bearer("bearer abc"), Some("abc"));
        assert_eq!(parse_bearer("Basic abc"), None);
        assert_eq!(parse_bearer("Bearer "), None);
    }
}
