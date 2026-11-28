//! Reading and validating an ID token (OpenID Connect Core section 3.1.3.7).
//!
//! The signature itself is checked by `ring`. This module only splits the
//! compact form and checks the claims, with no option that loosens anything.

use serde::Deserialize;

use super::jwks::{Algorithm, VerificationKey, usable_key_id};
use crate::json::parse_object;
use crate::secret::{base64url_decode, constant_time_eq, log_safe};

/// Allowance for the provider's clock and ours disagreeing: 60 seconds, on
/// `exp`, `iat` and `nbf`.
pub(crate) const CLOCK_SKEW_SECS: u64 = 60;
/// No real ID token comes near this.
const MAX_TOKEN_BYTES: usize = 64 * 1024;

/// Why an ID token was refused. Safe to log: it never contains the token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdTokenError {
    /// Not three base64url parts with a JSON header.
    #[error("it is not a well-formed signed token")]
    Malformed,
    /// `alg` is not one this crate verifies. `none` and `HS256` end here.
    #[error("it is signed with the unsupported algorithm \"{0}\"")]
    UnsupportedAlgorithm(String),
    /// The header has no `kid`, or one this crate will not look up.
    #[error("it does not name its signing key")]
    NoKeyId,
    /// The provider publishes no usable key with that `kid`, also after a
    /// refresh or when a refresh was not due.
    #[error("it names the signing key \"{0}\", which the provider does not publish")]
    UnknownKey(String),
    /// No published key with that `kid` and algorithm verifies it.
    #[error("its signature does not verify")]
    BadSignature,
    /// A required claim is missing or has the wrong type.
    #[error("its claims cannot be read: {0}")]
    MalformedClaims(String),
    /// `iss` is not the provider's issuer.
    #[error("its issuer is not the configured provider")]
    WrongIssuer,
    /// `aud` does not contain the client id.
    #[error("its audience does not include this client")]
    WrongAudience,
    /// `azp` is absent with several audiences, or is another client.
    #[error("its authorized party is missing or is not this client")]
    WrongAuthorizedParty,
    /// `exp` is more than the clock skew in the past.
    #[error("it has expired")]
    Expired,
    /// `iat` is more than the clock skew in the future.
    #[error("it was issued in the future")]
    IssuedInFuture,
    /// `nbf` is more than the clock skew in the future.
    #[error("it is not valid yet")]
    NotYetValid,
    /// `nonce` is absent or is not the pending login's.
    #[error("its nonce is missing or belongs to another login")]
    WrongNonce,
    /// `sub` is empty, or unusable as an identity.
    #[error("its subject is missing or unusable")]
    BadSubject,
    /// `email_verified` is true but `email` is not plausibly an address.
    #[error("its verified email is not a plausible address")]
    BadEmail,
}

/// A token split into its parts, signature not yet checked.
pub(crate) struct UnverifiedToken<'a> {
    pub(crate) algorithm: Algorithm,
    pub(crate) key_id: String,
    /// The exact bytes the signature covers: header, dot, payload.
    signing_input: &'a [u8],
    signature: Vec<u8>,
    payload: Vec<u8>,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: Option<String>,
    crit: Option<serde_json::Value>,
}

