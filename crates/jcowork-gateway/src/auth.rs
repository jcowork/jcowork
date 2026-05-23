//! JWT-based authentication middleware.

use anyhow::Result;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::password_hash::SaltString;
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};

/// JWT Claims structure.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,       // user_id
    pub username: String,
    pub exp: i64,          // expiry timestamp
    pub iat: i64,          // issued at timestamp
}

/// Auth configuration.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub jwt_secret: String,
    pub token_duration_hours: i64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: "change-me-in-production".to_string(),
            token_duration_hours: 24,
        }
    }
}

/// Create a JWT token for a user.
pub fn create_token(config: &AuthConfig, user_id: &str, username: &str) -> Result<String> {
    let now = Utc::now();
    let claims = Claims {
        sub: user_id.to_string(),
        username: username.to_string(),
        iat: now.timestamp(),
        exp: (now + Duration::hours(config.token_duration_hours)).timestamp(),
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )?;

    Ok(token)
}

/// Verify and decode a JWT token.
pub fn verify_token(config: &AuthConfig, token: &str) -> Result<Claims> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &Validation::default(),
    )?;

    Ok(token_data.claims)
}

/// Hash a password using Argon2.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Hash error: {}", e))?
        .to_string();
    Ok(hash)
}

/// Verify a password against a hash.
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed_hash = PasswordHash::new(hash).map_err(|e| anyhow::anyhow!("Parse error: {}", e))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_password_hash_verify() {
        let password = "test_password_123";
        let hash = hash_password(password).unwrap();
        assert!(verify_password(password, &hash).unwrap());
        assert!(!verify_password("wrong_password", &hash).unwrap());
    }

    #[test]
    fn test_token_create_verify() {
        let config = AuthConfig::default();
        let token = create_token(&config, "user-123", "testuser").unwrap();
        let claims = verify_token(&config, &token).unwrap();
        assert_eq!(claims.sub, "user-123");
        assert_eq!(claims.username, "testuser");
    }
}
