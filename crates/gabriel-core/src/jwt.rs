//! JWT inspection, locally.
//!
//! The workflow this replaces is pasting a live token into jwt.io — which means
//! handing a working credential to a third-party web page. Decoding is pure
//! base64 and JSON, so there is no reason for the token to leave the machine.
//!
//! This decodes and *inspects*; it does not verify signatures. Verification
//! needs the issuer's key, and a tool that said "valid" while only checking the
//! payload parsed would be worse than one that says nothing. What it does
//! instead is flag the things that are checkable without a key: expiry, the
//! `none` algorithm, and claims that look wrong.

use crate::error::{Error, Result};
use base64::Engine as _;
use serde_json::Value;

/// A decoded token, plus what could be judged without the signing key.
#[derive(Debug, Clone, PartialEq)]
pub struct Jwt {
    pub header: Value,
    pub payload: Value,
    /// Signature segment, still base64url — there is nothing to do with it here.
    pub signature: String,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warning {
    /// `alg: none` — an unsigned token. Accepting one is a classic auth bypass.
    NoSignatureAlgorithm,
    /// Signed with a symmetric algorithm; fine, but worth noticing when the
    /// issuer is supposed to be using a keypair.
    SymmetricAlgorithm,
    /// `exp` has passed.
    Expired,
    /// `nbf` is in the future.
    NotYetValid,
    /// No `exp` claim: the token is valid until the issuer revokes it.
    NoExpiry,
    /// An `exp`/`iat`/`nbf` that isn't a number, so no judgement is possible.
    UnreadableTimestamp,
    /// The signature segment is empty even though `alg` is not `none`.
    MissingSignature,
}

impl Warning {
    pub fn message(self) -> &'static str {
        match self {
            Warning::NoSignatureAlgorithm => {
                "alg is \"none\": this token is unsigned and anyone can forge one"
            }
            Warning::SymmetricAlgorithm => {
                "signed with a symmetric algorithm (HS*), so verification requires the shared secret"
            }
            Warning::Expired => "exp has passed: this token is expired",
            Warning::NotYetValid => "nbf is in the future: this token is not valid yet",
            Warning::NoExpiry => "no exp claim: this token does not expire on its own",
            Warning::UnreadableTimestamp => "a time claim is not a number, so validity is unknown",
            Warning::MissingSignature => "the signature segment is empty",
        }
    }

    /// Whether this is a security problem rather than an observation.
    pub fn is_serious(self) -> bool {
        matches!(
            self,
            Warning::NoSignatureAlgorithm | Warning::Expired | Warning::MissingSignature
        )
    }
}

impl Jwt {
    /// Decode `header.payload.signature`.
    pub fn decode(token: &str) -> Result<Self> {
        Self::decode_at(token, crate::now_ms())
    }

    /// Decode, judging time claims against a fixed instant. Exposed so tests
    /// don't depend on the clock.
    pub fn decode_at(token: &str, now_ms: u64) -> Result<Self> {
        // Tokens arrive with `Bearer ` attached more often than not.
        let token = token.trim();
        let token = token
            .strip_prefix("Bearer ")
            .or_else(|| token.strip_prefix("bearer "))
            .unwrap_or(token)
            .trim();

        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(Error::Invalid(format!(
                "not a JWT: expected 3 dot-separated segments, found {}",
                parts.len()
            )));
        }

        let header = decode_segment(parts[0], "header")?;
        let payload = decode_segment(parts[1], "payload")?;
        let signature = parts[2].to_string();

        let mut warnings = Vec::new();
        match header.get("alg").and_then(Value::as_str) {
            Some(alg) if alg.eq_ignore_ascii_case("none") => {
                warnings.push(Warning::NoSignatureAlgorithm)
            }
            Some(alg) if alg.starts_with("HS") => warnings.push(Warning::SymmetricAlgorithm),
            _ => {}
        }
        let unsigned = header
            .get("alg")
            .and_then(Value::as_str)
            .is_some_and(|a| a.eq_ignore_ascii_case("none"));
        if signature.is_empty() && !unsigned {
            warnings.push(Warning::MissingSignature);
        }

        match payload.get("exp") {
            Some(Value::Number(n)) => {
                let expires_ms = n.as_f64().unwrap_or(0.0) * 1000.0;
                if (expires_ms as u64) <= now_ms {
                    warnings.push(Warning::Expired);
                }
            }
            Some(_) => warnings.push(Warning::UnreadableTimestamp),
            None => warnings.push(Warning::NoExpiry),
        }

        if let Some(nbf) = payload.get("nbf") {
            match nbf {
                Value::Number(n) => {
                    if (n.as_f64().unwrap_or(0.0) * 1000.0) as u64 > now_ms {
                        warnings.push(Warning::NotYetValid);
                    }
                }
                _ => warnings.push(Warning::UnreadableTimestamp),
            }
        }

