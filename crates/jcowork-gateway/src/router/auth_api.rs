//! Authentication endpoints: SMS verification-code login, admin bootstrap
//! password login, and health.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use rand_core::{OsRng, RngCore};
use std::sync::atomic::{AtomicU32, Ordering};

use super::{
    AppState, LoginRequest, SmsCodeEntry, SmsLoginRequest, SmsSendRequest, SMS_CODE_TTL_SECS,
    SMS_LOCK_SECS, SMS_MAX_ATTEMPTS, SMS_PER_PHONE_DAILY, SMS_RESEND_COOLDOWN_SECS, mask_phone,
};
use crate::auth;
use crate::auth::normalize_phone;

pub(crate) async fn health() -> &'static str {
    "ok"
}

/// Password login, kept only as the one-time bootstrap door for `admin`.
pub(crate) async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> impl IntoResponse {
    // Look up user by username
    let user = match state.user_store.get_user_by_username(&req.username).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid username or password"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    // Recycle-bin accounts cannot log in until restored by the admin.
    if user.deleted_at.is_some() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Account has been deleted"})),
        );
    }

    // Password login exists solely to let a freshly seeded admin reach the
    // phone-binding screen. Check this before verifying the password: the
    // response has to be identical to a wrong password, or it would disclose
    // which usernames hold admin accounts.
    if !user.is_admin || user.phone.is_some() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Invalid username or password"})),
        );
    }

    // Verify password
    match auth::verify_password(&req.password, &user.password_hash) {
        Ok(true) => {},
        // An unparseable hash is not a server fault: SMS-only accounts hold a
        // deliberate sentinel that Argon2 rejects. Reporting it as invalid
        // credentials keeps password login failing closed without revealing
        // that the account authenticates some other way.
        Ok(false) | Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid username or password"})),
            );
        }
    }

    // Create JWT token
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
            // Always true here — the gate above only admits accounts without a
            // phone — but the frontend branches on it, so state it explicitly.
            "must_bind_phone": true,
        })),
    )
}

// --- SMS verification-code login ---

/// Generate a 6-digit code from the OS CSPRNG.
fn generate_code() -> String {
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    format!("{:06}", u64::from_be_bytes(bytes) % 1_000_000)
}

/// UTC day key that both the global and per-phone counters roll over on.
fn today_key() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// One reserved slot of the process-wide daily SMS budget.
///
/// Reserving before the send rather than counting afterwards is what keeps the
/// cap honest: a post-hoc counter lets a burst of N concurrent requests all pass
/// the same check and overshoot by N. Every path between the reservation and a
/// successful delivery has to give the slot back, and an RAII guard is the only
/// way to guarantee that without repeating the release at each early return.
struct BudgetSlot<'a> {
    counter: &'a AtomicU32,
    committed: bool,
}

impl<'a> BudgetSlot<'a> {
    /// Atomically take one slot, or return `None` when the budget is spent.
    fn try_reserve(counter: &'a AtomicU32, cap: u32) -> Option<Self> {
        // `fetch_add` yields the value *before* the increment, so taking and
        // comparing happen in one atomic step — two callers can never both see
        // room under the cap and both send.
        if counter.fetch_add(1, Ordering::SeqCst) >= cap {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self { counter, committed: false })
    }

    /// Keep the slot; the message is on its way.
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for BudgetSlot<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // Saturating so that a day rollover between reserve and release — which
        // resets the counter to zero — cannot wrap it to `u32::MAX` and wedge
        // every send until the next midnight.
        let _ = self
            .counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                Some(v.saturating_sub(1))
            });
    }
}

