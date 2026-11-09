//! Token generation and hashing. Tokens are shown once; only salted hashes are stored.

use sha2::{Digest, Sha256};

/// Token length in bytes (256 bits).
pub const TOKEN_BYTES: usize = 32;
const SALT_BYTES: usize = 16;

/// A freshly issued credential. `token` is the only copy the human will ever see.
pub struct IssuedToken {
    pub token_id: String,
    pub token: String,
    pub salt: String,
    pub hash: String,
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0_u8; bytes];
    getrandom::getrandom(&mut buf).expect("OS randomness unavailable");
    hex::encode(buf)
}

#[must_use]
pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

/// Issue a token of the form `<token_id>.<secret>`.
#[must_use]
pub fn issue_token() -> IssuedToken {
    let token_id = new_id();
    let secret = random_hex(TOKEN_BYTES);
    let salt = random_hex(SALT_BYTES);
    let hash = hash_secret(&salt, &secret);
    IssuedToken {
        token: format!("{token_id}.{secret}"),
        token_id,
        salt,
        hash,
    }
}

#[must_use]
pub fn hash_secret(salt: &str, secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update([0_u8]);
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

/// Split a presented token into `(token_id, secret)`.
#[must_use]
pub fn split_token(token: &str) -> Option<(&str, &str)> {
    let (id, secret) = token.split_once('.')?;
    (!id.is_empty() && !secret.is_empty()).then_some((id, secret))
}

/// Constant-time comparison of two hex digests.
#[must_use]
pub fn digests_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0_u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Verify a presented secret against a stored salt and hash.
#[must_use]
pub fn verify_secret(salt: &str, stored_hash: &str, secret: &str) -> bool {
    digests_equal(&hash_secret(salt, secret), stored_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_token_verifies_and_is_256_bit() {
        let t = issue_token();
        let (id, secret) = split_token(&t.token).unwrap();
        assert_eq!(id, t.token_id);
        assert_eq!(secret.len(), TOKEN_BYTES * 2);
        assert!(verify_secret(&t.salt, &t.hash, secret));
        assert!(!verify_secret(&t.salt, &t.hash, "wrong"));
        assert!(!t.hash.contains(secret));
    }

    #[test]
    fn malformed_tokens_do_not_split() {
        assert!(split_token("nodot").is_none());
        assert!(split_token(".x").is_none());
        assert!(split_token("x.").is_none());
    }
}