        Ok(Jwt { header, payload, signature, warnings })
    }

    pub fn algorithm(&self) -> Option<&str> {
        self.header.get("alg").and_then(Value::as_str)
    }

    pub fn key_id(&self) -> Option<&str> {
        self.header.get("kid").and_then(Value::as_str)
    }

    /// A time claim as epoch milliseconds.
    pub fn time_claim_ms(&self, claim: &str) -> Option<u64> {
        let seconds = self.payload.get(claim)?.as_f64()?;
        (seconds > 0.0).then_some((seconds * 1000.0) as u64)
    }

    /// Milliseconds until `exp`, negative once it has passed.
    pub fn expires_in_ms(&self, now_ms: u64) -> Option<i64> {
        Some(self.time_claim_ms("exp")? as i64 - now_ms as i64)
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_in_ms(now_ms).is_some_and(|remaining| remaining <= 0)
    }

    /// Claims a reader usually wants first, in a stable order.
    pub fn notable_claims(&self) -> Vec<(&str, String)> {
        const ORDER: &[&str] = &["iss", "sub", "aud", "azp", "scope", "scp", "client_id", "jti"];
        let mut out = Vec::new();
        for name in ORDER {
            if let Some(value) = self.payload.get(*name) {
                out.push((*name, render(value)));
            }
        }
        out
    }
}