impl<'a> UnverifiedToken<'a> {
    pub(crate) fn parse(token: &'a str) -> Result<Self, IdTokenError> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(IdTokenError::Malformed);
        }
        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(IdTokenError::Malformed);
        };
        let decode = |part| base64url_decode(part).ok_or(IdTokenError::Malformed);
        let header: Header = parse_object(&decode(header)?).map_err(|_| IdTokenError::Malformed)?;
        let algorithm = Algorithm::from_name(&header.alg)
            .ok_or_else(|| IdTokenError::UnsupportedAlgorithm(log_safe(&header.alg)))?;
        // A critical extension this code does not understand must be
        // refused (RFC 7515 section 4.1.11), and it understands none.
        if header.crit.is_some() {
            return Err(IdTokenError::Malformed);
        }
        let key_id = usable_key_id(header.kid).ok_or(IdTokenError::NoKeyId)?;
        // The signature is the last part, so what it covers ends one byte
        // (the dot) before it.
        let signing_input = &token.as_bytes()[..token.len() - signature.len() - 1];
        Ok(Self {
            algorithm,
            key_id,
            signing_input,
            signature: decode(signature)?,
            payload: decode(payload)?,
        })
    }

    /// Checks the signature against `keys` and hands out the payload only if
    /// one of them verifies it. A key is tried only if it carries the
    /// token's key id and its own algorithm is the token's.
    pub(crate) fn verify(&self, keys: &[VerificationKey]) -> Result<&[u8], IdTokenError> {
        let mut candidates = keys
            .iter()
            .filter(|key| key.key_id == self.key_id)
            .peekable();
        if candidates.peek().is_none() {
            return Err(IdTokenError::UnknownKey(log_safe(&self.key_id)));
        }
        let verified = candidates.any(|key| {
            key.algorithm() == self.algorithm && key.verifies(self.signing_input, &self.signature)
        });
        if verified {
            Ok(&self.payload)
        } else {
            Err(IdTokenError::BadSignature)
        }
    }
}

/// What the claims must agree with.
pub(crate) struct Expected<'a> {
    /// The issuer as the discovery document spells it.
    pub(crate) issuer: &'a str,
    pub(crate) client_id: &'a str,
    /// The pending login's nonce.
    pub(crate) nonce: &'a str,
    /// Seconds since the Unix epoch.
    pub(crate) now: u64,
}

/// The claims sign-in uses, from a token whose signature and claims are valid.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Claims {
    pub(crate) subject: String,
    pub(crate) email: Option<String>,
    /// True only for the JSON boolean `true`. A string "true", as a few
    /// providers send, does not count.
    pub(crate) email_verified: bool,
}

#[derive(Deserialize)]
struct RawClaims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: f64,
    iat: f64,
    nbf: Option<f64>,
    nonce: Option<String>,
    azp: Option<String>,
    email: Option<serde_json::Value>,
    email_verified: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

/// Validates the payload of a token whose signature has been verified.
pub(crate) fn validate_claims(
    payload: &[u8],
    expected: &Expected<'_>,
) -> Result<Claims, IdTokenError> {
    let raw: RawClaims = parse_object(payload).map_err(IdTokenError::MalformedClaims)?;
    // In this order: when several checks fail, the first one names the error.
    raw.check_issuer(expected)?;
    raw.check_audience(expected)?;
    raw.check_times(expected.now)?;
    raw.check_nonce(expected)?;
    raw.check_subject()?;
    Ok(raw.into_claims())
}

impl Audience {
    fn as_slice(&self) -> &[String] {
        match self {
            Self::One(audience) => std::slice::from_ref(audience),
            Self::Many(audiences) => audiences.as_slice(),
        }
    }
}

impl RawClaims {
    fn check_issuer(&self, expected: &Expected<'_>) -> Result<(), IdTokenError> {
        if self.iss != expected.issuer {
            return Err(IdTokenError::WrongIssuer);
        }
        Ok(())
    }

    /// `aud` must name this client, and with several audiences only `azp`
    /// says the token was issued to it; when present `azp` must say so in
    /// every case.
    fn check_audience(&self, expected: &Expected<'_>) -> Result<(), IdTokenError> {
        let audiences = self.aud.as_slice();
        if !audiences
            .iter()
            .any(|audience| audience == expected.client_id)
        {
            return Err(IdTokenError::WrongAudience);
        }
        match &self.azp {
            Some(party) if party != expected.client_id => Err(IdTokenError::WrongAuthorizedParty),
            None if audiences.len() > 1 => Err(IdTokenError::WrongAuthorizedParty),
            _ => Ok(()),
        }
    }

