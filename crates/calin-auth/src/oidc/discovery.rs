//! The provider's discovery document (OpenID Connect Discovery 1.0).

use serde::Deserialize;
use url::Url;

use crate::config::{LoopbackHttp, check_scheme, parse_strict_url};
use crate::json::parse_object;

/// What sign-in needs from the discovery document.
#[derive(Debug, Clone)]
pub(crate) struct ProviderMetadata {
    /// The issuer as the provider spells it: what `iss` must equal.
    pub(crate) issuer: String,
    pub(crate) authorization_endpoint: Url,
    pub(crate) token_endpoint: Url,
    pub(crate) jwks_uri: Url,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MetadataError {
    #[error("the discovery document cannot be read: {0}")]
    Unreadable(String),
    #[error("the discovery document is for another issuer than the one configured")]
    IssuerMismatch,
    #[error("the discovery document's {name} {reason}")]
    Endpoint {
        name: &'static str,
        reason: &'static str,
    },
    #[error("the provider does not support PKCE with S256")]
    NoS256,
}

#[derive(Deserialize)]
struct Document {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    code_challenge_methods_supported: Option<Vec<String>>,
}

/// `<issuer>/.well-known/openid-configuration`, with exactly one `/` between.
pub(crate) fn discovery_url(issuer: &str) -> Result<Url, &'static str> {
    let base = issuer.strip_suffix('/').unwrap_or(issuer);
    parse_strict_url(&format!("{base}/.well-known/openid-configuration"))
}

/// Whether the issuer a document names is the configured one. The two must
/// be the same string, except that one may end with a single `/` the other
/// lacks: Auth0's issuer ends with one and is routinely configured without.
/// Nothing else is normalized, not even letter case.
pub(crate) fn same_issuer(configured: &str, published: &str) -> bool {
    configured == published
        || configured.strip_suffix('/') == Some(published)
        || published.strip_suffix('/') == Some(configured)
}

pub(crate) fn parse_metadata(
    body: &[u8],
    configured_issuer: &str,
    loopback_http: LoopbackHttp,
) -> Result<ProviderMetadata, MetadataError> {
    let document: Document = parse_object(body).map_err(MetadataError::Unreadable)?;
    if !same_issuer(configured_issuer, &document.issuer) {
        return Err(MetadataError::IssuerMismatch);
    }
    if !document.may_support_s256() {
        return Err(MetadataError::NoS256);
    }
    let endpoint = |name, text: &str| endpoint(name, text, loopback_http);
    Ok(ProviderMetadata {
        authorization_endpoint: endpoint(
            "authorization_endpoint",
            &document.authorization_endpoint,
        )?,
        token_endpoint: endpoint("token_endpoint", &document.token_endpoint)?,
        jwks_uri: endpoint("jwks_uri", &document.jwks_uri)?,
        issuer: document.issuer,
    })
}

impl Document {
    /// Present and without S256 means the challenge would be ignored or
    /// refused. Absent means the provider does not say; several real ones
    /// that support S256 do not.
    fn may_support_s256(&self) -> bool {
        self.code_challenge_methods_supported
            .as_ref()
            .is_none_or(|methods| methods.iter().any(|method| method == "S256"))
    }
}

