//! Channel-agnostic verification-code storage with throttling.
//!
//! Extracted from the SMS login flow so email verification enforces identical
//! rules: code TTL, resend cooldown, per-target daily limit, brute-force
//! lockout, and a process-wide daily budget with RAII reservation. Each
//! channel (SMS, email) holds its own `CodeStore`, so their global caps stay
//! independent — SMS is billed per message, email is not.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::{http::StatusCode, Json};
use dashmap::DashMap;
use rand_core::{OsRng, RngCore};

/// Lifetime of a code once sent, in seconds.
pub(crate) const CODE_TTL_SECS: i64 = 300;
/// Minimum gap between two sends to the same target.
pub(crate) const RESEND_COOLDOWN_SECS: i64 = 60;
/// Per-target ceiling within a single UTC day.
pub(crate) const PER_TARGET_DAILY: u32 = 10;
/// Wrong guesses before the code is locked out.
pub(crate) const MAX_ATTEMPTS: u8 = 5;
/// Length of the lockout, in seconds.
pub(crate) const LOCK_SECS: i64 = 900;

/// One row of the verification-code table, keyed by the normalized target
/// (phone number or email address).
#[derive(Debug, Clone)]
pub(crate) struct CodeEntry {
    pub(crate) code: String,
    pub(crate) expires_at: i64,
    /// Wrong guesses against the current code. Reset on every new send.
    pub(crate) attempts: u8,
    /// Brute-force lockout deadline; survives resends and successful logins.
    pub(crate) locked_until: i64,
    pub(crate) last_sent_at: i64,
    /// `YYYY-MM-DD` the counters below belong to. Storing it per entry is what
    /// keeps the daily limit a daily limit even after a long idle stretch.
    pub(crate) day_key: String,
    pub(crate) sent_today: u32,
}

/// Why [`CodeStore::issue`] refused to hand out a code.
#[derive(Debug)]
pub(crate) enum CodeError {
    /// Resend cooldown still active; carries `retry_after`.
    TooSoon(i64),
    /// The target hit its per-day ceiling.
    TargetDaily,
    /// The process-wide daily budget is spent.
    GlobalCap,
    /// Brute-force lockout still active; carries `retry_after`.
    Locked(i64),
}

impl CodeError {
    /// Map to the HTTP response. `channel` (`"SMS"`/`"Email"`) and `target`
    /// (`"phone number"`/`"email address"`) fill the two channel-specific
    /// messages; the others are shared verbatim across channels.
    pub(crate) fn into_response(self, channel: &str, target: &str) -> (StatusCode, Json<serde_json::Value>) {
        match self {
            CodeError::GlobalCap => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": format!("{channel} quota exhausted, please try again tomorrow"),
                })),
            ),
            CodeError::TooSoon(retry_after) => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "Please wait before requesting another code",
                    "retry_after": retry_after,
                })),
            ),
            CodeError::TargetDaily => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": format!("Daily limit reached for this {target}"),
                })),
            ),
            CodeError::Locked(retry_after) => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "Too many failed attempts, please try again later",
                    "retry_after": retry_after,
                })),
            ),
        }
    }
}

/// Why [`CodeStore::consume`] rejected a submitted code. These messages are
/// channel-neutral and identical for every channel.
#[derive(Debug)]
pub(crate) enum ConsumeError {
    Required,
    Expired,
    /// Brute-force lockout still active; carries `retry_after`.
    Locked(i64),
    /// Wrong code; carries `attempts_left`.
    Wrong(u8),
}

impl ConsumeError {
    pub(crate) fn into_response(self) -> (StatusCode, Json<serde_json::Value>) {
        match self {
            ConsumeError::Required => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Verification code is required",
                })),
            ),
            ConsumeError::Expired => (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Verification code has expired, please request a new one",
                })),
            ),
            ConsumeError::Locked(retry_after) => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "Too many failed attempts, please try again later",
                    "retry_after": retry_after,
                })),
            ),
            ConsumeError::Wrong(attempts_left) => (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Invalid verification code",
                    "attempts_left": attempts_left,
                })),
            ),
        }
    }
}

/// Generate a 6-digit code from the OS CSPRNG.
pub(crate) fn generate_code() -> String {
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    format!("{:06}", u64::from_be_bytes(bytes) % 1_000_000)
}

/// UTC day key that both the global and per-target counters roll over on.
fn today_key() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// The throttled code table plus the process-wide daily budget.
#[derive(Clone)]
pub(crate) struct CodeStore {
    entries: Arc<DashMap<String, CodeEntry>>,
    /// Messages sent process-wide on the current UTC day. Per-target throttling
    /// alone cannot stop an attacker enumerating targets and draining the
    /// budget, so this global ceiling is the real cost guard.
    sent_today: Arc<AtomicU32>,
    /// `YYYY-MM-DD` key for [`CodeStore::sent_today`], reset on rollover.
    day_key: Arc<Mutex<String>>,
    daily_global_cap: u32,
}

