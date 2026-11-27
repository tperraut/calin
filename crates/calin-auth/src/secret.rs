//! Randomness, hashing and constant-time comparison: the few primitives every
//! other module builds its secrets from.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use subtle::ConstantTimeEq;

/// The operating system could not provide random bytes. Nothing that needs a
/// secret proceeds without them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the operating system's random source failed")]
pub struct RandomError;

/// Length of a 256-bit value in unpadded URL-safe base64.
pub(crate) const TOKEN_CHARS: usize = 43;

/// `N` bytes from the operating system's generator (`getrandom(2)` on Linux).
pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N], RandomError> {
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| RandomError)?;
    Ok(bytes)
}

/// 256 random bits, URL-safe: the form of every secret this crate issues.
pub(crate) fn random_token() -> Result<String, RandomError> {
    Ok(base64url(&random_bytes::<32>()?))
}

/// 128 random bits, URL-safe: a public identifier that says nothing about
/// the secret it stands next to.
pub(crate) fn random_id() -> Result<String, RandomError> {
    Ok(base64url(&random_bytes::<16>()?))
}

pub(crate) fn base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Strict decoding: padding and non-canonical trailing bits are refused.
pub(crate) fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text).ok()
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Equality whose duration does not depend on where the inputs differ.
/// Lengths are not secret here, so unequal lengths may return early.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// Whether `value` has the exact form [`random_token`] produces. Anything
/// else is refused before it is hashed, which also bounds the work per request.
pub(crate) fn has_token_shape(value: &str) -> bool {
    value.len() == TOKEN_CHARS
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Makes text from outside safe to put in a log line: short, one line,
/// printable ASCII only.
pub(crate) fn log_safe(text: &str) -> String {
    const MAX: usize = 120;
    let mut out: String = text
        .chars()
        .take(MAX)
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect();
    if text.chars().nth(MAX).is_some() {
        out.push_str("...");
    }
    out
}

/// A `Debug` that names the type and nothing else, for types holding a secret.
macro_rules! redacted_debug {
    ($type:ident) => {
        impl std::fmt::Debug for $type {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($type), "(REDACTED)"))
            }
        }
    };
}
pub(crate) use redacted_debug;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_256_bits_and_differ() {
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_ne!(a, b);
        assert!(has_token_shape(&a));
        assert_eq!(base64url_decode(&a).unwrap().len(), 32);
        assert_eq!(base64url_decode(&random_id().unwrap()).unwrap().len(), 16);
    }

    #[test]
    fn token_shape_is_exact() {
        let token = random_token().unwrap();
        assert!(!has_token_shape(&token[1..]));
        assert!(!has_token_shape(&format!("{token}A")));
        assert!(!has_token_shape(&format!("{}=", &token[1..])));
        assert!(!has_token_shape(&format!("{}+", &token[1..])));
        assert!(!has_token_shape(""));
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        let hex: String = sha256(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn constant_time_eq_compares_content_and_length() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"sama"));
        assert!(!constant_time_eq(b"same", b"same!"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn strict_base64url_refuses_padding_and_trailing_bits() {
        assert_eq!(base64url_decode("AQ").unwrap(), vec![1]);
        assert!(base64url_decode("AQ==").is_none());
        assert!(base64url_decode("AR").is_none());
        assert!(base64url_decode("A+").is_none());
    }

    #[test]
    fn log_safe_strips_control_characters_and_truncates() {
        assert_eq!(log_safe("a\nb\u{1b}[0m\u{e9}"), "a?b?[0m?");
        let long = "x".repeat(500);
        assert_eq!(log_safe(&long).len(), 123);
    }
}
