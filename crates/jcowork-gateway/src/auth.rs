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

/// Reduce user-typed input to a normalized email address: trimmed, lowercased,
/// ASCII, ≤254 characters, exactly one `@`, non-empty local part, and a domain
/// with at least one dot and no empty labels.
///
/// Returns `None` for anything else so callers can reject early instead of
/// paying the mail provider for a guaranteed delivery failure.
pub fn normalize_email(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 254 || !trimmed.is_ascii() {
        return None;
    }
    let lowered = trimmed.to_lowercase();
    let (local, domain) = lowered.split_once('@')?;
    // A second `@` lands in the domain half; a missing half is not an address.
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return None;
    }
    // ".com", "example." and "example..com" are all unroutable shapes people
    // type by accident — reject them rather than mailing them.
    if !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
    {
        return None;
    }
    Some(lowered)
}

/// Shortest and longest accepted password, in characters.
pub(crate) const PASSWORD_MIN_CHARS: usize = 8;
pub(crate) const PASSWORD_MAX_CHARS: usize = 128;

/// Password policy shared by email registration/reset and the admin
/// change-password endpoint. Returns the response message when the password
/// is unacceptable.
pub(crate) fn password_error(password: &str) -> Option<&'static str> {
    let chars = password.chars().count();
    if chars < PASSWORD_MIN_CHARS {
        return Some("Password must be at least 8 characters");
    }
    if chars > PASSWORD_MAX_CHARS {
        return Some("Password must be at most 128 characters");
    }
    // A string of spaces satisfies the length check but is not a password.
    if password.trim().is_empty() {
        return Some("Password cannot be only whitespace");
    }
    None
}

/// Default super-user username (created/promoted on startup).
pub const DEFAULT_ADMIN_USERNAME: &str = "admin";
/// Password the seeded admin account is created with.
///
/// This is a published constant, so the account is locked to the
/// change-password screen until the bootstrap password is replaced on first
/// login. Set `JCWORK_ADMIN_BOOTSTRAP_PASSWORD` to avoid the published
/// default; `JCWORK_ADMIN_PHONE` additionally binds a phone number at
/// startup (SMS login alongside the password — it does not disable either).
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
            "Seeded default admin account — set a new admin password on first login"
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
    if admin.password_changed_at.is_none() {
        tracing::warn!(
            username = DEFAULT_ADMIN_USERNAME,
            "Admin still uses the bootstrap password; the first login will require \
             setting a new one. Set JCWORK_ADMIN_BOOTSTRAP_PASSWORD to avoid the \
             published default."
        );
    }
    Ok(())
}

/// Bind, rebind, or clear the admin's phone number from `JCWORK_ADMIN_PHONE`.
///
/// A misconfigured number logs an error and lets startup continue rather than
/// aborting: the bootstrap password still forces a change on first login, so
/// refusing to boot would turn a typo into an unrecoverable lockout.
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
                "JCWORK_ADMIN_PHONE=clear: admin phone unbound"
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

    tracing::info!(
        username = DEFAULT_ADMIN_USERNAME,
        "Admin phone bound from JCWORK_ADMIN_PHONE; SMS login is available alongside \
         the password (which still must be changed on first login)"
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

    #[test]
    fn password_error_rejects_short_whitespace_and_huge() {
        assert_eq!(
            password_error("short"),
            Some("Password must be at least 8 characters")
        );
        assert_eq!(
            password_error("        "),
            Some("Password cannot be only whitespace")
        );
        assert_eq!(
            password_error(&"x".repeat(129)),
            Some("Password must be at most 128 characters")
        );
        assert_eq!(password_error("a-reasonable-password"), None);
    }

    #[test]
    fn normalize_email_lowercases_and_trims() {
        assert_eq!(
            normalize_email("  Alice@Example.COM ").as_deref(),
            Some("alice@example.com")
        );
    }

    #[test]
    fn normalize_email_accepts_ordinary_addresses() {
        assert_eq!(
            normalize_email("john.doe_99+tag@corp.io").as_deref(),
            Some("john.doe_99+tag@corp.io")
        );
        assert_eq!(normalize_email("a@b.cn").as_deref(), Some("a@b.cn"));
    }

    #[test]
    fn normalize_email_rejects_malformed_addresses() {
        assert_eq!(normalize_email(""), None);
        assert_eq!(normalize_email("no-at-sign"), None);
        assert_eq!(normalize_email("@example.com"), None);
        assert_eq!(normalize_email("alice@"), None);
        // A second `@` hides in the domain half.
        assert_eq!(normalize_email("a@b@example.com"), None);
        // Dotless, empty-label and trailing-dot domains are unroutable.
        assert_eq!(normalize_email("a@localhost"), None);
        assert_eq!(normalize_email("a@.com"), None);
        assert_eq!(normalize_email("a@example..com"), None);
        assert_eq!(normalize_email("a@example.com."), None);
        // Non-ASCII and oversized input.
        assert_eq!(normalize_email("小美@example.com"), None);
        assert_eq!(normalize_email(&format!("{}@example.com", "a".repeat(250))), None);
    }

    /// One test covers every `JCWORK_ADMIN_PHONE` permutation on purpose:
    /// `std::env::set_var` is process-global, so parallel tests would observe
    /// each other's values (same reasoning as the merged SMS/mail env cases).
    #[tokio::test]
    async fn admin_phone_env_binds_without_retiring_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            jcowork_storage::UserStore::new(dir.path().to_str().unwrap()).await.unwrap();

        // Startup with the variable set binds the number and leaves the
        // bootstrap password alone — binding adds SMS login, it no longer
        // stamps the SMS-only sentinel over the hash.
        unsafe { std::env::set_var("JCWORK_ADMIN_PHONE", "138 0013 8123"); }
        ensure_default_admin(&store).await.unwrap();
        unsafe { std::env::remove_var("JCWORK_ADMIN_PHONE"); }

        let admin = store.get_user_by_username(DEFAULT_ADMIN_USERNAME).await.unwrap().unwrap();
        assert_eq!(admin.phone.as_deref(), Some("13800138123"));
        assert!(admin.password_changed_at.is_none());
        assert!(
            verify_password(DEFAULT_ADMIN_PASSWORD, &admin.password_hash).unwrap(),
            "binding a phone must keep the bootstrap password verifiable"
        );

        // A second startup with the same number is a no-op, and `clear`
        // unbinds it again.
        unsafe { std::env::set_var("JCWORK_ADMIN_PHONE", "13800138123"); }
        ensure_default_admin(&store).await.unwrap();
        unsafe { std::env::set_var("JCWORK_ADMIN_PHONE", "clear"); }
        ensure_default_admin(&store).await.unwrap();
        unsafe { std::env::remove_var("JCWORK_ADMIN_PHONE"); }

        let admin = store.get_user_by_username(DEFAULT_ADMIN_USERNAME).await.unwrap().unwrap();
        assert!(admin.phone.is_none(), "JCWORK_ADMIN_PHONE=clear must unbind the number");
        assert!(
            verify_password(DEFAULT_ADMIN_PASSWORD, &admin.password_hash).unwrap(),
            "unbinding must not disturb the password either"
        );

        // An invalid number is ignored rather than aborting startup.
        unsafe { std::env::set_var("JCWORK_ADMIN_PHONE", "not-a-number"); }
        ensure_default_admin(&store).await.unwrap();
        unsafe { std::env::remove_var("JCWORK_ADMIN_PHONE"); }
        let admin = store.get_user_by_username(DEFAULT_ADMIN_USERNAME).await.unwrap().unwrap();
        assert!(admin.phone.is_none());
    }
}
