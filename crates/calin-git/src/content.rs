//! Reading file content: size limits, hashing, and telling text from binary.

use std::fs::File;
use std::io::{ErrorKind, Read as _};
use std::path::Path;

use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result};

/// The bytes as text, or `None` when they are binary: a NUL byte or invalid UTF-8.
pub(crate) fn decode(bytes: Vec<u8>) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Lowercase hex SHA-256.
pub(crate) fn hash(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

fn hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The whole file, or `None` when it is larger than `max_bytes`. Never holds
/// more than the limit in memory.
pub(crate) fn read_capped(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    let file = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io(path, e))?;
    Ok((bytes.len() as u64 <= max_bytes).then_some(bytes))
}

/// A file read once, start to end.
pub(crate) struct Hashed {
    /// The content, or `None` when it is larger than the limit.
    pub(crate) bytes: Option<Vec<u8>>,
    pub(crate) size: u64,
    pub(crate) hash: String,
}

/// Hashes the whole file and keeps its content when it fits in `max_bytes`.
/// Size, hash and content come from the same pass, so they describe the same
/// bytes even while an agent is writing the file.
pub(crate) fn read_hashed(path: &Path, max_bytes: u64) -> Result<Hashed> {
    let mut file = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut kept = Some(Vec::new());
    let mut size = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::io(path, e)),
        };
        hasher.update(&buf[..n]);
        size += n as u64;
        if size > max_bytes {
            kept = None;
        } else if let Some(bytes) = kept.as_mut() {
            bytes.extend_from_slice(&buf[..n]);
        }
    }
    Ok(Hashed {
        bytes: kept,
        size,
        hash: hex(hasher.finalize().as_slice()),
    })
}