    /// `exp`, then `iat`, then `nbf`, each with [`CLOCK_SKEW_SECS`] of
    /// allowance. A time that is not one is refused when its own check runs.
    fn check_times(&self, now: u64) -> Result<(), IdTokenError> {
        let soon = now.saturating_add(CLOCK_SKEW_SECS);
        if now >= seconds(self.exp, "exp")?.saturating_add(CLOCK_SKEW_SECS) {
            return Err(IdTokenError::Expired);
        }
        if seconds(self.iat, "iat")? > soon {
            return Err(IdTokenError::IssuedInFuture);
        }
        if let Some(not_before) = self.nbf
            && seconds(not_before, "nbf")? > soon
        {
            return Err(IdTokenError::NotYetValid);
        }
        Ok(())
    }

    fn check_nonce(&self, expected: &Expected<'_>) -> Result<(), IdTokenError> {
        let nonce_matches = self
            .nonce
            .as_deref()
            .is_some_and(|nonce| constant_time_eq(nonce.as_bytes(), expected.nonce.as_bytes()));
        if !nonce_matches {
            return Err(IdTokenError::WrongNonce);
        }
        Ok(())
    }

    fn check_subject(&self) -> Result<(), IdTokenError> {
        if self.sub.is_empty() {
            return Err(IdTokenError::BadSubject);
        }
        Ok(())
    }

    fn into_claims(self) -> Claims {
        Claims {
            subject: self.sub,
            email: match self.email {
                Some(serde_json::Value::String(email)) => Some(email),
                _ => None,
            },
            email_verified: self.email_verified == Some(serde_json::Value::Bool(true)),
        }
    }
}

