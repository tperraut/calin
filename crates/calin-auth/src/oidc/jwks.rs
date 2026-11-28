//! The provider's published keys (RFC 7517), reduced to what may verify an
//! ID token here.

use ring::signature;
use serde::Deserialize;

use crate::json::parse_object;
use crate::secret::base64url_decode;

/// Keys read from one key set. A provider publishes a handful.
const MAX_KEYS: usize = 64;
const MAX_KEY_ID_BYTES: usize = 256;

/// The signature algorithms an ID token may use. Asymmetric only: `none`
/// and the HMAC family, whose "key" would be the client secret, have no
/// variant and so no code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Algorithm {
    /// RSASSA-PKCS1-v1_5 with SHA-256.
    Rs256,
    /// ECDSA on P-256 with SHA-256.
    Es256,
}

impl Algorithm {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "RS256" => Some(Self::Rs256),
            "ES256" => Some(Self::Es256),
            _ => None,
        }
    }
}

/// A published key together with the one algorithm it may be used with.
#[derive(Debug, Clone)]
pub(crate) struct VerificationKey {
    pub(crate) key_id: String,
    material: Material,
}

#[derive(Debug, Clone)]
enum Material {
    Rsa {
        modulus: Vec<u8>,
        exponent: Vec<u8>,
    },
    /// The uncompressed point: 0x04, x, y.
    P256 {
        point: [u8; 65],
    },
}

impl VerificationKey {
    /// The algorithm follows from the key's type, never from the token.
    pub(crate) fn algorithm(&self) -> Algorithm {
        match self.material {
            Material::Rsa { .. } => Algorithm::Rs256,
            Material::P256 { .. } => Algorithm::Es256,
        }
    }

    pub(crate) fn verifies(&self, message: &[u8], signature: &[u8]) -> bool {
        match &self.material {
            // ring refuses moduli under 2048 or over 8192 bits.
            Material::Rsa { modulus, exponent } => signature::RsaPublicKeyComponents {
                n: modulus,
                e: exponent,
            }
            .verify(&signature::RSA_PKCS1_2048_8192_SHA256, message, signature)
            .is_ok(),
            // The fixed-length (r, s) form JWS uses, not ASN.1.
            Material::P256 { point } => {
                signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                    .verify(message, signature)
                    .is_ok()
            }
        }
    }
}

#[derive(Deserialize)]
struct KeySet {
    keys: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    key_ops: Option<Vec<String>>,
    n: Option<String>,
    e: Option<String>,
    crv: Option<String>,
    x: Option<String>,
    y: Option<String>,
}

/// Reads a key set, keeping only keys that are usable here: a key id, a
/// supported type, and nothing (`alg`, `use`, `key_ops`) saying the key is
/// meant for something other than verifying these signatures. A key set
/// that is not one at all is an error; unusable keys in it are skipped,
/// since providers publish encryption keys alongside.
pub(crate) fn parse_key_set(body: &[u8]) -> Result<Vec<VerificationKey>, String> {
    let set: KeySet =
        parse_object(body).map_err(|reason| format!("the key set cannot be read: {reason}"))?;
    Ok(set
        .keys
        .into_iter()
        .take(MAX_KEYS)
        // An object, not an array read by position: see `parse_object`.
        .filter(serde_json::Value::is_object)
        .filter_map(|value| serde_json::from_value::<Jwk>(value).ok())
        .filter_map(usable_key)
        .collect())
}

fn usable_key(mut jwk: Jwk) -> Option<VerificationKey> {
    let key_id = usable_key_id(jwk.kid.take())?;
    if !jwk.is_for_verifying() {
        return None;
    }
    let (material, algorithm) = jwk.material()?;
    // An RSA key declared for PS256 or RSA-OAEP is not an RS256 key.
    if jwk.alg.is_some_and(|alg| alg != algorithm) {
        return None;
    }
    Some(VerificationKey { key_id, material })
}

/// A key id this crate will look up, in a key set or a token header: not
/// empty, and not long enough to matter in memory or in a log line.
pub(crate) fn usable_key_id(key_id: Option<String>) -> Option<String> {
    key_id.filter(|id| !id.is_empty() && id.len() <= MAX_KEY_ID_BYTES)
}

