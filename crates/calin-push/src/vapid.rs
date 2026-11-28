//! The server's VAPID key pair (RFC 8292): created once, kept in a file,
//! and used to sign the token every push request carries.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use calin_proto::PushKey;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _};
use serde::{Deserialize, Serialize};

use crate::ece::PUBLIC_KEY_LEN;
use crate::fsio::{create_atomically, tighten_permissions};
use crate::secret::{base64url, base64url_decode};

const FILE_VERSION: u32 = 1;
/// How long a signed token is valid. RFC 8292 allows at most 24 hours;
/// half of that leaves room for a push service whose clock runs ahead.
pub(crate) const TOKEN_LIFETIME_SECS: u64 = 12 * 60 * 60;

/// The server's VAPID key pair. Every browser subscription is bound to its
/// public key, so it is never replaced once created.
pub struct VapidKeys {
    key_pair: EcdsaKeyPair,
    public: [u8; PUBLIC_KEY_LEN],
    generated: bool,
}

/// Loading or using the key pair failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VapidError {
    /// The file could not be read or written.
    #[error("cannot access the VAPID key file {}: {source}", path.display())]
    Io {
        /// The key file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The file exists but does not hold a key pair. It is left alone:
    /// a new key would silently orphan every subscription.
    #[error(
        "the VAPID key file {} is corrupt ({reason}); restore it from a backup, or delete it \
         to create a new key, after which every device must subscribe again",
        path.display()
    )]
    Corrupt {
        /// The key file.
        path: PathBuf,
        /// Why, safe to log.
        reason: String,
    },
    /// No randomness, no key and no signature.
    #[error("the operating system's random source failed")]
    Random,
    /// The blocking task reading the file did not complete (runtime shutdown).
    #[error("the task reading the VAPID key file did not complete")]
    Task,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFile {
    version: u32,
    /// The PKCS#8 document `ring` generated, base64url.
    pkcs8: String,
    /// Derived from `pkcs8`; written for whoever reads the file, and checked
    /// on load so a hand-edited file is caught.
    public_key: String,
}

impl VapidKeys {
    /// Loads the key pair from `path`, or creates it there (mode 0600) if no
    /// file exists. The directory must exist.
    ///
    /// A file that exists but cannot be understood is an error naming it;
    /// it is never replaced.
    pub async fn load_or_generate(path: impl Into<PathBuf>) -> Result<Self, VapidError> {
        let path = path.into();
        tokio::task::spawn_blocking(move || Self::load_or_generate_blocking(&path))
            .await
            .map_err(|_| VapidError::Task)?
    }

    fn load_or_generate_blocking(path: &Path) -> Result<Self, VapidError> {
        if let Some(keys) = Self::load_existing(path)? {
            return Ok(keys);
        }
        let (keys, bytes) = Self::generate(path)?;
        if create_atomically(path, &bytes).map_err(io_error(path))? {
            return Ok(keys);
        }
        // Another process created it first: theirs is the key browsers get.
        let bytes = std::fs::read(path).map_err(io_error(path))?;
        Self::parse_file(path, &bytes)
    }

