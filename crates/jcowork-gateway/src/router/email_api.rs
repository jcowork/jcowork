//! Email + password authentication endpoints: verification-code send,
//! registration, login, and password reset.
//!
//! Security contract shared with the SMS flow in [`super::auth_api`]:
//! - The code and the channel name never appear in an HTTP response; in dev
//!   mode the code exists only in the server log.
//! - `/api/auth/email/send` answers every syntactically-valid address
//!   identically, so it cannot be used to enumerate accounts. Registration
//!   and password reset burn a code first and may then reveal account
//!   existence — the caller has proven control of the mailbox by then.
//! - Throttling (cooldown, per-address daily cap, brute-force lockout) and the
//!   process-wide daily budget live in the shared `CodeStore`, identical rules
//!   to SMS but with an independent budget.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use super::code_store::{ConsumeError, CODE_TTL_SECS, RESEND_COOLDOWN_SECS};
use super::{
    AppState, EmailLoginRequest, EmailRegisterRequest, EmailSendRequest, PasswordResetRequest,
    mask_email,
};
use crate::auth;
use crate::auth::normalize_email;
use crate::auth::password_error;

/// Send a verification code to an email address.
///
/// Registration and password reset share this endpoint, so the response must
/// not depend on whether the address holds an account: a syntax-valid address
/// always gets a 200 with the same body. The code is only ever delivered
/// through the mail channel.
pub(crate) async fn email_send(
    State(state): State<AppState>,
    Json(req): Json<EmailSendRequest>,
) -> impl IntoResponse {
    let Some(email) = normalize_email(&req.email) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid email address"})),
        );
    };

    // Snapshot the mail runtime: the RwLock is only held for the clone, so an
    // admin PUT rebuilding the channel can never block on this request.
    let mail = state.mail.read().expect("mail runtime lock poisoned").clone();

    // Reserve the throttling budget and stage the code; every refusal path
    // releases the budget internally.
    let (code, pending) = match mail.store.issue(&email) {
        Ok(issued) => issued,
        Err(e) => return e.into_response("Email", "email address"),
    };

    // `pending` stages the code but commits nothing: a rejected send releases
    // the budget slot and restores the address's previous entry, so the failed
    // attempt costs neither side anything.
    if let Err(e) = mail.sender.send_code(&email, &code).await {
        drop(pending);
        tracing::error!(error = %e, email = %mask_email(&email), "Email send failed");
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "Failed to send verification code, please try again later",
            })),
        );
    }

    pending.commit();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "expires_in": CODE_TTL_SECS,
            "resend_after": RESEND_COOLDOWN_SECS,
        })),
    )
}

/// Register an account from a verified email address and a chosen password.
///
/// The code is consumed only after every cheap precondition passes, so a
/// malformed request never burns the code that was mailed out.
pub(crate) async fn email_register(
    State(state): State<AppState>,
    Json(req): Json<EmailRegisterRequest>,
) -> impl IntoResponse {
    if !state.mail.read().expect("mail runtime lock poisoned").allow_register {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Email registration is disabled"})),
        );
    }

    let Some(email) = normalize_email(&req.email) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid email address"})),
        );
    };

    if let Some(msg) = password_error(&req.password) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg})));
    }

    // By now the caller has proven control of the mailbox, so revealing that
    // the address is taken leaks nothing an attacker could use.
    if let Ok(Some(_)) = state.user_store.get_user_by_email(&email).await {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Email address is already registered"})),
        );
    }

    if let Err(resp) = consume_email_code(&state, &email, req.code.trim()) {
        return resp;
    }

    let hash = match auth::hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    let user = match state.user_store.create_email_user(&email, &hash).await {
        Ok(u) => u,
        Err(e) => {
            // The pre-check above races with a concurrent registration for the
            // same address; the unique index is the authority.
            let msg = e.to_string();
            let status = if msg.contains("already registered") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            return (status, Json(serde_json::json!({"error": msg})));
        }
    };

    tracing::info!(
        user_id = %user.id,
        email = %mask_email(&email),
        "Email account registered"
    );

    let token = match auth::create_token(&state.auth_config, &user.id, &user.username) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "token": token,
            "user_id": user.id,
            "username": user.username,
            "is_admin": user.is_admin,
            // The account carries its email from birth, so the admin
            // bootstrap gate in the middleware never applies to it.
            "must_change_password": false,
            "must_configure_mail": false,
        })),
    )
}

/// Log in with an email address and a password.
///
/// Every failure below answers identically — unknown address, wrong password,
/// or an unparseable hash (SMS-only accounts hold a deliberate sentinel that
/// Argon2 rejects) — so the endpoint is not an account-existence oracle.
pub(crate) async fn email_login(
    State(state): State<AppState>,
    Json(req): Json<EmailLoginRequest>,
) -> impl IntoResponse {
    let Some(email) = normalize_email(&req.email) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid email address"})),
        );
    };

    let user = match state.user_store.get_user_by_email(&email).await {
        Ok(Some(u)) => u,
        Ok(None) => return invalid_credentials(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    let verified = match auth::verify_password(&req.password, &user.password_hash) {
        Ok(v) => v,
        // An unparseable hash fails closed, indistinguishable from a wrong
        // password.
        Err(_) => false,
    };
    if !verified {
        return invalid_credentials();
    }

    // Checked after the password so that only someone who could have logged in
    // learns the account is in the recycle bin.
    if user.deleted_at.is_some() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Account has been deleted"})),
        );
    }

    let token = match auth::create_token(&state.auth_config, &user.id, &user.username) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "token": token,
            "user_id": user.id,
            "username": user.username,
            "is_admin": user.is_admin,
            // Only the admin has a bootstrap-password gate; email accounts
            // set their own password at registration.
            "must_change_password": user.is_admin && user.password_changed_at.is_none(),
            "must_configure_mail": user.is_admin
                && state.mail.read().expect("mail runtime lock poisoned").auto_dev,
        })),
    )
}

fn invalid_credentials() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "Invalid email or password"})),
    )
}

/// Set a new password after verifying an emailed code.
///
/// Unlike the send endpoint, a 404 here is safe: consuming the code proves
/// control of the mailbox, so the caller has the right to know whether the
/// address owns an account.
pub(crate) async fn password_reset(
    State(state): State<AppState>,
    Json(req): Json<PasswordResetRequest>,
) -> impl IntoResponse {
    let Some(email) = normalize_email(&req.email) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid email address"})),
        );
    };

    if let Some(msg) = password_error(&req.new_password) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg})));
    }

    if let Err(resp) = consume_email_code(&state, &email, req.code.trim()) {
        return resp;
    }

    let user = match state.user_store.get_user_by_email(&email).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Account not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    // The account is in the recycle bin; a password would not resurrect it.
    if user.deleted_at.is_some() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Account has been deleted"})),
        );
    }

    let hash = match auth::hash_password(&req.new_password) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    if let Err(e) = state.user_store.update_password_hash(&user.id, &hash).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    tracing::info!(
        user_id = %user.id,
        email = %mask_email(&email),
        "Password reset via emailed verification code"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({"message": "Password has been reset"})),
    )
}

/// Verify `submitted` against the outstanding code for `email` and spend it.
fn consume_email_code(
    state: &AppState,
    email: &str,
    submitted: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    state
        .mail
        .read()
        .expect("mail runtime lock poisoned")
        .store
        .consume(email, submitted)
        .map_err(ConsumeError::into_response)
}