impl CodeStore {
    pub(crate) fn new(daily_global_cap: u32) -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
            sent_today: Arc::new(AtomicU32::new(0)),
            day_key: Arc::new(Mutex::new(String::new())),
            daily_global_cap,
        }
    }

    /// Reserve the throttling budget for one send and stage a fresh code.
    ///
    /// On success the caller owns a [`PendingSend`]: the code is already
    /// recorded in the table, so the *only* remaining obligations are to
    /// deliver it and then call [`PendingSend::commit`] — or let the
    /// `PendingSend` drop when delivery fails, which releases the budget slot
    /// and restores the target's previous entry. Every `Err` releases the
    /// budget internally, so handlers can return the mapped response as-is.
    pub(crate) fn issue(&self, target: &str) -> Result<(String, PendingSend), CodeError> {
        let now = chrono::Utc::now().timestamp();
        let day = today_key();

        // Sweep entries idle for over a day. The window has to outlive the code
        // TTL, otherwise `sent_today` resets every time the previous code
        // expires and the "daily" limit quietly degrades into a per-code limit.
        self.entries.retain(|_, e| e.last_sent_at > now - 24 * 3600);

        // Reserve a slot from the process-wide budget before touching
        // per-target state, so that once it is exhausted an attacker cannot
        // probe which targets exist.
        let slot = {
            // A poisoned lock only matters if a holder panicked mid-update; the
            // string is still usable, so recover rather than 500 the request.
            let mut guard = self.day_key.lock().unwrap_or_else(|e| e.into_inner());
            if *guard != day {
                *guard = day.clone();
                self.sent_today.store(0, Ordering::SeqCst);
            }
            BudgetSlot::try_reserve(self.sent_today.clone(), self.daily_global_cap)
        };
        let slot = match slot {
            Some(s) => s,
            None => return Err(CodeError::GlobalCap),
        };

        // Snapshot for rollback: a send the provider rejected must not consume
        // the target's cooldown or per-day allowance.
        let previous = self.entries.get(target).map(|e| e.clone());

        let code = generate_code();
        {
            // Every throttle check and every mutation lives inside this one
            // guard so two concurrent requests to the same target cannot both
            // pass cooldown.
            let mut entry = self
                .entries
                .entry(target.to_string())
                .or_insert_with(|| CodeEntry {
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
                return Err(CodeError::Locked(entry.locked_until - now));
            }
            if now - entry.last_sent_at < RESEND_COOLDOWN_SECS {
                return Err(CodeError::TooSoon(
                    RESEND_COOLDOWN_SECS - (now - entry.last_sent_at),
                ));
            }
            if entry.sent_today >= PER_TARGET_DAILY {
                return Err(CodeError::TargetDaily);
            }
            entry.code = code.clone();
            entry.expires_at = now + CODE_TTL_SECS;
            entry.attempts = 0;
            entry.last_sent_at = now;
            entry.sent_today += 1;
        }
        // The entry guard is dropped here. Holding a DashMap reference across
        // an await in the caller would block every other request touching this
        // shard.

        Ok((
            code,
            PendingSend {
                entries: self.entries.clone(),
                target: target.to_string(),
                previous,
                slot: Some(slot),
                committed: false,
            },
        ))
    }

    /// Verify `submitted` against the outstanding code for `target` and spend
    /// it. On `Ok` the code is single-use-consumed; attempt counters and the
    /// lockout deadline keep accumulating either way.
    pub(crate) fn consume(&self, target: &str, submitted: &str) -> Result<(), ConsumeError> {
        if submitted.is_empty() {
            return Err(ConsumeError::Required);
        }

        let now = chrono::Utc::now().timestamp();
        // Lockout, expiry and attempt counting all happen under one guard so
        // concurrent guesses cannot each get a free try.
        let mut entry = match self.entries.get_mut(target) {
            Some(e) => e,
            None => return Err(ConsumeError::Expired),
        };
        if now < entry.locked_until {
            return Err(ConsumeError::Locked(entry.locked_until - now));
        }
        if entry.code.is_empty() || now > entry.expires_at {
            return Err(ConsumeError::Expired);
        }
        if entry.attempts >= MAX_ATTEMPTS {
            entry.locked_until = now + LOCK_SECS;
            return Err(ConsumeError::Locked(LOCK_SECS));
        }
        entry.attempts += 1;
        if entry.code != submitted {
            return Err(ConsumeError::Wrong(MAX_ATTEMPTS - entry.attempts));
        }
        // Blank the code in place rather than removing the entry: this makes it
        // single-use while `sent_today` and `locked_until` keep accumulating.
        entry.code.clear();
        entry.expires_at = now;
        Ok(())
    }
}

/// A staged-but-undelivered code plus the budget it reserved.
///
/// The RAII contract: [`commit`](PendingSend::commit) keeps both the budget
/// slot and the new table entry; any other drop path releases the slot and
/// restores the target's previous entry, so a failed send leaves no trace a
/// user or attacker could observe.
pub(crate) struct PendingSend {
    entries: Arc<DashMap<String, CodeEntry>>,
    target: String,
    previous: Option<CodeEntry>,
    slot: Option<BudgetSlot>,
    committed: bool,
}

