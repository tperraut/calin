//! Randomness, encoding and hashing: the few primitives the other modules
//! build on, and the helpers that keep secrets out of logs.

use base64::Engine as _;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};

/// The operating system could not provide random bytes. Nothing that needs a
/// key, a salt or an id proceeds without them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the operating system's random source failed")]
pub struct RandomError;

/// `N` bytes from the operating system's generator.
pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N], RandomError> {
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| RandomError)?;
    Ok(bytes)
}

/// 128 random bits, URL-safe: a public identifier.
pub(crate) fn random_id() -> Result<String, RandomError> {
    Ok(base64url(&random_bytes::<16>()?))
}

pub(crate) fn base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Strict decoding, for what this crate wrote itself.
pub(crate) fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text).ok()
}

/// Lenient on padding only, for keys a browser sends: `toJSON()` gives them
/// unpadded, but some libraries pad. The alphabet stays URL-safe.
pub(crate) fn base64url_decode_browser(text: &str) -> Option<Vec<u8>> {
    const ENGINE: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_encode_padding(false)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    ENGINE.decode(text).ok()
}

/// Standard base64, for tests that check the alphabet.
#[cfg(test)]
pub(crate) fn base64_standard(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Makes text from outside safe to put in a log line: short, one line,
/// printable ASCII only.
pub(crate) fn log_safe(text: &str, max_chars: usize) -> String {
    let mut out: String = text
        .chars()
        .take(max_chars)
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect();
    if text.chars().nth(max_chars).is_some() {
        out.push_str("...");
    }
    out
}

/// Text from a client, made safe to store and list: no control characters,
/// trimmed, bounded length.
pub(crate) fn clean_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_128_bits_and_differ() {
        let a = random_id().unwrap();
        assert_ne!(a, random_id().unwrap());
        assert_eq!(base64url_decode(&a).unwrap().len(), 16);
    }

    #[test]
    fn browser_keys_may_be_padded_but_stay_url_safe() {
        assert_eq!(base64url_decode_browser("AQ").unwrap(), vec![1]);
        assert_eq!(base64url_decode_browser("AQ==").unwrap(), vec![1]);
        assert!(base64url_decode_browser("A+").is_none());
        assert!(base64url_decode_browser("A/").is_none());
        assert!(base64url_decode("AQ==").is_none());
        assert_eq!(base64_standard(&[0xfb]), "+w==");
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn log_safe_strips_control_characters_and_truncates() {
        assert_eq!(log_safe("a\nb\u{1b}[0m\u{e9}", 100), "a?b?[0m?");
        assert_eq!(log_safe(&"x".repeat(500), 120).len(), 123);
    }

    #[test]
    fn clean_text_drops_control_characters() {
        assert_eq!(clean_text("  a\u{0}b\nc  ", 10), "abc");
        assert_eq!(clean_text("abcdef", 3), "abc");
    }
}