    /// The key pair in the file at `path`; `None` when there is no file.
    fn load_existing(path: &Path) -> Result<Option<Self>, VapidError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(path)(error)),
        };
        tighten_permissions(path).map_err(io_error(path))?;
        Self::parse_file(path, &bytes).map(Some)
    }

    /// A new key pair, and the file that keeps it at `path`.
    fn generate(path: &Path) -> Result<(Self, Vec<u8>), VapidError> {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| VapidError::Random)?;
        let keys = Self::from_pkcs8(pkcs8.as_ref(), true).map_err(|_| VapidError::Random)?;
        let file = KeyFile {
            version: FILE_VERSION,
            pkcs8: base64url(pkcs8.as_ref()),
            public_key: keys.public_key_base64url(),
        };
        let mut bytes = serde_json::to_vec_pretty(&file)
            .map_err(|error| io_error(path)(io::Error::other(error)))?;
        bytes.push(b'\n');
        Ok((keys, bytes))
    }

    /// The key pair in `bytes`, read from `path`; `Corrupt` naming `path`
    /// when it does not hold one.
    fn parse_file(path: &Path, bytes: &[u8]) -> Result<Self, VapidError> {
        Self::parse(bytes, false).map_err(|reason| VapidError::Corrupt {
            path: path.to_owned(),
            reason,
        })
    }

    fn parse(bytes: &[u8], generated: bool) -> Result<Self, String> {
        let file: KeyFile = serde_json::from_slice(bytes)
            .map_err(|error| crate::secret::log_safe(&error.to_string(), 120))?;
        if file.version != FILE_VERSION {
            return Err(format!("unsupported version {}", file.version));
        }
        let pkcs8 = base64url_decode(&file.pkcs8).ok_or("the key is not base64url")?;
        let keys = Self::from_pkcs8(&pkcs8, generated)?;
        if keys.public_key_base64url() != file.public_key {
            return Err("the public key does not match the private key".to_owned());
        }
        Ok(keys)
    }

    fn from_pkcs8(pkcs8: &[u8], generated: bool) -> Result<Self, String> {
        let key_pair = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8,
            &SystemRandom::new(),
        )
        .map_err(|error| format!("not a P-256 key pair: {error}"))?;
        let public = key_pair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| "the public key is not an uncompressed point".to_owned())?;
        Ok(Self {
            key_pair,
            public,
            generated,
        })
    }

    /// What `GET /api/v1/push/key` returns.
    pub fn push_key(&self) -> PushKey {
        PushKey {
            public_key: self.public_key_base64url(),
        }
    }

    /// The uncompressed public point, base64url without padding: what a
    /// browser passes as `applicationServerKey`.
    pub fn public_key_base64url(&self) -> String {
        base64url(&self.public)
    }

    /// Whether this call created the key (a first start), rather than
    /// loading it.
    pub fn was_generated(&self) -> bool {
        self.generated
    }

    /// The `Authorization` header value for a push service at `audience`
    /// (its origin), per RFC 8292 section 3.
    pub(crate) fn authorization(
        &self,
        audience: &str,
        subject: &str,
        now_secs: u64,
    ) -> Result<String, VapidError> {
        let header = base64url(br#"{"typ":"JWT","alg":"ES256"}"#);
        // A `Value` displays without failing, unlike serializing to bytes.
        let claims = serde_json::json!({
            "aud": audience,
            "exp": now_secs.saturating_add(TOKEN_LIFETIME_SECS),
            "sub": subject,
        });
        let claims = base64url(claims.to_string().as_bytes());
        let signing_input = format!("{header}.{claims}");
        let signature = self
            .key_pair
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|_| VapidError::Random)?;
        Ok(format!(
            "vapid t={signing_input}.{}, k={}",
            base64url(signature.as_ref()),
            self.public_key_base64url()
        ))
    }
}

fn io_error(path: &Path) -> impl Fn(io::Error) -> VapidError + '_ {
    move |source| VapidError::Io {
        path: path.to_owned(),
        source,
    }
}