impl Jwk {
    /// Nothing (`use`, `key_ops`) says the key is meant for something other
    /// than verifying signatures.
    fn is_for_verifying(&self) -> bool {
        let usage_ok = self.usage.as_ref().is_none_or(|usage| usage == "sig");
        let ops_ok = self
            .key_ops
            .as_ref()
            .is_none_or(|ops| ops.iter().any(|op| op == "verify"));
        usage_ok && ops_ok
    }

    /// The key material of a supported type, with the one algorithm it may
    /// verify.
    fn material(&self) -> Option<(Material, &'static str)> {
        match self.kty.as_str() {
            "RSA" => Some((self.rsa_material()?, "RS256")),
            "EC" if self.crv.as_deref() == Some("P-256") => Some((self.p256_material()?, "ES256")),
            _ => None,
        }
    }

    fn rsa_material(&self) -> Option<Material> {
        let modulus = unsigned(self.n.as_deref()?)?;
        let exponent = unsigned(self.e.as_deref()?)?;
        // Checked here so an undersized key is ignored at load, not
        // discovered at the first sign-in.
        if !(256..=1024).contains(&modulus.len()) {
            return None;
        }
        Some(Material::Rsa { modulus, exponent })
    }

    fn p256_material(&self) -> Option<Material> {
        let x = base64url_decode(self.x.as_deref()?)?;
        let y = base64url_decode(self.y.as_deref()?)?;
        if x.len() != 32 || y.len() != 32 {
            return None;
        }
        let mut point = [0u8; 65];
        point[0] = 4;
        point[1..33].copy_from_slice(&x);
        point[33..].copy_from_slice(&y);
        Some(Material::P256 { point })
    }
}

