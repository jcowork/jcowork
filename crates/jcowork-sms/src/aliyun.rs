//! Aliyun SMS (dysmsapi) sender using the RPC-style HMAC-SHA1 signature.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use sha1::Sha1;

use crate::provider::SmsSender;

/// RFC 3986 unreserved characters stay literal; everything else is `%XX`.
/// `NON_ALPHANUMERIC` alone would also encode `-_.~`, which Aliyun's
/// canonicalization does not want.
const RFC3986: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

const ENDPOINT: &str = "https://dysmsapi.aliyuncs.com/";
const API_VERSION: &str = "2017-05-25";

pub struct AliyunSmsSender {
    access_key_id: String,
    access_key_secret: String,
    sign_name: String,
    template_code: String,
    http: reqwest::Client,
}

impl AliyunSmsSender {
    pub fn new(
        access_key_id: String,
        access_key_secret: String,
        sign_name: String,
        template_code: String,
    ) -> Result<Self> {
        Ok(Self {
            access_key_id,
            access_key_secret,
            sign_name,
            template_code,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()?,
        })
    }
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, RFC3986).to_string()
}

/// `HTTPMethod&%2F&<percent-encoded canonicalized query string>`.
///
/// The double encoding is deliberate and is the most common source of
/// `SignatureDoesNotMatch`.
fn string_to_sign(params: &BTreeMap<String, String>) -> String {
    let canonical: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("GET&{}&{}", encode("/"), encode(&canonical))
}

/// The signing key is the access key secret with a trailing `&` appended
/// (placeholder for an empty STS security token). Omitting it is the second
/// most common cause of `SignatureDoesNotMatch`.
fn sign(params: &BTreeMap<String, String>, access_key_secret: &str) -> String {
    let mut mac =
        Hmac::<Sha1>::new_from_slice(format!("{}&", access_key_secret).as_bytes())
            .expect("HMAC-SHA1 accepts any key length");
    mac.update(string_to_sign(params).as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

#[async_trait]
impl SmsSender for AliyunSmsSender {
    async fn send_code(&self, phone: &str, code: &str) -> Result<()> {
        // BTreeMap keeps the byte-ascending key order the signature requires.
        let mut params = BTreeMap::new();
        params.insert("AccessKeyId".to_string(), self.access_key_id.clone());
        params.insert("Action".to_string(), "SendSms".to_string());
        params.insert("Format".to_string(), "JSON".to_string());
        params.insert("PhoneNumbers".to_string(), phone.to_string());
        params.insert("SignName".to_string(), self.sign_name.clone());
        params.insert("SignatureMethod".to_string(), "HMAC-SHA1".to_string());
        params.insert("SignatureNonce".to_string(), uuid::Uuid::new_v4().to_string());
        params.insert("SignatureVersion".to_string(), "1.0".to_string());
        params.insert("TemplateCode".to_string(), self.template_code.clone());
        params.insert(
            "TemplateParam".to_string(),
            serde_json::json!({ "code": code }).to_string(),
        );
        params.insert(
            "Timestamp".to_string(),
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        );
        params.insert("Version".to_string(), API_VERSION.to_string());

        let signature = sign(&params, &self.access_key_secret);
        params.insert("Signature".to_string(), signature);

        let resp = self.http.get(ENDPOINT).query(&params).send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;

        // Aliyun reports business failures inside an HTTP 200, so the `Code`
        // field is authoritative. `isv.BUSINESS_LIMIT_CONTROL` means Aliyun's
        // own per-number throttle (1/min, 5/hour, 10/day) rejected us.
        if body["Code"].as_str() != Some("OK") {
            tracing::warn!(
                %status,
                code = %body["Code"],
                message = %body["Message"],
                request_id = %body["RequestId"],
                "Aliyun SendSms rejected"
            );
            anyhow::bail!("SMS provider error: {}", body["Code"]);
        }

        Ok(())
    }

    fn name(&self) -> &'static str {
        "aliyun"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_params() -> BTreeMap<String, String> {
        let mut p = BTreeMap::new();
        p.insert("AccessKeyId".to_string(), "testid".to_string());
        p.insert("Action".to_string(), "SendSms".to_string());
        p.insert("PhoneNumbers".to_string(), "13800138000".to_string());
        p
    }

    #[test]
    fn keeps_rfc3986_unreserved_and_encodes_space_as_percent20() {
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode(" "), "%20");
        assert_eq!(encode("*"), "%2A");
    }

    #[test]
    fn encodes_json_template_param() {
        assert_eq!(
            encode(r#"{"code":"1234"}"#),
            "%7B%22code%22%3A%221234%22%7D"
        );
    }

    #[test]
    fn string_to_sign_is_double_encoded() {
        let mut p = BTreeMap::new();
        p.insert("A".to_string(), "a".to_string());
        p.insert("B".to_string(), "b".to_string());
        // Inner "&" and "=" separators must come back out as %26 / %3D.
        assert_eq!(string_to_sign(&p), "GET&%2F&A%3Da%26B%3Db");
    }

    #[test]
    fn params_are_sorted_by_key_byte_order() {
        let mut p = fixed_params();
        p.insert("Version".to_string(), API_VERSION.to_string());
        p.insert("SignatureNonce".to_string(), "n".to_string());
        let sts = string_to_sign(&p);
        let keys: Vec<&str> = sts
            .rsplit_once('&')
            .unwrap()
            .1
            .split("%26")
            .map(|pair| pair.split("%3D").next().unwrap())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    /// RFC 2202 test case 1 — validates the HMAC-SHA1 wiring independently of
    /// any Aliyun-specific canonicalization.
    #[test]
    fn hmac_sha1_matches_rfc2202_vector() {
        let key = [0x0bu8; 20];
        let mut mac = Hmac::<Sha1>::new_from_slice(&key).unwrap();
        mac.update(b"Hi There");
        assert_eq!(
            hex_of(&mac.finalize().into_bytes()),
            "b617318655057264e28bc0b6fb378c8ef146be00"
        );
    }

    #[test]
    fn signing_key_appends_trailing_ampersand() {
        let p = fixed_params();
        let via_sign = sign(&p, "secret");

        let mut mac = Hmac::<Sha1>::new_from_slice(b"secret&").unwrap();
        mac.update(string_to_sign(&p).as_bytes());
        let manual = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert_eq!(via_sign, manual);
    }

    #[test]
    fn signature_is_deterministic_for_identical_params() {
        let p = fixed_params();
        assert_eq!(sign(&p, "secret"), sign(&p, "secret"));
        assert_ne!(sign(&p, "secret"), sign(&p, "other"));
    }

    fn hex_of(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}
