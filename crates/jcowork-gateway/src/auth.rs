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

/// Reduce user-typed input to a bare 11-digit mainland-China mobile number.
///
/// Accepts separators and a leading `86`/`+86`; returns `None` for anything
/// that is not a plausible mobile number, so callers can reject early instead
/// of paying the carrier for a guaranteed delivery failure.
pub fn normalize_phone(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let local = if digits.len() == 13 && digits.starts_with("86") {
        digits[2..].to_string()
    } else {
        digits
    };
    let b = local.as_bytes();
    let valid = b.len() == 11 && b[0] == b'1' && (b'3'..=b'9').contains(&b[1]);
    valid.then_some(local)
}

/// Default super-user username (created/promoted on startup).
pub const DEFAULT_ADMIN_USERNAME: &str = "admin";
/// Password the seeded admin account is created with.
///
/// This is a published constant, so it is only ever a one-time bootstrap door:
/// password login is refused the moment the account has a phone number, and
/// binding overwrites the hash with an unparseable sentinel. Set
/// `JCWORK_ADMIN_BOOTSTRAP_PASSWORD` to close the window even before the first
/// login, or `JCWORK_ADMIN_PHONE` to skip the password entirely.
pub const DEFAULT_ADMIN_PASSWORD: &str = "admin123";

/// `JCWORK_ADMIN_PHONE` value that unbinds the admin's number instead of
/// setting one — the recovery hatch for a phone that is no longer reachable.
const ADMIN_PHONE_CLEAR: &str = "clear";

/// Ensure the default admin account exists and is flagged as super user.
///
/// Called once at startup (server + desktop). If the `admin` account is
/// missing it is created; if the username already exists it is promoted to
/// admin. Only the seeded account is marked, so existing deployments do not
/// gain an unexpected super user unless the username is actually free or
/// already taken by "admin".
pub async fn ensure_default_admin(user_store: &jcowork_storage::UserStore) -> Result<()> {
    let bootstrap_password = std::env::var("JCWORK_ADMIN_BOOTSTRAP_PASSWORD")
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());

    // Only consulted when the account is actually created; an existing admin
    // keeps whatever hash it already has, so rotating this variable later does
    // not silently change a live account's credentials.
    let hash = hash_password(bootstrap_password.as_deref().unwrap_or(DEFAULT_ADMIN_PASSWORD))?;
    let created = user_store
        .ensure_admin_user(DEFAULT_ADMIN_USERNAME, &hash)
        .await?;
    if created {
        tracing::info!(
            username = DEFAULT_ADMIN_USERNAME,
            used_custom_password = bootstrap_password.is_some(),
            "Seeded default admin account — bind a phone number on first login to close password access"
        );
    } else {
        tracing::info!(
            username = DEFAULT_ADMIN_USERNAME,
            "Default admin account ensured (promoted if needed)"
        );
    }

    apply_admin_phone(user_store).await?;

    let Some(admin) = user_store.get_user_by_username(DEFAULT_ADMIN_USERNAME).await? else {
        return Ok(());
    };
    if admin.phone.is_none() {
        tracing::warn!(
            username = DEFAULT_ADMIN_USERNAME,
            "Admin has no phone number bound, so password login is still open for it. \
             Log in once and bind a phone, or set JCWORK_ADMIN_PHONE to bind it at startup."
        );
    }
    Ok(())
}

/// Bind, rebind, or clear the admin's phone number from `JCWORK_ADMIN_PHONE`.
///
/// A misconfigured number logs an error and lets startup continue rather than
/// aborting: the password bootstrap door is still open, so refusing to boot
/// would turn a typo into an unrecoverable lockout.
async fn apply_admin_phone(user_store: &jcowork_storage::UserStore) -> Result<()> {
    let raw = match std::env::var("JCWORK_ADMIN_PHONE") {
        Ok(v) => v.trim().to_string(),
        Err(_) => return Ok(()),
    };
    if raw.is_empty() {
        return Ok(());
    }

    let Some(admin) = user_store.get_user_by_username(DEFAULT_ADMIN_USERNAME).await? else {
        return Ok(());
    };

    if raw.eq_ignore_ascii_case(ADMIN_PHONE_CLEAR) {
        if admin.phone.is_some() {
            user_store.clear_user_phone(&admin.id).await?;
            tracing::warn!(
                username = DEFAULT_ADMIN_USERNAME,
                "JCWORK_ADMIN_PHONE=clear: admin phone unbound, password bootstrap is open again"
            );
        }
        return Ok(());
    }

    let Some(phone) = normalize_phone(&raw) else {
        tracing::error!(
            "JCWORK_ADMIN_PHONE is not a valid 11-digit mobile number; ignoring it"
        );
        return Ok(());
    };

    if admin.phone.as_deref() == Some(phone.as_str()) {
        return Ok(());
    }

    match user_store.set_user_phone(&admin.id, &phone).await {
        Ok(()) => {}
        Err(e) => {
            tracing::error!(
                error = %e,
                "Failed to bind JCWORK_ADMIN_PHONE to the admin account; ignoring it"
            );
            return Ok(());
        }
    }

    // Same sentinel the self-service bind stamps, so the database never holds a
    // verifiable password for an account that has a phone number.
    if let Err(e) = user_store
        .update_password_hash(&admin.id, &jcowork_storage::sms_only_password_hash())
        .await
    {
        tracing::error!(error = %e, "Failed to close admin password login after binding phone");
    }
    tracing::info!(
        username = DEFAULT_ADMIN_USERNAME,
        "Admin phone bound from JCWORK_ADMIN_PHONE; password login disabled for this account"
    );
    Ok(())
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

    #[test]
    fn normalize_phone_accepts_plain_digits() {
        assert_eq!(normalize_phone("13800138000").as_deref(), Some("13800138000"));
    }

    #[test]
    fn normalize_phone_strips_separators_and_country_code() {
        assert_eq!(
            normalize_phone("+86 138-0013-8000").as_deref(),
            Some("13800138000")
        );
        assert_eq!(normalize_phone("8613800138000").as_deref(), Some("13800138000"));
    }

    #[test]
    fn normalize_phone_rejects_invalid_numbers() {
        assert_eq!(normalize_phone(""), None);
        assert_eq!(normalize_phone("12345"), None);
        assert_eq!(normalize_phone("1380013800"), None);
        assert_eq!(normalize_phone("23800138000"), None);
        assert_eq!(normalize_phone("12800138000"), None);
        assert_eq!(normalize_phone("138001380001"), None);
        assert_eq!(normalize_phone("1380013800a"), None);
        assert_eq!(normalize_phone("not-a-phone"), None);
    }
}