/// A big-endian unsigned integer, without the leading zero bytes some
/// providers emit and ring refuses.
fn unsigned(text: &str) -> Option<Vec<u8>> {
    let bytes = base64url_decode(text)?;
    let first = bytes.iter().position(|byte| *byte != 0)?;
    Some(bytes[first..].to_vec())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::secret::base64url;

    fn rsa(bits: usize) -> serde_json::Value {
        let mut modulus = vec![0xc3u8; bits / 8];
        modulus[0] |= 0x80;
        json!({"kty": "RSA", "kid": "r1", "n": base64url(&modulus), "e": "AQAB"})
    }

    fn ec() -> serde_json::Value {
        json!({"kty": "EC", "crv": "P-256", "kid": "e1", "x": base64url(&[7u8; 32]), "y": base64url(&[9u8; 32])})
    }

    fn with(mut key: serde_json::Value, name: &str, value: serde_json::Value) -> serde_json::Value {
        key[name] = value;
        key
    }

    fn without(mut key: serde_json::Value, name: &str) -> serde_json::Value {
        key.as_object_mut().unwrap().remove(name);
        key
    }

    fn usable(key: serde_json::Value) -> Vec<VerificationKey> {
        parse_key_set(json!({"keys": [key]}).to_string().as_bytes()).unwrap()
    }

    #[test]
    fn algorithm_names() {
        assert_eq!(Algorithm::from_name("RS256"), Some(Algorithm::Rs256));
        assert_eq!(Algorithm::from_name("ES256"), Some(Algorithm::Es256));
        for name in [
            "none", "None", "NONE", "", "HS256", "HS384", "HS512", "rs256", "RS384", "RS512",
            "PS256", "ES384", "ES256K", "EdDSA", "RS256 ",
        ] {
            assert_eq!(Algorithm::from_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn usable_keys_get_the_algorithm_of_their_type() {
        let keys = usable(rsa(2048));
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key_id, "r1");
        assert_eq!(keys[0].algorithm(), Algorithm::Rs256);
        let keys = usable(ec());
        assert_eq!(keys[0].algorithm(), Algorithm::Es256);

        for key in [
            with(rsa(2048), "alg", json!("RS256")),
            with(rsa(2048), "use", json!("sig")),
            with(rsa(4096), "key_ops", json!(["verify"])),
            with(ec(), "alg", json!("ES256")),
            with(ec(), "use", json!("sig")),
            with(ec(), "x5c", json!(["ignored"])),
        ] {
            assert_eq!(usable(key.clone()).len(), 1, "{key}");
        }
    }

    #[test]
    fn unusable_keys_are_ignored() {
        let cases = vec![
            ("no kid", without(rsa(2048), "kid")),
            ("empty kid", with(rsa(2048), "kid", json!(""))),
            ("huge kid", with(rsa(2048), "kid", json!("k".repeat(300)))),
            ("kid not a string", with(rsa(2048), "kid", json!(7))),
            ("encryption use", with(rsa(2048), "use", json!("enc"))),
            (
                "alg for another scheme",
                with(rsa(2048), "alg", json!("PS256")),
            ),
            ("encryption alg", with(rsa(2048), "alg", json!("RSA-OAEP"))),
            (
                "symmetric alg on rsa",
                with(rsa(2048), "alg", json!("HS256")),
            ),
            ("alg none", with(rsa(2048), "alg", json!("none"))),
            (
                "key_ops without verify",
                with(rsa(2048), "key_ops", json!(["sign", "encrypt"])),
            ),
            ("1024-bit rsa", rsa(1024)),
            ("16384-bit rsa", rsa(16384)),
            ("no modulus", without(rsa(2048), "n")),
            ("no exponent", without(rsa(2048), "e")),
            ("zero exponent", with(rsa(2048), "e", json!("AA"))),
            ("modulus not base64url", with(rsa(2048), "n", json!("+++"))),
            (
                "symmetric key",
                json!({"kty": "oct", "kid": "s1", "k": "c2VjcmV0"}),
            ),
            (
                "okp key",
                json!({"kty": "OKP", "crv": "Ed25519", "kid": "o1", "x": base64url(&[1u8; 32])}),
            ),
            ("other curve", with(ec(), "crv", json!("P-384"))),
            ("no curve", without(ec(), "crv")),
            ("short x", with(ec(), "x", json!(base64url(&[7u8; 31])))),
            ("long y", with(ec(), "y", json!(base64url(&[7u8; 33])))),
            ("no y", without(ec(), "y")),
            ("ec with rsa alg", with(ec(), "alg", json!("RS256"))),
            ("ec for key agreement", with(ec(), "alg", json!("ECDH-ES"))),
            ("unknown kty", with(rsa(2048), "kty", json!("rsa"))),
            ("not an object", json!("key")),
            ("no kty", without(ec(), "kty")),
        ];
        for (what, key) in cases {
            assert!(usable(key).is_empty(), "{what}");
        }
    }

    #[test]
    fn good_keys_survive_next_to_bad_ones() {
        let set = json!({"keys": [json!(null), rsa(1024), ec(), with(rsa(2048), "use", json!("enc")), rsa(2048)]});
        let keys = parse_key_set(set.to_string().as_bytes()).unwrap();
        let ids: Vec<&str> = keys.iter().map(|k| k.key_id.as_str()).collect();
        assert_eq!(ids, ["e1", "r1"]);
    }

    #[test]
    fn leading_zeros_in_rsa_numbers_are_tolerated() {
        let mut modulus = vec![0u8; 3];
        modulus.extend(vec![0xc3u8; 256]);
        let key = json!({"kty": "RSA", "kid": "z", "n": base64url(&modulus), "e": base64url(&[0, 1, 0, 1])});
        match &usable(key)[0].material {
            Material::Rsa { modulus, exponent } => {
                assert_eq!(modulus.len(), 256);
                assert_eq!(exponent, &[1, 0, 1]);
            }
            Material::P256 { .. } => panic!("expected an RSA key"),
        }
    }

    #[test]
    fn what_is_not_a_key_set_is_an_error() {
        for body in [
            "",
            "not json",
            "[]",
            "{}",
            "{\"keys\": {}}",
            "{\"keys\": \"x\"}",
        ] {
            assert!(parse_key_set(body.as_bytes()).is_err(), "{body}");
        }
        assert!(parse_key_set(b"{\"keys\": []}").unwrap().is_empty());
    }

    #[test]
    fn the_number_of_keys_is_bounded() {
        let keys: Vec<serde_json::Value> = (0..200)
            .map(|i| with(ec(), "kid", json!(format!("k{i}"))))
            .collect();
        let parsed = parse_key_set(json!({"keys": keys}).to_string().as_bytes()).unwrap();
        assert_eq!(parsed.len(), MAX_KEYS);
    }
}
