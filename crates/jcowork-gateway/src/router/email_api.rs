//! Email + password authentication endpoints: image-captcha-guarded
//! registration, code send, login, and password reset.
//!
//! Security contract:
//! - Neither the emailed code nor the captcha text ever appears in an HTTP
//!   response; in dev mode the code exists only in the server log, the
//!   captcha text only inside the rendered image.
//! - `/api/auth/email/send` answers every syntactically-valid address
//!   identically, so it cannot be used to enumerate accounts. Password reset
//!   burns a code first and may then reveal account existence — the caller
//!   has proven control of the mailbox by then.
//! - Registration does NOT prove mailbox control (no emailed code anymore);
//!   its human-check is the image captcha. The endpoint may reveal that an
//!   address or an account name is taken (409) — a deliberate trade-off:
//!   probing costs one captcha per attempt, and the true mailbox owner can
//!   always reclaim the account through the code-verified password reset.
//! - The registration-credentials email (account + password, by explicit
//!   product decision) is throttled through the shared `CodeStore` under the
//!   `regcred:<email>` key, so it obeys the same cooldown / per-address daily
//!   cap / global daily budget as reset codes.
//! - Code-channel throttling (cooldown, per-address daily cap, brute-force
//!   lockout) and the process-wide daily budget live in the shared
//!   `CodeStore`, identical rules to SMS but with an independent budget.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use super::code_store::{ConsumeError, CODE_TTL_SECS, RESEND_COOLDOWN_SECS};
use super::profile::validate_username;
use super::{
    AppState, EmailLoginRequest, EmailRegisterRequest, EmailSendRequest, PasswordResetRequest,
    mask_email,
};
use crate::auth;
use crate::auth::normalize_email;
use crate::auth::password_error;

/// Send a verification code to an email address.
///
/// Password reset is the only remaining consumer: registration switched to
/// the image captcha and mails the credentials instead. The response must
/// still not depend on whether the address holds an account — a syntax-valid
/// address always gets a 200 with the same body — because the caller has not
/// proven anything at this point. The code is only ever delivered through the
/// mail channel.
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

/// Register an account from a chosen account name, an email address, a
/// password, and a solved image captcha — no mailbox proof required. On
/// success the account name and password are mailed to the address as a
/// record; the response's `email_sent` says whether that delivery worked. A
/// failed delivery never rolls the registration back: the account is fully
/// usable without it.
///
/// Check order is load-bearing: cheap format rejections (email, name,
/// password) happen before the captcha is spent, and the captcha is burned
/// before any 409 exists-check, so probing whether an address or a name is
/// registered costs one captcha per attempt.
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

    let username = req.username.trim();
    if let Some(msg) = validate_username(username) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg})));
    }

    if let Some(msg) = password_error(&req.password) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": msg})));
    }

    let (captcha_id, captcha_code) = match (req.captcha_id, req.captcha_code) {
        (Some(id), Some(code)) if !id.trim().is_empty() && !code.trim().is_empty() => {
            (id, code)
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Captcha is required"})),
            );
        }
    };

    // Single use in every direction: a wrong (or unknown) id burns the
    // challenge, so the client must refresh the image between attempts.
    if !state
        .captcha
        .verify(captcha_id.trim(), captcha_code.trim())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Captcha is incorrect or has expired",
            })),
        );
    }

    // The captcha is spent, so the caller paid for this answer. A mailbox
    // owner who dislikes what they learn can reclaim the address through the
    // code-verified password reset at any time.
    if let Ok(Some(_)) = state.user_store.get_user_by_email(&email).await {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Email address is already registered"})),
        );
    }
    if let Ok(Some(_)) = state.user_store.get_user_by_username(username).await {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Username already exists"})),
        );
    }

    // The credentials email is a real send: reserve the mail budget under a
    // dedicated key before creating anything. Refusal releases the budget
    // internally, exactly like the send endpoint.
    let mail = state.mail.read().expect("mail runtime lock poisoned").clone();
    let (_staged_code, pending) = match mail.store.issue(&format!("regcred:{email}")) {
        Ok(issued) => issued,
        Err(e) => return e.into_response("Email", "email address"),
    };

    let hash = match auth::hash_password(&req.password) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    let user = match state
        .user_store
        .create_email_user(username, &email, &hash)
        .await
    {
        Ok(u) => u,
        Err(e) => {
            // The pre-checks above race with concurrent registrations; the
            // unique indexes are the authority.
            let msg = e.to_string();
            let status = if msg.contains("already registered") || msg.contains("already exists") {
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

    // Deliver the account + password record. The staged regcred entry is
    // never consumed — commit just books the budget and the cooldown.
    let subject = "Jcowork 注册成功";
    let body = format!(
        "您的账号：{username}\n您的邮箱：{email}（可用于找回密码）\n您的密码：{}\n请妥善保管本邮件。\n若非本人操作，可忽略或通过忘记密码重置。",
        req.password
    );
    let email_sent = match mail.sender.send_message(&email, subject, &body).await {
        Ok(()) => {
            pending.commit();
            true
        }
        Err(e) => {
            drop(pending);
            tracing::error!(
                error = %e,
                email = %mask_email(&email),
                "Registration credentials email failed"
            );
            false
        }
    };

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
            "email_sent": email_sent,
        })),
    )
}

/// Log in with an account name (or, for accounts created before names were
/// explicit, the registration email) and a password.
///
/// Every failure below answers identically — unknown account, wrong password,
/// or an unparseable hash (SMS-only accounts hold a deliberate sentinel that
/// Argon2 rejects) — so the endpoint is not an account-existence oracle.
pub(crate) async fn email_login(
    State(state): State<AppState>,
    Json(req): Json<EmailLoginRequest>,
) -> impl IntoResponse {
    let submitted = req.username.trim();

    // `@` can only come from an email address: usernames reject it at
    // registration and on every later rename.
    let user = if submitted.contains('@') {
        match normalize_email(submitted) {
            Some(email) => state.user_store.get_user_by_email(&email).await,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "Invalid email address"})),
                );
            }
        }
    } else {
        state.user_store.get_user_by_username(submitted).await
    };

    let user = match user {
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
        Json(serde_json::json!({"error": "Invalid username or password"})),
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