/// Send a verification code to a phone number.
///
/// The response body deliberately omits both the code and the provider name.
/// In dev mode the code exists only in the server log; echoing it — or even
/// revealing that the channel is `dev` — would tell an attacker exactly where
/// to look.
pub(crate) async fn sms_send(
    State(state): State<AppState>,
    Json(req): Json<SmsSendRequest>,
) -> impl IntoResponse {
    let phone = match normalize_phone(&req.phone) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Invalid phone number"})),
            );
        }
    };

    let now = chrono::Utc::now().timestamp();
    let day = today_key();

    // Sweep entries idle for over a day. The window has to outlive the code TTL,
    // otherwise `sent_today` resets every time the previous code expires and the
    // "daily" limit quietly degrades into a per-code limit.
    state.sms.codes.retain(|_, e| e.last_sent_at > now - 24 * 3600);

    // Reserve a slot from the process-wide budget before touching per-phone
    // state, so that once it is exhausted an attacker cannot probe which numbers
    // exist. The slot is released automatically by every early return below.
    let slot = {
        // A poisoned lock only matters if a holder panicked mid-update; the
        // string is still usable, so recover rather than 500 the request.
        let mut guard = state.sms.day_key.lock().unwrap_or_else(|e| e.into_inner());
        if *guard != day {
            *guard = day.clone();
            state.sms.sent_today.store(0, Ordering::SeqCst);
        }
        BudgetSlot::try_reserve(&state.sms.sent_today, state.sms.daily_global_cap)
    };
    let slot = match slot {
        Some(s) => s,
        None => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "SMS quota exhausted, please try again tomorrow",
                })),
            );
        }
    };

    // Snapshot for rollback: a send the carrier rejected must not consume the
    // user's cooldown or their per-phone daily allowance. (The global budget
    // slot is released separately, by dropping `slot`.)
    let previous = state.sms.codes.get(&phone).map(|e| e.clone());

    let code = generate_code();
    {
        // Every throttle check and every mutation lives inside this one guard so
        // two concurrent requests to the same number cannot both pass cooldown.
        let mut entry = state.sms.codes.entry(phone.clone()).or_insert_with(|| SmsCodeEntry {
            code: String::new(),
            expires_at: 0,
            attempts: 0,
            locked_until: 0,
            // 0 means "never sent", which the cooldown check below accepts.
            last_sent_at: 0,
            day_key: day.clone(),
            sent_today: 0,
        });
        if entry.day_key != day {
            entry.day_key = day.clone();
            entry.sent_today = 0;
        }
        if now < entry.locked_until {
            return too_many_attempts(entry.locked_until - now);
        }
        if now - entry.last_sent_at < SMS_RESEND_COOLDOWN_SECS {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "Please wait before requesting another code",
                    "retry_after": SMS_RESEND_COOLDOWN_SECS - (now - entry.last_sent_at),
                })),
            );
        }
        if entry.sent_today >= SMS_PER_PHONE_DAILY {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "Daily limit reached for this phone number",
                })),
            );
        }
        entry.code = code.clone();
        entry.expires_at = now + SMS_CODE_TTL_SECS;
        entry.attempts = 0;
        entry.last_sent_at = now;
        entry.sent_today += 1;
    }
    // The guard is dropped here. Holding a DashMap reference across the await
    // below would block every other request touching this shard.

    if let Err(e) = state.sms.sender.send_code(&phone, &code).await {
        match previous {
            Some(entry) => {
                state.sms.codes.insert(phone.clone(), entry);
            }
            None => {
                state.sms.codes.remove(&phone);
            }
        }
        tracing::error!(error = %e, phone = %mask_phone(&phone), "SMS send failed");
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "Failed to send verification code, please try again later",
            })),
        );
    }

    slot.commit();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "expires_in": SMS_CODE_TTL_SECS,
            "resend_after": SMS_RESEND_COOLDOWN_SECS,
        })),
    )
}

/// Exchange a verified phone number + code for a session token.
///
/// An unknown number auto-provisions an account unless `sms_allow_register` is
/// off, which is what makes this endpoint the only signup path.
pub(crate) async fn sms_login(
    State(state): State<AppState>,
    Json(req): Json<SmsLoginRequest>,
) -> impl IntoResponse {
    let phone = match normalize_phone(&req.phone) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Invalid phone number"})),
            );
        }
    };

    let submitted = req.code.trim();
    if let Err(resp) = consume_sms_code(&state, &phone, submitted) {
        return resp;
    }

    let user = match state.user_store.get_user_by_phone(&phone).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            if !state.sms.allow_register {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"error": "Phone number is not registered"})),
                );
            }
            match state.user_store.get_or_create_by_phone(&phone).await {
                Ok(u) => u,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"error": e.to_string()})),
                    );
                }
            }
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    // A trashed account still occupies its number in the unique index, so the
    // lookup above finds it; reject here instead of handing out a token that the
    // auth middleware would immediately bounce.
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
            "must_bind_phone": user.phone.is_none(),
        })),
    )
}

/// Verify `submitted` against the outstanding code for `phone` and consume it.
///
/// Shared by SMS login and phone binding so both paths enforce identical
/// attempt limits. On `Ok` the code is spent; on `Err` the caller returns the
/// response verbatim.
pub(crate) fn consume_sms_code(
    state: &AppState,
    phone: &str,
    submitted: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if submitted.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Verification code is required"})),
        ));
    }

    let now = chrono::Utc::now().timestamp();
    // Lockout, expiry and attempt counting all happen under one guard so
    // concurrent guesses cannot each get a free try.
    let mut entry = match state.sms.codes.get_mut(phone) {
        Some(e) => e,
        None => return Err(code_expired()),
    };
    if now < entry.locked_until {
        return Err(too_many_attempts(entry.locked_until - now));
    }
    if entry.code.is_empty() || now > entry.expires_at {
        return Err(code_expired());
    }
    if entry.attempts >= SMS_MAX_ATTEMPTS {
        entry.locked_until = now + SMS_LOCK_SECS;
        return Err(too_many_attempts(SMS_LOCK_SECS));
    }
    entry.attempts += 1;
    if entry.code != submitted {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Invalid verification code",
                "attempts_left": SMS_MAX_ATTEMPTS - entry.attempts,
            })),
        ));
    }
    // Blank the code in place rather than removing the entry: this makes it
    // single-use while `sent_today` and `locked_until` keep accumulating.
    entry.code.clear();
    entry.expires_at = now;
    Ok(())
}

fn code_expired() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": "Verification code has expired, please request a new one",
        })),
    )
}

fn too_many_attempts(retry_after: i64) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({
            "error": "Too many failed attempts, please try again later",
            "retry_after": retry_after,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_code_is_six_digits() {
        for _ in 0..200 {
            let code = generate_code();
            assert_eq!(code.len(), 6, "code {:?} is not 6 characters", code);
            assert!(code.chars().all(|c| c.is_ascii_digit()), "code {:?} has non-digits", code);
        }
    }
}