fn decode_segment(segment: &str, what: &str) -> Result<Value> {
    // JWT uses base64url without padding; be lenient about receiving it padded.
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let bytes = engine
        .decode(segment.trim_end_matches('='))
        .map_err(|e| Error::Invalid(format!("{what} is not valid base64url: {e}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::Invalid(format!("{what} is not JSON: {e}")))
}

fn render(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(render).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// Find a JWT in an `Authorization` header value, or anywhere in a string.
///
/// Used to pull a token out of captured traffic without the developer having to
/// select it by hand.
pub fn find_in(text: &str) -> Option<&str> {
    text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
        .find(|candidate| looks_like_jwt(candidate))
}

fn looks_like_jwt(candidate: &str) -> bool {
    let parts: Vec<&str> = candidate.split('.').collect();
    if parts.len() != 3 || parts[0].len() < 4 || parts[1].len() < 2 {
        return false;
    }
    // The header segment always decodes to JSON starting with `{`.
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    engine
        .decode(parts[0].trim_end_matches('='))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|header| header.get("alg").is_some() || header.get("typ").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a token from parts, so tests don't carry opaque blobs.
    fn token(header: Value, payload: Value, signature: &str) -> String {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.{}",
            engine.encode(serde_json::to_vec(&header).unwrap()),
            engine.encode(serde_json::to_vec(&payload).unwrap()),
            signature
        )
    }

    const NOW: u64 = 1_785_283_200_000; // 2026-07-29T00:00:00Z

    fn valid_token() -> String {
        token(
            serde_json::json!({ "alg": "RS256", "typ": "JWT", "kid": "key-1" }),
            serde_json::json!({
                "iss": "https://auth.example.com",
                "sub": "user_123",
                "aud": ["api", "web"],
                "exp": NOW / 1000 + 3600,
                "iat": NOW / 1000
            }),
            "c2lnbmF0dXJl",
        )
    }

    #[test]
    fn decodes_header_payload_and_signature() {
        let jwt = Jwt::decode_at(&valid_token(), NOW).unwrap();
        assert_eq!(jwt.algorithm(), Some("RS256"));
        assert_eq!(jwt.key_id(), Some("key-1"));
        assert_eq!(jwt.payload["sub"], serde_json::json!("user_123"));
        assert_eq!(jwt.signature, "c2lnbmF0dXJl");
    }

    #[test]
    fn a_bearer_prefix_is_tolerated() {
        let with_prefix = format!("Bearer {}", valid_token());
        assert_eq!(
            Jwt::decode_at(&with_prefix, NOW).unwrap(),
            Jwt::decode_at(&valid_token(), NOW).unwrap()
        );
    }

    #[test]
    fn reports_time_remaining_and_expiry() {
        let jwt = Jwt::decode_at(&valid_token(), NOW).unwrap();
        assert_eq!(jwt.expires_in_ms(NOW), Some(3_600_000));
        assert!(!jwt.is_expired(NOW));
        assert!(jwt.is_expired(NOW + 3_600_001));
        assert!(!jwt.warnings.contains(&Warning::Expired));
    }

    #[test]
    fn an_expired_token_is_flagged() {
        let expired = token(
            serde_json::json!({ "alg": "RS256" }),
            serde_json::json!({ "exp": NOW / 1000 - 60 }),
            "sig",
        );
        let jwt = Jwt::decode_at(&expired, NOW).unwrap();
        assert!(jwt.warnings.contains(&Warning::Expired));
        assert!(jwt.warnings.iter().any(|w| w.is_serious()));
    }

    /// `alg: none` is an auth-bypass primitive, not a curiosity.
    #[test]
    fn the_none_algorithm_is_flagged_as_serious() {
        let unsigned = token(
            serde_json::json!({ "alg": "none" }),
            serde_json::json!({ "sub": "admin", "exp": NOW / 1000 + 60 }),
            "",
        );
        let jwt = Jwt::decode_at(&unsigned, NOW).unwrap();
        assert!(jwt.warnings.contains(&Warning::NoSignatureAlgorithm));
        assert!(Warning::NoSignatureAlgorithm.is_serious());
        // An empty signature is expected when alg is none, so no second warning.
        assert!(!jwt.warnings.contains(&Warning::MissingSignature));
    }

    #[test]
    fn a_signed_token_with_no_signature_is_flagged() {
        let mangled = token(
            serde_json::json!({ "alg": "RS256" }),
            serde_json::json!({ "exp": NOW / 1000 + 60 }),
            "",
        );
        let jwt = Jwt::decode_at(&mangled, NOW).unwrap();
        assert!(jwt.warnings.contains(&Warning::MissingSignature));
    }

    #[test]
    fn symmetric_algorithms_are_noted_but_not_serious() {
        let hs = token(
            serde_json::json!({ "alg": "HS256" }),
            serde_json::json!({ "exp": NOW / 1000 + 60 }),
            "sig",
        );
        let jwt = Jwt::decode_at(&hs, NOW).unwrap();
        assert!(jwt.warnings.contains(&Warning::SymmetricAlgorithm));
        assert!(!Warning::SymmetricAlgorithm.is_serious());
    }

    #[test]
    fn a_token_without_expiry_is_noted() {
        let forever = token(
            serde_json::json!({ "alg": "RS256" }),
            serde_json::json!({ "sub": "service-account" }),
            "sig",
        );
        assert!(Jwt::decode_at(&forever, NOW).unwrap().warnings.contains(&Warning::NoExpiry));
    }

    #[test]
    fn a_not_yet_valid_token_is_flagged() {
        let future = token(
            serde_json::json!({ "alg": "RS256" }),
            serde_json::json!({ "nbf": NOW / 1000 + 600, "exp": NOW / 1000 + 3600 }),
            "sig",
        );
        assert!(Jwt::decode_at(&future, NOW).unwrap().warnings.contains(&Warning::NotYetValid));
    }

    #[test]
    fn a_string_timestamp_is_reported_as_unreadable() {
        let odd = token(
            serde_json::json!({ "alg": "RS256" }),
            serde_json::json!({ "exp": "1785283200" }),
            "sig",
        );
        let jwt = Jwt::decode_at(&odd, NOW).unwrap();
        assert!(jwt.warnings.contains(&Warning::UnreadableTimestamp));
        // And no false claim of expiry either way.
        assert!(!jwt.warnings.contains(&Warning::Expired));
    }

    #[test]
    fn notable_claims_come_back_in_a_stable_order() {
        let jwt = Jwt::decode_at(&valid_token(), NOW).unwrap();
        let claims = jwt.notable_claims();
        assert_eq!(claims[0].0, "iss");
        assert_eq!(claims[1], ("sub", "user_123".to_string()));
        // Arrays are flattened for display.
        assert_eq!(claims[2], ("aud", "api, web".to_string()));
    }

    #[test]
    fn malformed_input_is_rejected_with_a_reason() {
        for (input, expected) in [
            ("not-a-jwt", "3 dot-separated"),
            ("a.b", "3 dot-separated"),
            ("!!!.eyJ9.sig", "base64url"),
            // Valid base64 that isn't JSON.
            ("aGVsbG8.aGVsbG8.sig", "not JSON"),
        ] {
            let error = Jwt::decode_at(input, NOW).unwrap_err().to_string();
            assert!(
                error.contains(expected),
                "decoding {input:?} gave an unhelpful error: {error}"
            );
        }
    }

    #[test]
    fn padded_base64_is_accepted() {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = engine.encode(br#"{"alg":"RS256"}"#);
        let payload = engine.encode(br#"{"sub":"x"}"#);
        let padded = format!("{header}=.{payload}==.sig");
        assert!(Jwt::decode_at(&padded, NOW).is_ok());
    }

    #[test]
    fn finds_a_token_inside_a_header_value() {
        let value = format!("Bearer {}", valid_token());
        assert_eq!(find_in(&value), Some(valid_token().as_str()));
    }

    #[test]
    fn finds_a_token_inside_a_json_body() {
        let body = format!("{{\"access_token\":\"{}\",\"type\":\"Bearer\"}}", valid_token());
        assert_eq!(find_in(&body), Some(valid_token().as_str()));
    }

    #[test]
    fn does_not_mistake_ordinary_text_for_a_token() {
        for text in [
            "no tokens here",
            "example.com/path",
            "1.2.3",
            "sk-live-abcdefghijklmnop",
            "Bearer opaque-session-token",
        ] {
            assert_eq!(find_in(text), None, "false positive on {text:?}");
        }
    }
}