/// One of the provider's endpoints: a strict URL sign-in may contact, with
/// no fragment.
fn endpoint(
    name: &'static str,
    text: &str,
    loopback_http: LoopbackHttp,
) -> Result<Url, MetadataError> {
    let invalid = |reason| MetadataError::Endpoint { name, reason };
    let url = parse_strict_url(text).map_err(invalid)?;
    check_scheme(&url, loopback_http).map_err(invalid)?;
    if url.fragment().is_some() {
        return Err(invalid("must not have a fragment"));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(issuer: &str) -> serde_json::Value {
        serde_json::json!({
            "issuer": issuer,
            "authorization_endpoint": "https://idp.example.com/authorize",
            "token_endpoint": "https://idp.example.com/oauth/token",
            "jwks_uri": "https://keys.example.net/jwks.json",
            "response_types_supported": ["code"],
        })
    }

    fn parse(value: &serde_json::Value, issuer: &str) -> Result<ProviderMetadata, MetadataError> {
        parse_metadata(value.to_string().as_bytes(), issuer, LoopbackHttp::Refuse)
    }

    #[test]
    fn discovery_url_has_one_slash() {
        let cases = [
            (
                "https://idp.example.com",
                "https://idp.example.com/.well-known/openid-configuration",
            ),
            (
                "https://idp.example.com/",
                "https://idp.example.com/.well-known/openid-configuration",
            ),
            (
                "https://idp.example.com/realms/a",
                "https://idp.example.com/realms/a/.well-known/openid-configuration",
            ),
            (
                "https://idp.example.com/realms/a/",
                "https://idp.example.com/realms/a/.well-known/openid-configuration",
            ),
        ];
        for (issuer, expected) in cases {
            assert_eq!(discovery_url(issuer).unwrap().as_str(), expected);
        }
    }

    #[test]
    fn issuer_comparison_table() {
        let cases = [
            ("https://idp.example.com", "https://idp.example.com", true),
            ("https://idp.example.com/", "https://idp.example.com/", true),
            ("https://idp.example.com", "https://idp.example.com/", true),
            ("https://idp.example.com/", "https://idp.example.com", true),
            (
                "https://idp.example.com/a",
                "https://idp.example.com/a/",
                true,
            ),
            (
                "https://idp.example.com",
                "https://idp.example.com//",
                false,
            ),
            (
                "https://idp.example.com//",
                "https://idp.example.com",
                false,
            ),
            ("https://idp.example.com", "https://IDP.example.com", false),
            (
                "https://idp.example.com",
                "https://idp.example.com:443",
                false,
            ),
            ("https://idp.example.com", "http://idp.example.com", false),
            (
                "https://idp.example.com/a",
                "https://idp.example.com/b",
                false,
            ),
            (
                "https://idp.example.com/a",
                "https://idp.example.com",
                false,
            ),
            (
                "https://idp.example.com",
                "https://idp.example.com.evil.example",
                false,
            ),
            ("https://idp.example.com", "", false),
        ];
        for (configured, published, expected) in cases {
            assert_eq!(
                same_issuer(configured, published),
                expected,
                "{configured} {published}"
            );
        }
    }

    #[test]
    fn a_good_document_is_read_and_keeps_the_providers_spelling() {
        let metadata = parse(
            &document("https://idp.example.com/"),
            "https://idp.example.com",
        )
        .unwrap();
        assert_eq!(metadata.issuer, "https://idp.example.com/");
        assert_eq!(
            metadata.authorization_endpoint.as_str(),
            "https://idp.example.com/authorize"
        );
        assert_eq!(
            metadata.token_endpoint.as_str(),
            "https://idp.example.com/oauth/token"
        );
        // Endpoints may live on another host, as Google's do.
        assert_eq!(
            metadata.jwks_uri.as_str(),
            "https://keys.example.net/jwks.json"
        );
    }

    #[test]
    fn another_issuer_is_refused() {
        assert_eq!(
            parse(
                &document("https://evil.example.com"),
                "https://idp.example.com"
            )
            .unwrap_err(),
            MetadataError::IssuerMismatch
        );
    }

    #[test]
    fn endpoints_must_be_https_and_well_formed() {
        let cases = [
            (
                "authorization_endpoint",
                "http://idp.example.com/authorize",
                "must be an https URL",
            ),
            (
                "token_endpoint",
                "http://127.0.0.1:9000/token",
                "must be an https URL",
            ),
            (
                "jwks_uri",
                "ftp://idp.example.com/jwks",
                "must be an https URL",
            ),
            ("jwks_uri", "/jwks", "must be an https URL"),
            (
                "token_endpoint",
                "https://user:pw@idp.example.com/token",
                "must not contain credentials",
            ),
            (
                "authorization_endpoint",
                "https://idp.example.com/authorize#x",
                "must not have a fragment",
            ),
            (
                "token_endpoint",
                "https://idp.example.com\\@evil.example/token",
                "contains whitespace, a control character or a backslash",
            ),
        ];
        for (name, value, reason) in cases {
            let mut doc = document("https://idp.example.com");
            doc[name] = serde_json::json!(value);
            assert_eq!(
                parse(&doc, "https://idp.example.com").unwrap_err(),
                MetadataError::Endpoint { name, reason },
                "{name} = {value}"
            );
        }
    }

    #[test]
    fn the_first_failing_check_names_the_error() {
        let endpoint = |name, reason| MetadataError::Endpoint { name, reason };
        // Bad in every way; each step fixes what the last error named.
        let mut doc = document("https://evil.example.com");
        doc["code_challenge_methods_supported"] = serde_json::json!(["plain"]);
        doc["authorization_endpoint"] = serde_json::json!("http://user:pw@idp.example.com/a#x");
        doc["token_endpoint"] = serde_json::json!("http://idp.example.com/token#x");
        doc["jwks_uri"] = serde_json::json!("https://idp.example.com/jwks#x");
        let https = "must be an https URL";
        let fragment = "must not have a fragment";
        let steps = [
            (
                MetadataError::IssuerMismatch,
                "issuer",
                "https://idp.example.com",
            ),
            (
                MetadataError::NoS256,
                "code_challenge_methods_supported",
                "",
            ),
            (
                endpoint("authorization_endpoint", "must not contain credentials"),
                "authorization_endpoint",
                "http://idp.example.com/a#x",
            ),
            (
                endpoint("authorization_endpoint", https),
                "authorization_endpoint",
                "https://idp.example.com/a#x",
            ),
            (
                endpoint("authorization_endpoint", fragment),
                "authorization_endpoint",
                "https://idp.example.com/a",
            ),
            (
                endpoint("token_endpoint", https),
                "token_endpoint",
                "https://idp.example.com/token",
            ),
            (
                endpoint("jwks_uri", fragment),
                "jwks_uri",
                "https://idp.example.com/jwks",
            ),
        ];
        for (error, name, fixed) in steps {
            assert_eq!(
                parse(&doc, "https://idp.example.com").unwrap_err(),
                error,
                "before fixing {name}"
            );
            if fixed.is_empty() {
                doc.as_object_mut().unwrap().remove(name);
            } else {
                doc[name] = serde_json::json!(fixed);
            }
        }
        assert!(parse(&doc, "https://idp.example.com").is_ok());
    }

    #[test]
    fn loopback_http_endpoints_need_the_flag() {
        let mut doc = document("http://127.0.0.1:9000");
        for name in ["authorization_endpoint", "token_endpoint", "jwks_uri"] {
            doc[name] = serde_json::json!(format!("http://127.0.0.1:9000/{name}"));
        }
        let bytes = doc.to_string();
        assert!(
            parse_metadata(
                bytes.as_bytes(),
                "http://127.0.0.1:9000",
                LoopbackHttp::AllowForDevelopment
            )
            .is_ok()
        );
        assert!(
            parse_metadata(
                bytes.as_bytes(),
                "http://127.0.0.1:9000",
                LoopbackHttp::Refuse
            )
            .is_err()
        );
    }

    #[test]
    fn missing_fields_and_garbage_are_unreadable() {
        for name in [
            "issuer",
            "authorization_endpoint",
            "token_endpoint",
            "jwks_uri",
        ] {
            let mut doc = document("https://idp.example.com");
            doc.as_object_mut().unwrap().remove(name);
            let error = parse(&doc, "https://idp.example.com").unwrap_err();
            assert!(
                matches!(&error, MetadataError::Unreadable(why) if why.contains(name)),
                "{error}"
            );
        }
        for body in ["", "<html>not json</html>", "[]", "null", "{\"issuer\": 5}"] {
            let error = parse_metadata(
                body.as_bytes(),
                "https://idp.example.com",
                LoopbackHttp::Refuse,
            );
            assert!(matches!(error, Err(MetadataError::Unreadable(_))), "{body}");
        }
    }

    #[test]
    fn pkce_support_is_checked_when_the_provider_states_it() {
        let mut doc = document("https://idp.example.com");
        doc["code_challenge_methods_supported"] = serde_json::json!(["plain", "S256"]);
        assert!(parse(&doc, "https://idp.example.com").is_ok());
        doc["code_challenge_methods_supported"] = serde_json::json!(["plain"]);
        assert_eq!(
            parse(&doc, "https://idp.example.com").unwrap_err(),
            MetadataError::NoS256
        );
        doc["code_challenge_methods_supported"] = serde_json::json!([]);
        assert_eq!(
            parse(&doc, "https://idp.example.com").unwrap_err(),
            MetadataError::NoS256
        );
    }
}
