//! Reading file content: size limits, hashing, and telling text from binary.

use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::ops::ControlFlow;
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

pub(crate) fn hex(digest: &[u8]) -> String {
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

/// Hands what `reader` holds to `each`, one chunk at a time, until the end or
/// until `each` breaks. An interrupted read is tried again.
pub(crate) fn read_chunks(
    mut reader: impl Read,
    mut each: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                if each(&buf[..n]).is_break() {
                    return Ok(());
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
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
    let file = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut kept = Some(Vec::new());
    let mut size = 0u64;
    read_chunks(file, |chunk| {
        hasher.update(chunk);
        size += chunk.len() as u64;
        if size > max_bytes {
            kept = None;
        } else if let Some(bytes) = kept.as_mut() {
            bytes.extend_from_slice(chunk);
        }
        ControlFlow::Continue(())
    })
    .map_err(|e| Error::io(path, e))?;
    Ok(Hashed {
        bytes: kept,
        size,
        hash: hex(hasher.finalize().as_slice()),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::testing::Sandbox;

    /// Answers each read with the next result; a read past the last one is an error.
    struct Scripted(VecDeque<io::Result<&'static [u8]>>);

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let bytes = self
                .0
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::other("read past the end")))?;
            buf[..bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        }
    }

    #[test]
    fn chunks_are_read_up_to_the_end_and_an_interrupted_read_is_tried_again() {
        let reader = Scripted(VecDeque::from([
            Err(ErrorKind::Interrupted.into()),
            Ok(&b"ab"[..]),
            Err(ErrorKind::Interrupted.into()),
            Ok(&b"c"[..]),
            Ok(&b""[..]),
        ]));
        let mut seen = Vec::new();
        read_chunks(reader, |chunk| {
            seen.extend_from_slice(chunk);
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(seen, b"abc");
    }

    #[test]
    fn a_capped_read_keeps_a_file_at_the_limit_and_refuses_one_byte_more() {
        let sandbox = Sandbox::new();
        let path = sandbox.dir("files").join("f");
        std::fs::write(&path, "12345").unwrap();
        assert_eq!(
            read_capped(&path, 5).unwrap().as_deref(),
            Some(&b"12345"[..])
        );
        assert_eq!(read_capped(&path, 4).unwrap(), None);
    }
}
