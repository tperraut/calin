//! The plan limits agents report (integrations spec, section 1): the
//! newest report of each runner, kept in memory for `GET /usage/limits`,
//! and the rule that decides which reports go into a session's log.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use calin_proto::{LimitWindow, RunnerLimits};

/// An agent's limits are logged at most this often while nothing moves.
const LOG_INTERVAL_MS: i64 = 60_000;
/// A move of the used share by more than this is logged at once: one point.
const LOG_STEP: f64 = 0.01;
/// What an agent last logged is forgotten after this long, so the map does
/// not keep every agent that ever ran.
const FORGET_AFTER_MS: i64 = 60 * 60 * 1000;

#[derive(Default)]
pub(crate) struct LimitBoard {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// By runner id.
    newest: BTreeMap<String, RunnerLimits>,
    /// By session and agent: when its limits were last logged, and what they were.
    logged: HashMap<(String, String), (i64, Vec<LimitWindow>)>,
}

impl LimitBoard {
    fn state(&self) -> MutexGuard<'_, State> {
        // Each critical section is one lookup and one insert: consistent whatever panicked.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `windows` as `runner`'s newest report and says whether the
    /// agent's log should get a `limits` event for it now. When it should,
    /// the report counts as logged.
    pub(crate) fn report(
        &self,
        runner: &str,
        session_id: &str,
        agent_id: &str,
        windows: &[LimitWindow],
        now: i64,
    ) -> bool {
        let mut state = self.state();
        state.newest.insert(
            runner.to_owned(),
            RunnerLimits {
                runner: runner.to_owned(),
                windows: windows.to_vec(),
                reported_at: now,
            },
        );
        let key = (session_id.to_owned(), agent_id.to_owned());
        if !worth_logging(state.logged.get(&key), windows, now) {
            return false;
        }
        state
            .logged
            .retain(|_, (at, _)| now.saturating_sub(*at) < FORGET_AFTER_MS);
        state.logged.insert(key, (now, windows.to_vec()));
        true
    }

    /// The newest report of each runner that reported since the start, by runner id.
    pub(crate) fn newest(&self) -> Vec<RunnerLimits> {
        self.state().newest.values().cloned().collect()
    }
}

/// At most once a minute per agent, unless a used share moved by more than
/// a point, a window refuses or stops refusing, or a window came or went.
fn worth_logging(
    last: Option<&(i64, Vec<LimitWindow>)>,
    windows: &[LimitWindow],
    now: i64,
) -> bool {
    let Some((at, logged)) = last else {
        return true;
    };
    if now.saturating_sub(*at) >= LOG_INTERVAL_MS || logged.len() != windows.len() {
        return true;
    }
    windows.iter().any(
        |window| match logged.iter().find(|old| old.name == window.name) {
            None => true,
            Some(old) => old.refused != window.refused || (old.used - window.used).abs() > LOG_STEP,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(name: &str, used: f64, refused: bool) -> LimitWindow {
        LimitWindow {
            name: name.to_owned(),
            used,
            resets_at: Some(1_000),
            refused,
        }
    }

    #[test]
    fn limits_are_logged_once_a_minute_unless_something_moves() {
        let board = LimitBoard::default();
        let at = |used: f64| {
            [
                window("five_hour", used, false),
                window("seven_day", 0.3, false),
            ]
        };
        let t0 = 1_700_000_000_000;
        // The first report of an agent is always logged.
        assert!(board.report("claude", "s1", "a1", &at(0.40), t0));
        // Small moves within the minute are not.
        assert!(!board.report("claude", "s1", "a1", &at(0.405), t0 + 1_000));
        assert!(!board.report("claude", "s1", "a1", &at(0.41), t0 + 2_000));
        // Compared with what was logged, not with the last report.
        assert!(board.report("claude", "s1", "a1", &at(0.4101), t0 + 3_000));
        // A minute after the last logged one, even unchanged.
        assert!(!board.report("claude", "s1", "a1", &at(0.4101), t0 + 62_999));
        assert!(board.report("claude", "s1", "a1", &at(0.4101), t0 + 63_000));
        // Refusing, or no longer refusing, is logged at once.
        let refusing = [
            window("five_hour", 0.4101, true),
            window("seven_day", 0.3, false),
        ];
        assert!(board.report("claude", "s1", "a1", &refusing, t0 + 64_000));
        assert!(!board.report("claude", "s1", "a1", &refusing, t0 + 65_000));
        assert!(board.report("claude", "s1", "a1", &at(0.4101), t0 + 66_000));
        // A window that appears or goes.
        assert!(board.report("claude", "s1", "a1", &at(0.4101)[..1], t0 + 67_000));
        let other = [window("opus", 0.4101, false)];
        assert!(board.report("claude", "s1", "a1", &other, t0 + 68_000));
        // Every agent has its own minute.
        assert!(board.report("claude", "s1", "a2", &at(0.4101), t0 + 68_500));
        assert!(board.report("claude", "s2", "a1", &at(0.4101), t0 + 69_000));
    }

    #[test]
    fn the_newest_report_of_each_runner_is_kept() {
        let board = LimitBoard::default();
        assert_eq!(board.newest(), []);
        board.report("mock", "s1", "a1", &[window("five_hour", 0.1, false)], 10);
        board.report("claude", "s2", "a2", &[window("five_hour", 0.2, false)], 20);
        // Kept as newest even when it is not logged.
        board.report("mock", "s3", "a3", &[window("five_hour", 0.15, false)], 30);
        board.report("mock", "s3", "a3", &[window("five_hour", 0.151, false)], 40);
        let newest = board.newest();
        assert_eq!(
            newest,
            [
                RunnerLimits {
                    runner: "claude".into(),
                    windows: vec![window("five_hour", 0.2, false)],
                    reported_at: 20,
                },
                RunnerLimits {
                    runner: "mock".into(),
                    windows: vec![window("five_hour", 0.151, false)],
                    reported_at: 40,
                },
            ]
        );
    }

    #[test]
    fn agents_not_heard_of_for_an_hour_are_forgotten() {
        let board = LimitBoard::default();
        let windows = [window("five_hour", 0.1, false)];
        board.report("mock", "s1", "a1", &windows, 0);
        board.report("mock", "s2", "a2", &windows, FORGET_AFTER_MS + 1);
        assert_eq!(board.state().logged.len(), 1);
    }
}
