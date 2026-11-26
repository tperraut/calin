//! Killing what a terminal's program left behind.
//!
//! The program is the leader of its own session, so "everything it started" is
//! every process still in that session. A shell with job control puts each job
//! in its own process group, so signalling the leader's group is not enough.

use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};

/// How long to keep re-signalling a session whose processes will not die.
const SWEEP_LIMIT: Duration = Duration::from_secs(1);
const SWEEP_PAUSE: Duration = Duration::from_millis(2);

/// Kills every process in the session led by `leader` and waits, within
/// [`SWEEP_LIMIT`], until they are gone.
///
/// The caller must not have reaped `leader` yet: while it is a zombie its pid,
/// and so the session id, cannot be reused by an unrelated process.
pub(crate) fn kill_all(leader: Pid) {
    let deadline = Instant::now() + SWEEP_LIMIT;
    loop {
        let members = members(leader);
        for group in members.groups {
            // A group that vanished in the meantime is fine.
            let _ = kill_process_group(group, Signal::KILL);
        }
        // Scan again after signalling: a process may have forked into a new
        // group between the scan and the signal.
        if !members.alive || Instant::now() >= deadline {
            return;
        }
        thread::sleep(SWEEP_PAUSE);
    }
}

struct Members {
    groups: HashSet<Pid>,
    /// Whether any member has yet to exit.
    alive: bool,
}

#[cfg(target_os = "linux")]
fn members(leader: Pid) -> Members {
    let mut members = Members {
        groups: HashSet::new(),
        alive: false,
    };
    let Ok(entries) = std::fs::read_dir("/proc") else {
        members.groups.insert(leader);
        return members;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        // The process can exit between the listing and the read.
        let Ok(stat) = std::fs::read(entry.path().join("stat")) else {
            continue;
        };
        let Some(stat) = parse_stat(&String::from_utf8_lossy(&stat)) else {
            continue;
        };
        if stat.session == leader.as_raw_nonzero().get() {
            if let Some(group) = Pid::from_raw(stat.group) {
                members.groups.insert(group);
            }
            members.alive |= !stat.zombie;
        }
    }
    members
}

/// Without `/proc` there is no cheap way to list a session: only the leader's
/// own group is signalled.
#[cfg(not(target_os = "linux"))]
fn members(leader: Pid) -> Members {
    Members {
        groups: HashSet::from([leader]),
        alive: false,
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, PartialEq)]
pub(crate) struct Stat {
    pub(crate) zombie: bool,
    pub(crate) group: i32,
    pub(crate) session: i32,
}

/// Parses `/proc/<pid>/stat`: `pid (comm) state ppid pgrp session ...`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_stat(stat: &str) -> Option<Stat> {
    // `comm` can itself contain spaces and parentheses; the fields start after
    // the last closing one.
    let mut fields = stat[stat.rfind(')')? + 1..].split_ascii_whitespace();
    let state = fields.next()?;
    let _parent = fields.next()?;
    let group = fields.next()?.parse().ok()?;
    let session = fields.next()?.parse().ok()?;
    Some(Stat {
        // `X` is the instant between zombie and gone.
        zombie: matches!(state, "Z" | "X"),
        group,
        session,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_stat_line_with_an_awkward_command_name() {
        let stat = parse_stat("4242 (a b) c)) S 1 4200 4100 34816 4242 0 0").expect("parses");
        assert_eq!(
            stat,
            Stat {
                zombie: false,
                group: 4200,
                session: 4100
            }
        );
        let stat = parse_stat("7 (sh) Z 1 7 7 0 -1").expect("parses");
        assert!(stat.zombie);
        assert_eq!(parse_stat("garbage"), None);
    }
}
