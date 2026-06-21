//! Feishu event verification (signature check + challenge handshake).

use sha2::{Sha256, Digest};

/// Verify the X-Lark-Signature header for incoming events.
/// Feishu signs the request body with the verification token using HMAC-SHA256.
/// For simplicity, we verify the timestamp + nonce + body + token hash.
pub fn verify_signature(
    body: &[u8],
    timestamp: &str,
    nonce: &str,
    signature: &str,
    verification_token: &str,
) -> bool {
    // Feishu signature format: SHA256(timestamp + nonce + body + token)
    let mut hasher = Sha256::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(nonce.as_bytes());
    hasher.update(body);
    hasher.update(verification_token.as_bytes());
    let result = hasher.finalize();
    let computed = hex::encode(result);
    computed == signature
}

/// Build the challenge response for Feishu event subscription verification.
/// Feishu sends a challenge request when configuring the event URL.
pub fn challenge_response(challenge: &str, verification_token: &str) -> serde_json::Value {
    serde_json::json!({
        "challenge": challenge,
        "token": verification_token
    })
}
