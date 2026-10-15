//! Core authentication primitives (spec section 8).
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn hash_password(plain: &str) -> Result<String, anyhow::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("password hashing failed: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(plain: &str, stored_hash: &str) -> Result<bool, anyhow::Error> {
    let parsed = PasswordHash::new(stored_hash)
        .map_err(|e| anyhow::anyhow!("stored password hash is malformed: {e}"))?;
    Ok(Argon2::default()
        .verify_password(plain.as_bytes(), &parsed)
        .is_ok())
}

pub fn generate_session_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = hex_encode(&bytes);
    let token_hash = sha256_hex(&token);
    (token, token_hash)
}

pub fn hash_token(token: &str) -> String {
    sha256_hex(token)
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_round_trip() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
        assert!(!verify_password("wrong password", &hash).unwrap());
    }

    #[test]
    fn two_hashes_of_the_same_password_differ() {
        let a = hash_password("same-password").unwrap();
        let b = hash_password("same-password").unwrap();
        assert_ne!(
            a, b,
            "password hashes must use a random salt, not a fixed one"
        );
    }

    #[test]
    fn session_tokens_are_random_and_hash_is_deterministic() {
        let (token_a, hash_a) = generate_session_token();
        let (token_b, hash_b) = generate_session_token();
        assert_ne!(token_a, token_b);
        assert_ne!(hash_a, hash_b);
        assert_eq!(hash_token(&token_a), hash_a);
    }
}
