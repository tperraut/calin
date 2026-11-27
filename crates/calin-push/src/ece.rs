//! Message encryption for Web Push: RFC 8291 key derivation over the
//! `aes128gcm` content encoding of RFC 8188, one record.
//!
//! The ephemeral key and the salt are inputs to [`encrypt_with`], so the
//! RFC's worked example can be reproduced; [`encrypt`] draws both fresh.

use ring::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::agreement::{self, ECDH_P256, EphemeralPrivateKey, UnparsedPublicKey};
use ring::hkdf;
use ring::rand::SystemRandom;

use crate::secret::random_bytes;

/// An uncompressed P-256 point: `0x04 || X || Y`.
pub(crate) const PUBLIC_KEY_LEN: usize = 65;
/// The subscription's authentication secret (RFC 8291 section 3.2).
pub(crate) const AUTH_LEN: usize = 16;
const SALT_LEN: usize = 16;
const TAG_LEN: usize = 16;
/// The record size written in the header. One record holds the whole
/// message, and no message is larger than this.
const RECORD_SIZE: u32 = 4096;
/// salt (16) || rs (4) || idlen (1) || keyid (65): RFC 8188 section 2.1.
pub(crate) const HEADER_LEN: usize = SALT_LEN + 4 + 1 + PUBLIC_KEY_LEN;
/// Push services must accept a body of 4,096 octets (RFC 8030 section 7.2),
/// and need not accept more.
pub(crate) const MAX_BODY_LEN: usize = 4096;
/// The largest plaintext that fits: the body less the header, the padding
/// delimiter and the authentication tag.
pub(crate) const MAX_PLAINTEXT_LEN: usize = MAX_BODY_LEN - HEADER_LEN - 1 - TAG_LEN;

/// Why a message could not be encrypted. Carries no key material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EncryptError {
    /// The encrypted message would exceed the 4,096 octets every push
    /// service accepts.
    #[error("the payload is {len} bytes; at most {MAX_PLAINTEXT_LEN} fit in one push message")]
    PayloadTooLarge {
        /// The plaintext's length.
        len: usize,
    },
    /// The subscription's public key is not a point on P-256.
    #[error("the subscription's public key is not a valid P-256 point")]
    InvalidKey,
    /// The operating system's random source failed.
    #[error("the operating system's random source failed")]
    Random,
}

/// The keys a browser gives when it subscribes.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SubscriberKeys {
    pub(crate) p256dh: [u8; PUBLIC_KEY_LEN],
    pub(crate) auth: [u8; AUTH_LEN],
}

impl std::fmt::Debug for SubscriberKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SubscriberKeys(REDACTED)")
    }
}

/// The sender's side of the key agreement. Isolated so the test vector can
/// inject a fixed key: `ring` only ever generates ephemeral keys itself.
pub(crate) trait EphemeralKey {
    /// The uncompressed public point, which becomes the header's key id.
    fn public_key(&self) -> [u8; PUBLIC_KEY_LEN];
    /// The ECDH shared secret (the X coordinate) with `peer`.
    fn agree(self, peer: &[u8; PUBLIC_KEY_LEN]) -> Result<[u8; 32], EncryptError>;
}

/// A fresh key from `ring`, used once.
pub(crate) struct RingEphemeral {
    private: EphemeralPrivateKey,
    public: [u8; PUBLIC_KEY_LEN],
}

impl RingEphemeral {
    pub(crate) fn generate() -> Result<Self, EncryptError> {
        let private = EphemeralPrivateKey::generate(&ECDH_P256, &SystemRandom::new())
            .map_err(|_| EncryptError::Random)?;
        let public = private
            .compute_public_key()
            .map_err(|_| EncryptError::Random)?;
        let public = public
            .as_ref()
            .try_into()
            .map_err(|_| EncryptError::Random)?;
        Ok(Self { private, public })
    }
}

impl EphemeralKey for RingEphemeral {
    fn public_key(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.public
    }

    fn agree(self, peer: &[u8; PUBLIC_KEY_LEN]) -> Result<[u8; 32], EncryptError> {
        let peer = UnparsedPublicKey::new(&ECDH_P256, peer);
        // ring validates the peer point here and fails if it is off the curve.
        agreement::agree_ephemeral(self.private, &peer, |secret| {
            <[u8; 32]>::try_from(secret).map_err(|_| EncryptError::InvalidKey)
        })
        .map_err(|_| EncryptError::InvalidKey)?
    }
}