impl fmt::Debug for VapidKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The public key is public; the private half is never shown.
        f.debug_struct("VapidKeys")
            .field("public_key", &self.public_key_base64url())
            .field("private_key", &"REDACTED")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_key_is_created_once_then_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vapid.json");
        let first = VapidKeys::load_or_generate(&path).await.unwrap();
        assert!(first.was_generated());
        #[cfg(unix)]
        assert_eq!(crate::fsio::mode(&path), 0o600);
        let public = first.push_key().public_key;
        assert_eq!(base64url_decode(&public).unwrap().len(), 65);
        assert!(!public.contains('='));

        let second = VapidKeys::load_or_generate(&path).await.unwrap();
        assert!(!second.was_generated());
        assert_eq!(second.push_key().public_key, public);
        assert!(!dir.path().join("vapid.json.tmp").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_readable_key_file_is_tightened() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vapid.json");
        VapidKeys::load_or_generate(&path).await.unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        VapidKeys::load_or_generate(&path).await.unwrap();
        assert_eq!(crate::fsio::mode(&path), 0o600);
    }

    #[tokio::test]
    async fn a_corrupt_key_file_is_refused_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vapid.json");
        let good = {
            VapidKeys::load_or_generate(&path).await.unwrap();
            std::fs::read_to_string(&path).unwrap()
        };
        let other_public = good.replace("\"public_key\": \"B", "\"public_key\": \"C");
        let truncated_pkcs8 = {
            let file: serde_json::Value = serde_json::from_str(&good).unwrap();
            let pkcs8 = file["pkcs8"].as_str().unwrap();
            good.replace(pkcs8, &pkcs8[..40])
        };
        for content in [
            "".to_owned(),
            "not json".to_owned(),
            "{}".to_owned(),
            good.replace("\"version\": 1", "\"version\": 2"),
            other_public,
            truncated_pkcs8,
        ] {
            std::fs::write(&path, &content).unwrap();
            let error = VapidKeys::load_or_generate(&path).await.unwrap_err();
            assert!(matches!(error, VapidError::Corrupt { .. }), "{content:?}");
            assert!(
                error.to_string().contains(&path.display().to_string()),
                "{error}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content, "kept");
        }
    }

    #[tokio::test]
    async fn a_missing_directory_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = VapidKeys::load_or_generate(dir.path().join("no/such/vapid.json"))
            .await
            .unwrap_err();
        assert!(matches!(error, VapidError::Io { .. }), "{error}");
    }

    #[tokio::test]
    async fn a_key_file_that_cannot_be_read_is_an_io_error_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be: there, but not readable as one.
        let path = dir.path().join("vapid.json");
        std::fs::create_dir(&path).unwrap();
        let error = VapidKeys::load_or_generate(&path).await.unwrap_err();
        let VapidError::Io { path: named, .. } = &error else {
            panic!("{error}");
        };
        assert_eq!(named, &path);
        assert!(path.is_dir());
    }

    #[tokio::test]
    async fn the_token_is_an_es256_jwt_verifiable_with_the_public_key() {
        use p256::ecdsa::signature::Verifier as _;
        let dir = tempfile::tempdir().unwrap();
        let keys = VapidKeys::load_or_generate(dir.path().join("v"))
            .await
            .unwrap();
        let header = keys
            .authorization("https://push.example.net", "mailto:ops@example.com", 1_000)
            .unwrap();
        let rest = header.strip_prefix("vapid t=").unwrap();
        let (token, k) = rest.split_once(", k=").unwrap();
        assert_eq!(k, keys.public_key_base64url());
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let jose: serde_json::Value =
            serde_json::from_slice(&base64url_decode(parts[0]).unwrap()).unwrap();
        assert_eq!(jose, serde_json::json!({"typ": "JWT", "alg": "ES256"}));
        let claims: serde_json::Value =
            serde_json::from_slice(&base64url_decode(parts[1]).unwrap()).unwrap();
        assert_eq!(
            claims,
            serde_json::json!({
                "aud": "https://push.example.net",
                "exp": 1_000 + 12 * 3600,
                "sub": "mailto:ops@example.com",
            })
        );
        let verifying =
            p256::ecdsa::VerifyingKey::from_sec1_bytes(&base64url_decode(k).unwrap()).unwrap();
        let signature =
            p256::ecdsa::Signature::from_slice(&base64url_decode(parts[2]).unwrap()).unwrap();
        let signed = format!("{}.{}", parts[0], parts[1]);
        verifying.verify(signed.as_bytes(), &signature).unwrap();
        assert!(verifying.verify(b"other", &signature).is_err());
    }

    #[tokio::test]
    async fn debug_shows_no_private_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v");
        let keys = VapidKeys::load_or_generate(&path).await.unwrap();
        let file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let pkcs8 = file["pkcs8"].as_str().unwrap();
        let shown = format!("{keys:?}");
        assert!(!shown.contains(pkcs8), "{shown}");
        assert!(!shown.contains(&pkcs8[pkcs8.len() - 20..]), "{shown}");
        assert!(shown.contains("REDACTED"));
    }
}
