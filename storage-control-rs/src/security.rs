//! Password hashing and cookie signing.
//!
//! Hashes are PHC strings produced by argon2-cffi in the Flask app, so
//! `verify_password` reads existing rows without a migration; `hash_password`
//! writes the same format back.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::AppError;

type HmacSha256 = Hmac<Sha256>;

pub fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::internal("argon2 hash", e))
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

pub fn sign(secret: &[u8], payload: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

pub fn verify_signature(secret: &[u8], payload: &str, signature: &str) -> bool {
    let expected = sign(secret, payload);
    constant_time_eq(expected.as_bytes(), signature.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_roundtrip() {
        let hash = hash_password("s3cret").unwrap();
        assert!(hash.starts_with("$argon2"));
        assert!(verify_password(&hash, "s3cret"));
        assert!(!verify_password(&hash, "wrong"));
        assert!(!verify_password("not-a-phc-string", "s3cret"));
    }

    /// A real hash produced by argon2-cffi (the Flask app's PasswordHasher)
    /// for the password "correct horse battery staple". This is the contract
    /// that lets the importer carry the admin row over without a reset.
    #[test]
    fn argon2_reads_cffi_hash() {
        let hash = "$argon2id$v=19$m=65536,t=3,p=4$ds1G5oLvwX17y4NEivpy9g$OZNsF+k5hB8FaRrFri2AoDyGk5qA97ATBxacBzMhk6E";
        assert!(verify_password(hash, "correct horse battery staple"));
        assert!(!verify_password(hash, "wrong"));
    }

    #[test]
    fn signature_detects_tampering() {
        let secret = b"key";
        let sig = sign(secret, "payload");
        assert!(verify_signature(secret, "payload", &sig));
        assert!(!verify_signature(secret, "payload2", &sig));
        assert!(!verify_signature(secret, "payload", "AAAA"));
    }
}