/// Whether `bytes` is an uncompressed P-256 point on the curve, checked by
/// agreeing on a throwaway key with it.
pub(crate) fn is_valid_public_key(bytes: &[u8]) -> bool {
    let Ok(point) = <[u8; PUBLIC_KEY_LEN]>::try_from(bytes) else {
        return false;
    };
    if point[0] != 0x04 {
        return false;
    }
    RingEphemeral::generate().is_ok_and(|key| key.agree(&point).is_ok())
}

/// Encrypts `plaintext` for one subscription with a fresh key and salt.
pub(crate) fn encrypt(plaintext: &[u8], keys: &SubscriberKeys) -> Result<Vec<u8>, EncryptError> {
    let salt = random_bytes::<SALT_LEN>().map_err(|_| EncryptError::Random)?;
    encrypt_with(plaintext, keys, RingEphemeral::generate()?, salt)
}

/// The whole `aes128gcm` body: header, then one record.
pub(crate) fn encrypt_with(
    plaintext: &[u8],
    keys: &SubscriberKeys,
    ephemeral: impl EphemeralKey,
    salt: [u8; SALT_LEN],
) -> Result<Vec<u8>, EncryptError> {
    if plaintext.len() > MAX_PLAINTEXT_LEN {
        return Err(EncryptError::PayloadTooLarge {
            len: plaintext.len(),
        });
    }
    let as_public = ephemeral.public_key();
    let ecdh_secret = ephemeral.agree(&keys.p256dh)?;

    // RFC 8291 section 3.4: IKM = HKDF(auth_secret, ecdh_secret, key_info, 32).
    let mut key_info = Vec::with_capacity(14 + 2 * PUBLIC_KEY_LEN);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(&keys.p256dh);
    key_info.extend_from_slice(&as_public);
    let mut ikm = [0u8; 32];
    hkdf_sha256(&keys.auth, &ecdh_secret, &key_info, &mut ikm);

    // RFC 8188 section 2.2 and 2.3.
    let mut cek = [0u8; 16];
    hkdf_sha256(&salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek);
    let mut nonce = [0u8; 12];
    hkdf_sha256(&salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce);

    let mut body = Vec::with_capacity(HEADER_LEN + plaintext.len() + 1 + TAG_LEN);
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    // The key id length is 65, which fits the one-octet field.
    body.push(PUBLIC_KEY_LEN as u8);
    body.extend_from_slice(&as_public);

    // The last (and only) record ends with the 0x02 delimiter and no padding.
    let mut record = Vec::with_capacity(plaintext.len() + 1 + TAG_LEN);
    record.extend_from_slice(plaintext);
    record.push(0x02);
    // A key fresh for every message and record sequence number 0: the
    // nonce is used once, which is what `assume_unique_for_key` requires.
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_128_GCM, &cek).map_err(|_| EncryptError::InvalidKey)?,
    );
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::empty(),
        &mut record,
    )
    .map_err(|_| EncryptError::PayloadTooLarge {
        len: plaintext.len(),
    })?;
    body.extend_from_slice(&record);
    Ok(body)
}

