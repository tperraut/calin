//! Reading and opening session logs: one `calin_proto::Event` per line.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use calin_proto::Event;

use crate::error::{Error, Result};

/// Opens a log for appending, creating it and its directory if needed.
pub(super) fn open_for_append(path: &Path) -> Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

/// Length of the log in bytes; a log that does not exist is empty.
pub(super) fn len(path: &Path) -> Result<u64> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err.into()),
    }
}

/// Parses one log line (without its newline) and checks that it is the event
/// expected at this point of the log.
pub(super) fn parse_line(line: &[u8], session_id: &str, expected_seq: u64) -> Result<Event> {
    let corrupt = |detail: String| Error::Corrupt {
        session: session_id.to_owned(),
        detail,
    };
    let event: Event = serde_json::from_slice(line)
        .map_err(|err| corrupt(format!("line {expected_seq} does not parse: {err}")))?;
    if event.session_id != session_id {
        return Err(corrupt(format!(
            "line {expected_seq} belongs to session {}",
            event.session_id
        )));
    }
    if event.seq != expected_seq {
        return Err(corrupt(format!(
            "expected seq {expected_seq}, found {}",
            event.seq
        )));
    }
    Ok(event)
}

/// The events stored in bytes `start..end` of a log, where `start` is the
/// offset of the event with sequence number `first_seq`.
pub(super) fn read_range(
    path: &Path,
    session_id: &str,
    first_seq: u64,
    start: u64,
    end: u64,
) -> Result<Vec<Event>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    // Appends may land while this runs; the range ends where the subscription was cut.
    let reader = BufReader::new(file.take(end.saturating_sub(start)));
    let mut events = Vec::new();
    for (seq, line) in (first_seq..).zip(reader.split(b'\n')) {
        events.push(parse_line(&line?, session_id, seq)?);
    }
    Ok(events)
}
