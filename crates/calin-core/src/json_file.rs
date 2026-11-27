//! Small configuration files in the data directory, written whole.

use std::io::Write as _;
use std::path::Path;

use serde::Serialize;

use crate::error::Result;

/// Writes `value` next to the old file and renames it into place, so a crash
/// leaves the previous file or the new one, never half of either.
pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let temp = path.with_extension("json.tmp");
    let written = std::fs::File::create(&temp).and_then(|mut out| {
        out.write_all(&bytes)?;
        out.sync_all()
    });
    if let Err(err) = written.and_then(|()| std::fs::rename(&temp, path)) {
        // Best effort: the write already failed, and that is the error to report.
        let _ = std::fs::remove_file(&temp);
        return Err(err.into());
    }
    Ok(())
}