/// HKDF-SHA-256 into `out`, whose length is at most 32 here.
fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) {
    struct Len(usize);
    impl hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let info = [info];
    // Expansion fails only past 255 * 32 octets; every output here is at
    // most 32, so neither call can fail.
    let okm = prk
        .expand(&info, Len(out.len()))
        .expect("HKDF output of at most 32 octets");
    okm.fill(out).expect("HKDF output of at most 32 octets");
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::secret::{base64url, base64url_decode};

    /// A fixed sender key computed with the pure-Rust `p256` crate: the
    /// test vector's key cannot be loaded into `ring`.
    pub(crate) struct FixedEphemeral(pub(crate) p256::SecretKey);

    impl EphemeralKey for FixedEphemeral {
        fn public_key(&self) -> [u8; PUBLIC_KEY_LEN] {
            uncompressed(&self.0.public_key())
        }

        fn agree(self, peer: &[u8; PUBLIC_KEY_LEN]) -> Result<[u8; 32], EncryptError> {
            let peer =
                p256::PublicKey::from_sec1_bytes(peer).map_err(|_| EncryptError::InvalidKey)?;
            let shared = self.0.diffie_hellman(&peer);
            Ok(<[u8; 32]>::try_from(shared.raw_secret_bytes().as_slice()).unwrap())
        }
    }

    pub(crate) fn uncompressed(key: &p256::PublicKey) -> [u8; PUBLIC_KEY_LEN] {
        let bytes = key.to_sec1_bytes();
        <[u8; PUBLIC_KEY_LEN]>::try_from(&bytes[..]).expect("P-256 points are uncompressed")
    }

    fn b64(text: &str) -> Vec<u8> {
        let text: String = text.split_whitespace().collect();
        base64url_decode(&text).unwrap()
    }

    /// A subscriber's side, written from RFC 8291 section 3.4 and RFC 8188
    /// with HMAC directly rather than this module's HKDF.
    pub(crate) fn decrypt(body: &[u8], ua_private: &p256::SecretKey, auth: &[u8]) -> Vec<u8> {
        use ring::hmac;
        let hmac_sha256 = |key: &[u8], data: &[&[u8]]| -> Vec<u8> {
            let key = hmac::Key::new(hmac::HMAC_SHA256, key);
            let mut context = hmac::Context::with_key(&key);
            for part in data {
                context.update(part);
            }
            context.sign().as_ref().to_vec()
        };
        let salt = &body[..16];
        let rs = u32::from_be_bytes(body[16..20].try_into().unwrap());
        let idlen = usize::from(body[20]);
        assert_eq!(idlen, 65);
        let as_public = &body[21..21 + idlen];
        let record = &body[21 + idlen..];
        assert!(record.len() <= rs as usize, "one record");

        let ua_public = uncompressed(&ua_private.public_key());
        let shared =
            ua_private.diffie_hellman(&p256::PublicKey::from_sec1_bytes(as_public).unwrap());
        let prk_key = hmac_sha256(auth, &[shared.raw_secret_bytes().as_slice()]);
        let ikm = hmac_sha256(&prk_key, &[b"WebPush: info\0", &ua_public, as_public, &[1]]);
        let prk = hmac_sha256(salt, &[&ikm]);
        let cek = &hmac_sha256(&prk, &[b"Content-Encoding: aes128gcm\0", &[1]])[..16];
        let nonce = &hmac_sha256(&prk, &[b"Content-Encoding: nonce\0", &[1]])[..12];

        let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, cek).unwrap());
        let mut buffer = record.to_vec();
        let plain = key
            .open_in_place(
                Nonce::try_assume_unique_for_key(nonce).unwrap(),
                Aad::empty(),
                &mut buffer,
            )
            .unwrap()
            .to_vec();
        // The last record: strip trailing zero padding, then the 0x02 delimiter.
        let end = plain.iter().rposition(|&b| b != 0).unwrap();
        assert_eq!(plain[end], 0x02, "last-record delimiter");
        plain[..end].to_vec()
    }

    /// RFC 8291 section 5 and Appendix A, byte for byte.
    #[test]
    fn rfc_8291_appendix_a() {
        let plaintext = b64("V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24");
        assert_eq!(plaintext, b"When I grow up, I want to be a watermelon");
        let as_private = b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw");
        let as_public = b64("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIg
            Dll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let ua_public = b64("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcx
            aOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let ua_private = b64("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94");
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let auth = b64("BTBZMqHH6r4Tts7J_aSIgg");

        let ephemeral = FixedEphemeral(p256::SecretKey::from_slice(&as_private).unwrap());
        assert_eq!(ephemeral.public_key().to_vec(), as_public);
        let keys = SubscriberKeys {
            p256dh: ua_public.clone().try_into().unwrap(),
            auth: auth.clone().try_into().unwrap(),
        };

        // The intermediate values, through this module's own HKDF.
        let ecdh_secret = FixedEphemeral(p256::SecretKey::from_slice(&as_private).unwrap())
            .agree(&keys.p256dh)
            .unwrap();
        assert_eq!(
            base64url(&ecdh_secret),
            "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"
        );
        let mut key_info = b"WebPush: info\0".to_vec();
        key_info.extend_from_slice(&ua_public);
        key_info.extend_from_slice(&as_public);
        assert_eq!(
            key_info,
            b64("V2ViUHVzaDogaW5mbwAEJXGyvs3942BVG
                q8e0PTNNmwR zr5VX4m8t7GGpTM5FzFo7OLr4BhZe9MEebhuPI-OztV3
                ylkYfpJGmQ22ggCLDgT-M_SrDepxkU21WCP3O1SUj0Ew
                bZIHMtu5pZpTKGSCIA5Zent7wmC6HCJ5mFgJkuk5cwAv MBKiiujwa7t45ewP")
        );
        let mut ikm = [0u8; 32];
        hkdf_sha256(&auth, &ecdh_secret, &key_info, &mut ikm);
        assert_eq!(
            base64url(&ikm),
            "S4lYMb_L0FxCeq0WhDx813KgSYqU26kOyzWUdsXYyrg"
        );
        assert_eq!(
            b"Content-Encoding: aes128gcm\0".to_vec(),
            b64("Q29udGVudC1FbmNvZGluZzogYWVzMTI4Z2NtAA")
        );
        assert_eq!(
            b"Content-Encoding: nonce\0".to_vec(),
            b64("Q29udGVudC1FbmNvZGluZzogbm9uY2UA")
        );
        let mut cek = [0u8; 16];
        hkdf_sha256(&salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek);
        assert_eq!(base64url(&cek), "oIhVW04MRdy2XN9CiKLxTg");
        let mut nonce = [0u8; 12];
        hkdf_sha256(&salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce);
        assert_eq!(base64url(&nonce), "4h_95klXJ5E_qnoN");

        // The whole message.
        let body = encrypt_with(&plaintext, &keys, ephemeral, salt).unwrap();
        assert_eq!(
            body[..HEADER_LEN].to_vec(),
            b64(
                "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z 9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
                mlMoZIIgDll6e3vCYLocInmYWAmS6Tlz AC8wEqKK6PBru3jl7A8"
            )
        );
        assert_eq!(
            body[HEADER_LEN..].to_vec(),
            b64(
                "8pfeW0KbunFT06SuDKoJH9Ql87S1QUrd irN6GcG7sFz1y1sqLgVi1VhjVkHsUoEs
                bI_0LpXMuGvnzQ"
            )
        );
        assert_eq!(
            body,
            b64(
                "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
                mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT
                pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
            )
        );
        // 86 + 41 + 1 + 16. The example's request says `Content-Length: 145`,
        // but its own body above decodes to 144 octets.
        assert_eq!(body.len(), 144);

        // And the independent decryption reads it back with the receiver's key.
        let ua_private = p256::SecretKey::from_slice(&ua_private).unwrap();
        assert_eq!(decrypt(&body, &ua_private, &auth), plaintext);
    }

    pub(crate) fn subscriber() -> (p256::SecretKey, SubscriberKeys) {
        loop {
            let bytes = random_bytes::<32>().unwrap();
            if let Ok(secret) = p256::SecretKey::from_slice(&bytes) {
                let keys = SubscriberKeys {
                    p256dh: uncompressed(&secret.public_key()),
                    auth: random_bytes::<16>().unwrap(),
                };
                return (secret, keys);
            }
        }
    }

    #[test]
    fn fresh_encryptions_round_trip_and_differ() {
        let (secret, keys) = subscriber();
        let a = encrypt(b"{\"hello\":1}", &keys).unwrap();
        let b = encrypt(b"{\"hello\":1}", &keys).unwrap();
        assert_ne!(a, b, "fresh salt and key each time");
        assert_eq!(decrypt(&a, &secret, &keys.auth), b"{\"hello\":1}");
        assert_eq!(decrypt(&b, &secret, &keys.auth), b"{\"hello\":1}");
    }

    #[test]
    fn the_largest_plaintext_fills_exactly_4096_octets() {
        let (secret, keys) = subscriber();
        let plaintext = vec![b'a'; MAX_PLAINTEXT_LEN];
        let body = encrypt(&plaintext, &keys).unwrap();
        assert_eq!(body.len(), MAX_BODY_LEN);
        assert_eq!(decrypt(&body, &secret, &keys.auth), plaintext);
        assert_eq!(
            encrypt(&vec![b'a'; MAX_PLAINTEXT_LEN + 1], &keys),
            Err(EncryptError::PayloadTooLarge {
                len: MAX_PLAINTEXT_LEN + 1
            })
        );
    }

    #[test]
    fn public_keys_must_be_uncompressed_points_on_the_curve() {
        let (_, keys) = subscriber();
        assert!(is_valid_public_key(&keys.p256dh));
        let mut off_curve = keys.p256dh;
        off_curve[64] ^= 1;
        assert!(!is_valid_public_key(&off_curve));
        let mut compressed_tag = keys.p256dh;
        compressed_tag[0] = 0x02;
        assert!(!is_valid_public_key(&compressed_tag));
        assert!(!is_valid_public_key(&keys.p256dh[..64]));
        assert!(!is_valid_public_key(&[0u8; 65]));
        assert!(!is_valid_public_key(&[]));
        assert_eq!(
            encrypt(
                b"x",
                &SubscriberKeys {
                    p256dh: off_curve,
                    auth: keys.auth
                }
            ),
            Err(EncryptError::InvalidKey)
        );
        assert_eq!(format!("{keys:?}"), "SubscriberKeys(REDACTED)");
    }
}
