//! Image captcha guarding the email registration form.
//!
//! Registration no longer proves mailbox control with an emailed code; the
//! human-check burden moves to this challenge instead. The code never exists
//! anywhere but inside the rendered image and the server-side entry — it is
//! not emailed, and it never appears in an HTTP response body.
//!
//! Storage mirrors the `CodeStore` philosophy but with captcha semantics: a
//! short TTL, and single use in the strongest sense — the entry is removed on
//! the first verification attempt whether the answer was right or wrong, so a
//! wrong guess cannot be retried against the same id (the client refreshes the
//! image). There is no throttle: issuing a captcha sends nothing, costs no
//! budget, and expires on its own.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use dashmap::DashMap;
use rand_core::{OsRng, RngCore};

use super::AppState;

/// Lifetime of a captcha once issued, in seconds.
pub const CAPTCHA_TTL_SECS: i64 = 300;

/// Generates one challenge: a fresh id, the secret text, and the image as a
/// ready-to-use `data:` URL (the renderer picks the format) that web
/// `<img src>` and the miniprogram `image` component both accept verbatim.
///
/// Pluggable for the same reason `MailSender` is: integration tests inject a
/// recording implementation instead of OCR-ing the real image, and a fixed or
/// readable code path never ships in production code.
pub trait CaptchaProvider: Send + Sync {
    fn generate(&self) -> (String, String, String);
}

/// The production generator, backed by the `captcha-rs` renderer.
pub struct RenderedCaptchaProvider;

impl CaptchaProvider for RenderedCaptchaProvider {
    fn generate(&self) -> (String, String, String) {
        let captcha = captcha_rs::CaptchaBuilder::new()
            .length(5)
            .width(160)
            .height(50)
            .complexity(4)
            .build();
        // `to_base64` already yields a complete `data:` URL (JPEG — the
        // renderer's output format); web <img> and the miniprogram <image>
        // both take it verbatim.
        let image = captcha.to_base64();
        (random_id(), captcha.text, image)
    }
}

fn random_id() -> String {
    let mut buf = [0u8; 16];
    OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Outstanding captchas: `id -> (text, expires_at)`.
pub struct CaptchaService {
    provider: Arc<dyn CaptchaProvider>,
    store: DashMap<String, (String, i64)>,
}

impl CaptchaService {
    pub fn new() -> Self {
        Self::with_provider(Arc::new(RenderedCaptchaProvider))
    }

    pub fn with_provider(provider: Arc<dyn CaptchaProvider>) -> Self {
        Self {
            provider,
            store: DashMap::new(),
        }
    }

    /// Mint a fresh challenge. Expired leftovers are swept opportunistically
    /// so a long-lived process cannot accumulate garbage.
    pub fn issue(&self) -> (String, String) {
        let cutoff = now();
        self.store.retain(|_, (_, expires_at)| *expires_at > cutoff);

        let (id, text, image) = self.provider.generate();
        self.store
            .insert(id.clone(), (text, now() + CAPTCHA_TTL_SECS));
        (id, image)
    }

    /// Spend the challenge: the entry is removed unconditionally, then the
    /// answer is judged. Unknown/expired id, or wrong text, all answer false.
    pub fn verify(&self, id: &str, submitted: &str) -> bool {
        let Some((text, expires_at)) = self.store.remove(id).map(|(_, v)| v) else {
            return false;
        };
        now() <= expires_at && text.eq_ignore_ascii_case(submitted.trim())
    }
}

/// `GET /api/auth/captcha` — public (pre-login by definition); hands out one
/// challenge. The response carries the image and the id the client must send
/// back with the registration attempt.
pub(crate) async fn get_captcha(State(state): State<AppState>) -> impl IntoResponse {
    let (captcha_id, image) = state.captcha.issue();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "captcha_id": captcha_id,
            "image": image,
            "expires_in": CAPTCHA_TTL_SECS,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic provider for unit tests: the code equals the id, so
    /// tests can read the id straight out of `issue`.
    struct EchoProvider;

    impl CaptchaProvider for EchoProvider {
        fn generate(&self) -> (String, String, String) {
            ("fixed-id".to_string(), "AB12C".to_string(), "data:image/png;base64,x".to_string())
        }
    }

    fn service() -> CaptchaService {
        CaptchaService::with_provider(Arc::new(EchoProvider))
    }

    #[test]
    fn verify_accepts_the_issued_code_case_insensitively() {
        let svc = service();
        let (id, image) = svc.issue();
        assert!(image.starts_with("data:"));
        assert!(svc.verify(&id, " ab12c "));
    }

    #[test]
    fn a_wrong_answer_burns_the_captcha() {
        let svc = service();
        let (id, _) = svc.issue();
        assert!(!svc.verify(&id, "nope"));
        // Single use even after a wrong guess: retrying with the right code
        // against the same id must fail.
        assert!(!svc.verify(&id, "AB12C"));
    }

    #[test]
    fn a_right_answer_is_also_single_use() {
        let svc = service();
        let (id, _) = svc.issue();
        assert!(svc.verify(&id, "AB12C"));
        assert!(!svc.verify(&id, "AB12C"));
    }

    #[test]
    fn unknown_id_never_verifies() {
        let svc = service();
        assert!(!svc.verify("does-not-exist", "AB12C"));
    }

    #[test]
    fn default_provider_renders_a_real_image() {
        let svc = CaptchaService::new();
        let (id, image) = svc.issue();
        assert_eq!(id.len(), 32, "id is 16 random bytes hex-encoded");
        assert!(image.starts_with("data:"), "clients get a ready data URL");
        assert!(image.len() > 200, "a real rendered image is never this small");
    }
}