/// A time claim in whole seconds since the epoch.
fn seconds(value: f64, claim: &str) -> Result<u64, IdTokenError> {
    if value.is_finite() && value >= 0.0 {
        // Saturates far beyond any real date.
        Ok(value as u64)
    } else {
        Err(IdTokenError::MalformedClaims(format!(
            "{claim} is not a time"
        )))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::secret::base64url;

    const NOW: u64 = 1_800_000_000;

    fn expected() -> Expected<'static> {
        Expected {
            issuer: "https://idp.example.com/",
            client_id: "calin",
            nonce: "nonce-1",
            now: NOW,
        }
    }

    fn claims() -> Value {
        json!({
            "iss": "https://idp.example.com/",
            "sub": "user-1",
            "aud": "calin",
            "exp": NOW + 300,
            "iat": NOW,
            "nonce": "nonce-1",
            "email": "alice@example.com",
            "email_verified": true,
        })
    }

    fn check(value: &Value) -> Result<Claims, IdTokenError> {
        validate_claims(value.to_string().as_bytes(), &expected())
    }

    fn with(name: &str, value: Value) -> Value {
        let mut all = claims();
        all[name] = value;
        all
    }

    fn without(name: &str) -> Value {
        let mut all = claims();
        all.as_object_mut().unwrap().remove(name);
        all
    }

    #[test]
    fn good_claims_are_accepted() {
        assert_eq!(
            check(&claims()).unwrap(),
            Claims {
                subject: "user-1".to_owned(),
                email: Some("alice@example.com".to_owned()),
                email_verified: true,
            }
        );
    }

    #[test]
    fn issuer_must_be_exact() {
        for issuer in [
            "https://idp.example.com",
            "https://IDP.example.com/",
            "https://evil.example/",
            "",
        ] {
            assert_eq!(
                check(&with("iss", json!(issuer))),
                Err(IdTokenError::WrongIssuer),
                "{issuer}"
            );
        }
    }

    #[test]
    fn audience_table() {
        let ok = |aud: Value, azp: Option<&str>| {
            let mut all = with("aud", aud);
            if let Some(azp) = azp {
                all["azp"] = json!(azp);
            }
            check(&all).map(|_| ())
        };
        assert_eq!(ok(json!("calin"), None), Ok(()));
        assert_eq!(ok(json!("calin"), Some("calin")), Ok(()));
        assert_eq!(ok(json!(["calin"]), None), Ok(()));
        assert_eq!(ok(json!(["calin", "other-api"]), Some("calin")), Ok(()));
        assert_eq!(ok(json!(["other-api", "calin"]), Some("calin")), Ok(()));

        assert_eq!(ok(json!("other"), None), Err(IdTokenError::WrongAudience));
        assert_eq!(ok(json!("Calin"), None), Err(IdTokenError::WrongAudience));
        assert_eq!(ok(json!("calin2"), None), Err(IdTokenError::WrongAudience));
        assert_eq!(ok(json!([]), None), Err(IdTokenError::WrongAudience));
        assert_eq!(
            ok(json!(["other", "another"]), Some("calin")),
            Err(IdTokenError::WrongAudience)
        );
        // Several audiences need azp, and azp must always be this client.
        assert_eq!(
            ok(json!(["calin", "other-api"]), None),
            Err(IdTokenError::WrongAuthorizedParty)
        );
        assert_eq!(
            ok(json!(["calin", "other-api"]), Some("other-api")),
            Err(IdTokenError::WrongAuthorizedParty)
        );
        assert_eq!(
            ok(json!("calin"), Some("other")),
            Err(IdTokenError::WrongAuthorizedParty)
        );
        assert_eq!(
            ok(json!(["calin"]), Some("other")),
            Err(IdTokenError::WrongAuthorizedParty)
        );
    }

    #[test]
    fn time_claims_with_sixty_seconds_of_skew() {
        let times = |exp: u64, iat: u64, nbf: Option<u64>| {
            let mut all = claims();
            all["exp"] = json!(exp);
            all["iat"] = json!(iat);
            if let Some(nbf) = nbf {
                all["nbf"] = json!(nbf);
            }
            check(&all).map(|_| ())
        };
        // Expiry: valid until exp + 60, exclusive.
        assert_eq!(times(NOW + 1, NOW, None), Ok(()));
        assert_eq!(times(NOW, NOW - 10, None), Ok(()));
        assert_eq!(times(NOW - 59, NOW - 100, None), Ok(()));
        assert_eq!(times(NOW - 60, NOW - 100, None), Err(IdTokenError::Expired));
        assert_eq!(
            times(NOW - 3600, NOW - 7200, None),
            Err(IdTokenError::Expired)
        );
        assert_eq!(times(0, 0, None), Err(IdTokenError::Expired));
        // Issued-at: up to 60 seconds ahead.
        assert_eq!(times(NOW + 300, NOW + 60, None), Ok(()));
        assert_eq!(
            times(NOW + 300, NOW + 61, None),
            Err(IdTokenError::IssuedInFuture)
        );
        assert_eq!(
            times(NOW + 90_000, NOW + 86_400, None),
            Err(IdTokenError::IssuedInFuture)
        );
        // Not-before: same allowance.
        assert_eq!(times(NOW + 300, NOW, Some(NOW - 5)), Ok(()));
        assert_eq!(times(NOW + 300, NOW, Some(NOW + 60)), Ok(()));
        assert_eq!(
            times(NOW + 300, NOW, Some(NOW + 61)),
            Err(IdTokenError::NotYetValid)
        );
    }

    #[test]
    fn fractional_times_are_accepted_and_other_types_are_not() {
        assert!(check(&with("exp", json!(NOW as f64 + 300.5))).is_ok());
        assert!(check(&with("nbf", Value::Null)).is_ok());
        for (name, value) in [
            ("exp", json!("1800000300")),
            ("exp", json!(-1)),
            ("exp", Value::Null),
            ("iat", json!("now")),
            ("iat", json!(-5.5)),
            ("iat", json!([NOW])),
            ("nbf", json!("0")),
            ("nbf", json!(-1)),
        ] {
            let result = check(&with(name, value.clone()));
            assert!(
                matches!(result, Err(IdTokenError::MalformedClaims(_))),
                "{name} = {value}"
            );
        }
    }

    #[test]
    fn nonce_must_be_present_and_equal() {
        assert_eq!(check(&without("nonce")), Err(IdTokenError::WrongNonce));
        for nonce in ["", "nonce-2", "nonce-1 ", "NONCE-1", "nonce-11"] {
            assert_eq!(
                check(&with("nonce", json!(nonce))),
                Err(IdTokenError::WrongNonce),
                "{nonce:?}"
            );
        }
    }

    #[test]
    fn required_claims_must_be_present_with_the_right_type() {
        for name in ["iss", "sub", "aud", "exp", "iat"] {
            let result = check(&without(name));
            assert!(
                matches!(&result, Err(IdTokenError::MalformedClaims(why)) if why.contains(name)),
                "{name}: {result:?}"
            );
        }
        for (name, value) in [
            ("sub", json!(42)),
            ("aud", json!(7)),
            ("aud", json!(["calin", 7])),
            ("iss", json!(["x"])),
            ("azp", json!(1)),
            ("nonce", json!(1)),
        ] {
            assert!(
                matches!(
                    check(&with(name, value)),
                    Err(IdTokenError::MalformedClaims(_))
                ),
                "{name}"
            );
        }
        assert_eq!(
            check(&with("sub", json!(""))),
            Err(IdTokenError::BadSubject)
        );
        for body in ["", "[]", "null", "\"claims\"", "{"] {
            assert!(matches!(
                validate_claims(body.as_bytes(), &expected()),
                Err(IdTokenError::MalformedClaims(_))
            ));
        }
    }

    #[test]
    fn the_first_failing_check_names_the_error() {
        // Bad in every way; each step fixes the claim the last error named.
        let mut all = json!({
            "iss": "https://evil.example/",
            "sub": "",
            "aud": ["other", "another"],
            "exp": NOW - 3600,
            "iat": NOW + 3600,
            "nbf": NOW + 3600,
            "nonce": "nonce-2",
        });
        let steps = [
            (
                IdTokenError::WrongIssuer,
                "iss",
                json!("https://idp.example.com/"),
            ),
            (
                IdTokenError::WrongAudience,
                "aud",
                json!(["calin", "other-api"]),
            ),
            (IdTokenError::WrongAuthorizedParty, "azp", json!("calin")),
            (IdTokenError::Expired, "exp", json!(NOW + 300)),
            (IdTokenError::IssuedInFuture, "iat", json!(NOW)),
            (IdTokenError::NotYetValid, "nbf", json!(NOW)),
            (IdTokenError::WrongNonce, "nonce", json!("nonce-1")),
            (IdTokenError::BadSubject, "sub", json!("user-1")),
        ];
        for (error, name, fixed) in steps {
            assert_eq!(check(&all), Err(error), "before fixing {name}");
            all[name] = fixed;
        }
        assert!(check(&all).is_ok());

        // A time that is not one is found when its own check runs.
        let mut late = claims();
        late["iss"] = json!("https://evil.example/");
        late["exp"] = json!(-1);
        assert_eq!(check(&late), Err(IdTokenError::WrongIssuer));
        let mut expired = claims();
        expired["exp"] = json!(NOW - 3600);
        expired["iat"] = json!(-1);
        assert_eq!(check(&expired), Err(IdTokenError::Expired));
        let mut future = claims();
        future["iat"] = json!(NOW + 3600);
        future["nbf"] = json!(-1);
        assert_eq!(check(&future), Err(IdTokenError::IssuedInFuture));
        let mut bad_nbf = claims();
        bad_nbf["nbf"] = json!(-1);
        bad_nbf["nonce"] = json!("nonce-2");
        assert!(matches!(
            check(&bad_nbf),
            Err(IdTokenError::MalformedClaims(why)) if why == "nbf is not a time"
        ));
    }

    #[test]
    fn a_duplicated_claim_is_refused() {
        // Two issuers, the second being the good one: no guessing.
        let body = r#"{"iss":"https://evil.example/","iss":"https://idp.example.com/","sub":"u","aud":"calin","exp":1800000300,"iat":1800000000,"nonce":"nonce-1"}"#;
        assert!(matches!(
            validate_claims(body.as_bytes(), &expected()),
            Err(IdTokenError::MalformedClaims(_))
        ));
    }

    #[test]
    fn email_verified_counts_only_as_the_boolean_true() {
        for (value, verified) in [
            (json!(true), true),
            (json!(false), false),
            (json!("true"), false),
            (json!("True"), false),
            (json!(1), false),
            (Value::Null, false),
            (json!([true]), false),
        ] {
            assert_eq!(
                check(&with("email_verified", value.clone()))
                    .unwrap()
                    .email_verified,
                verified,
                "{value}"
            );
        }
        assert!(!check(&without("email_verified")).unwrap().email_verified);
        // An email that is not a string is no email.
        assert_eq!(
            check(&with("email", json!(["alice@example.com"])))
                .unwrap()
                .email,
            None
        );
        assert_eq!(check(&without("email")).unwrap().email, None);
    }

    fn compact(header: &Value, payload: &Value, signature: &[u8]) -> String {
        format!(
            "{}.{}.{}",
            base64url(header.to_string().as_bytes()),
            base64url(payload.to_string().as_bytes()),
            base64url(signature)
        )
    }

    #[test]
    fn compact_form_is_parsed_strictly() {
        let good = compact(
            &json!({"alg": "ES256", "kid": "k1", "typ": "JWT"}),
            &claims(),
            &[1; 64],
        );
        let token = UnverifiedToken::parse(&good).unwrap();
        assert_eq!(token.algorithm, Algorithm::Es256);
        assert_eq!(token.key_id, "k1");
        assert_eq!(token.signature, vec![1; 64]);
        let (signed, _) = good.rsplit_once('.').unwrap();
        assert_eq!(token.signing_input, signed.as_bytes());

        let parts: Vec<&str> = good.split('.').collect();
        let malformed = [
            String::new(),
            "..".to_owned(),
            "a.b".to_owned(),
            format!("{good}.extra"),
            format!("{}.{}", parts[0], parts[1]),
            format!("{}=.{}.{}", parts[0], parts[1], parts[2]),
            format!("{}.{}.{}==", parts[0], parts[1], parts[2]),
            format!("{}.{}!.{}", parts[0], parts[1], parts[2]),
            format!(" {good}"),
            format!("{good}\n"),
            format!("{}.{}.{}", base64url(b"not json"), parts[1], parts[2]),
            format!("{}.{}.{}", base64url(b"[]"), parts[1], parts[2]),
            format!(
                "{}.{}.{}",
                base64url(br#"{"kid":"k1"}"#),
                parts[1],
                parts[2]
            ),
            format!(
                "{}.{}.{}",
                base64url(br#"{"alg":"ES256","alg":"ES256","kid":"k1"}"#),
                parts[1],
                parts[2]
            ),
            compact(
                &json!({"alg": "ES256", "kid": "k1", "crit": ["exp"]}),
                &claims(),
                &[1; 64],
            ),
            format!("{}.{}.{}", parts[0], "A".repeat(MAX_TOKEN_BYTES), parts[2]),
        ];
        for token in malformed {
            let shown: String = token.chars().take(60).collect();
            assert!(
                matches!(UnverifiedToken::parse(&token), Err(IdTokenError::Malformed)),
                "{shown:?}"
            );
        }
    }

    #[test]
    fn unsupported_algorithms_and_missing_key_ids() {
        for alg in ["none", "None", "HS256", "HS512", "RS512", "PS256", ""] {
            let token = compact(&json!({"alg": alg, "kid": "k1"}), &claims(), &[]);
            assert!(
                matches!(UnverifiedToken::parse(&token), Err(IdTokenError::UnsupportedAlgorithm(name)) if name == alg),
                "{alg:?}"
            );
        }
        for header in [
            json!({"alg": "RS256"}),
            json!({"alg": "RS256", "kid": ""}),
            json!({"alg": "RS256", "kid": "k".repeat(300)}),
        ] {
            let token = compact(&header, &claims(), &[1; 256]);
            assert!(matches!(
                UnverifiedToken::parse(&token),
                Err(IdTokenError::NoKeyId)
            ));
        }
        // An algorithm name from outside cannot inject into a log line.
        let token = compact(&json!({"alg": "x\ny", "kid": "k1"}), &claims(), &[]);
        assert!(
            matches!(UnverifiedToken::parse(&token), Err(IdTokenError::UnsupportedAlgorithm(name)) if name == "x?y")
        );
    }
}