impl PendingSend {
    /// The message is on its way: keep the budget slot and the table entry.
    pub(crate) fn commit(mut self) {
        self.committed = true;
        if let Some(slot) = self.slot.take() {
            slot.commit();
        }
    }
}

impl Drop for PendingSend {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // Restore the pre-issue snapshot: the cooldown and per-day counters
        // must not record a message that never left the building.
        match self.previous.take() {
            Some(entry) => {
                self.entries.insert(self.target.clone(), entry);
            }
            None => {
                self.entries.remove(&self.target);
            }
        }
        // Dropping the slot releases the global budget slot.
        self.slot = None;
    }
}

/// One reserved slot of the process-wide daily budget.
///
/// Reserving before the send rather than counting afterwards is what keeps the
/// cap honest: a post-hoc counter lets a burst of N concurrent requests all
/// pass the same check and overshoot by N. Every path between the reservation
/// and a successful delivery has to give the slot back, and an RAII guard is
/// the only way to guarantee that without repeating the release at each early
/// return.
struct BudgetSlot {
    counter: Arc<AtomicU32>,
    committed: bool,
}

impl BudgetSlot {
    /// Atomically take one slot, or return `None` when the budget is spent.
    fn try_reserve(counter: Arc<AtomicU32>, cap: u32) -> Option<Self> {
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

impl Drop for BudgetSlot {
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

    #[test]
    fn issue_then_consume_round_trip() {
        let store = CodeStore::new(10);
        let (code, pending) = store.issue("t1").unwrap();
        pending.commit();
        assert_eq!(code.len(), 6);
        assert!(store.consume("t1", &code).is_ok());
        // Single use: the spent code no longer verifies.
        assert!(matches!(
            store.consume("t1", &code),
            Err(ConsumeError::Expired)
        ));
    }

    #[test]
    fn wrong_code_counts_attempts_then_locks() {
        let store = CodeStore::new(10);
        let (code, pending) = store.issue("t2").unwrap();
        pending.commit();
        for i in 1..=MAX_ATTEMPTS {
            match store.consume("t2", "000000") {
                Err(ConsumeError::Wrong(left)) => assert_eq!(left, MAX_ATTEMPTS - i),
                other => panic!("expected Wrong, got {other:?}"),
            }
        }
        // The 5th wrong guess armed the lockout — even the correct code is now
        // refused until it expires.
        assert!(matches!(
            store.consume("t2", &code),
            Err(ConsumeError::Locked(_))
        ));
    }

    #[test]
    fn missing_or_blank_code_is_expired_or_required() {
        let store = CodeStore::new(10);
        assert!(matches!(
            store.consume("never-issued", "123456"),
            Err(ConsumeError::Expired)
        ));
        let (_, pending) = store.issue("t3").unwrap();
        pending.commit();
        assert!(matches!(
            store.consume("t3", ""),
            Err(ConsumeError::Required)
        ));
    }

    #[test]
    fn resend_within_cooldown_is_rejected() {
        let store = CodeStore::new(10);
        let (_, pending) = store.issue("t4").unwrap();
        pending.commit();
        // No sleep needed: the second call is by construction within the
        // 60-second cooldown.
        assert!(matches!(
            store.issue("t4"),
            Err(CodeError::TooSoon(retry_after)) if retry_after > 0 && retry_after <= RESEND_COOLDOWN_SECS
        ));
    }

    #[test]
    fn dropped_pending_send_rolls_back_cooldown_and_budget() {
        let store = CodeStore::new(1);
        let (_, pending) = store.issue("t5").unwrap();
        drop(pending); // delivery "failed"

        // The budget slot came back, so a fresh target can issue again.
        assert!(store.issue("t6").is_ok());
        // And the failed target did not enter cooldown.
        assert!(store.issue("t5").is_ok());
    }

    #[test]
    fn global_cap_stops_the_whole_process() {
        let store = CodeStore::new(2);
        let (_, a) = store.issue("a").unwrap();
        a.commit();
        let (_, b) = store.issue("b").unwrap();
        b.commit();
        assert!(matches!(store.issue("c"), Err(CodeError::GlobalCap)));
    }

    #[test]
    fn sms_error_strings_are_byte_identical_to_the_originals() {
        let (status, body) = CodeError::GlobalCap.into_response("SMS", "phone number");
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body.get("error").unwrap(), "SMS quota exhausted, please try again tomorrow");

        let (_, body) = CodeError::TargetDaily.into_response("SMS", "phone number");
        assert_eq!(body.get("error").unwrap(), "Daily limit reached for this phone number");

        let (_, body) = CodeError::TooSoon(30).into_response("SMS", "phone number");
        assert_eq!(body.get("error").unwrap(), "Please wait before requesting another code");
        assert_eq!(body.get("retry_after").unwrap(), 30);

        let (_, body) = CodeError::Locked(900).into_response("SMS", "phone number");
        assert_eq!(body.get("error").unwrap(), "Too many failed attempts, please try again later");

        let (status, body) = ConsumeError::Wrong(3).into_response();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body.get("error").unwrap(), "Invalid verification code");
        assert_eq!(body.get("attempts_left").unwrap(), 3);
    }
}
