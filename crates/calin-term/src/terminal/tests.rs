//! Tests against real ptys. Programs are explicit `/bin/sh -c` scripts so that
//! nothing depends on the user's shell configuration.

use std::future::Future;
use std::time::{Duration, Instant};

use rustix::process::kill_process;
use tokio::time::{sleep, timeout};

use super::*;

/// Fails a test that hangs instead of blocking the suite. Dropping the test's
/// future drops its terminal, which kills the program.
async fn within<T>(test: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(30), test)
        .await
        .expect("test timed out")
}

fn sh(script: &str) -> TerminalConfig {
    TerminalConfig::new("/").command("/bin/sh", ["-c", script])
}

fn spawn(config: &TerminalConfig) -> (Terminal, Output) {
    Terminal::spawn(config).expect("terminal starts")
}

/// Everything the program writes until its output ends.
async fn read_all(output: &mut Output) -> String {
    let mut bytes = Vec::new();
    while let Some(chunk) = output.recv().await {
        bytes.extend(chunk);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Reads into `seen` until it contains `needle`.
async fn read_until(output: &mut Output, seen: &mut String, needle: &str) {
    while !seen.contains(needle) {
        let chunk = output
            .recv()
            .await
            .unwrap_or_else(|| panic!("output ended before {needle:?}; got {seen:?}"));
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
}

/// Reads until the program has printed `<key>:<number>.` and returns the number.
async fn read_pid(output: &mut Output, seen: &mut String, key: &str) -> u32 {
    let marker = format!("{key}:");
    loop {
        if let Some(start) = seen.find(&marker).map(|at| at + marker.len())
            && let Some(len) = seen[start..].find('.')
        {
            return seen[start..start + len].parse().expect("a pid");
        }
        let chunk = output.recv().await.expect("output ended before the pid");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
}

fn stat(pid: u32) -> Option<session::Stat> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    session::parse_stat(&stat)
}

/// Whether the process has exited (a zombie has, even if not reaped yet).
fn process_gone(pid: u32) -> bool {
    stat(pid).is_none_or(|stat| stat.zombie)
}

/// Names of this process's threads that belong to terminals `ids`.
fn live_threads(ids: std::ops::Range<u64>) -> Vec<String> {
    let names: Vec<String> = std::fs::read_dir("/proc/self/task")
        .expect("task directory")
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("comm")).ok())
        .map(|name| name.trim().to_owned())
        .collect();
    ids.flat_map(|id| {
        [
            format!("pty-out-{id}"),
            format!("pty-in-{id}"),
            format!("pty-wait-{id}"),
        ]
    })
    .filter(|name| names.contains(name))
    .collect()
}

async fn threads_gone(ids: std::ops::Range<u64>) {
    while !live_threads(ids.clone()).is_empty() {
        sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn terminal_can_be_shared_between_tasks() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Terminal>();
    assert_send_sync::<TermError>();
}

#[tokio::test]
async fn delivers_output_then_ends_with_exit_code_zero() {
    within(async {
        let (terminal, mut output) = spawn(&sh("printf 'hello %s' world"));
        let id = terminal.id;
        assert_eq!(read_all(&mut output).await, "hello world");
        assert_eq!(terminal.wait().await, Some(0));
        // Asking again gives the same answer.
        assert_eq!(terminal.wait().await, Some(0));
        // The terminal is still held, yet its threads are gone.
        threads_gone(id..id + 1).await;
    })
    .await;
}

#[tokio::test]
async fn reports_a_non_zero_exit_code() {
    within(async {
        let (terminal, mut output) = spawn(&sh("echo bye; exit 3"));
        assert_eq!(terminal.wait().await, Some(3));
        assert_eq!(read_all(&mut output).await, "bye\r\n");
    })
    .await;
}

#[tokio::test]
async fn starts_in_the_working_directory() {
    within(async {
        let dir = tempfile::tempdir().expect("temporary directory");
        let cwd = dir.path().canonicalize().expect("canonical path");
        let config = TerminalConfig::new(&cwd).command("/bin/sh", ["-c", "pwd -P"]);
        let (terminal, mut output) = spawn(&config);
        assert_eq!(
            read_all(&mut output).await.trim_end(),
            cwd.to_string_lossy()
        );
        assert_eq!(terminal.wait().await, Some(0));
    })
    .await;
}

#[tokio::test]
async fn input_reaches_the_program_and_extra_environment_is_visible() {
    within(async {
        let config = sh(r#"read line; echo "got:$line:$CALIN_TEST_VALUE""#)
            .env("CALIN_TEST_VALUE", "from-config");
        let (terminal, mut output) = spawn(&config);
        terminal.write("typed\n").await.expect("write");
        let text = read_all(&mut output).await;
        assert!(text.contains("got:typed:from-config"), "got {text:?}");
        assert_eq!(terminal.wait().await, Some(0));
    })
    .await;
}

#[tokio::test]
async fn sets_term_and_colorterm_over_the_configuration() {
    within(async {
        let config = sh(r#"echo "$TERM/$COLORTERM""#)
            .env("TERM", "dumb")
            .env("COLORTERM", "no");
        let (_terminal, mut output) = spawn(&config);
        assert_eq!(
            read_all(&mut output).await.trim_end(),
            "xterm-256color/truecolor"
        );
    })
    .await;
}

#[tokio::test]
async fn resize_is_visible_to_the_program_and_clamped() {
    within(async {
        let script = "stty size; read a; stty size; read b; stty size";
        let (terminal, mut output) = spawn(&sh(script).size(100, 30));
        let mut seen = String::new();
        read_until(&mut output, &mut seen, "30 100\r\n").await;

        terminal.resize(132, 43).expect("resize");
        terminal.write("\n").await.expect("write");
        read_until(&mut output, &mut seen, "43 132\r\n").await;

        terminal.resize(0, 5000).expect("resize");
        terminal.write("\n").await.expect("write");
        read_until(&mut output, &mut seen, "1000 1\r\n").await;
        assert_eq!(terminal.wait().await, Some(0));
    })
    .await;
}

#[tokio::test]
async fn initial_size_is_clamped() {
    within(async {
        let (_terminal, mut output) = spawn(&sh("stty size").size(0, 0));
        assert_eq!(read_all(&mut output).await, "1 1\r\n");
    })
    .await;
}

#[tokio::test]
async fn kill_ends_the_program_and_its_background_child() {
    within(async {
        let script = r#"sleep 1000 & echo "child:$!."; sleep 1000"#;
        let (terminal, mut output) = spawn(&sh(script));
        let id = terminal.id;
        let child = read_pid(&mut output, &mut String::new(), "child").await;
        assert!(!process_gone(child));
        // Proves that `threads_gone` below can see this terminal's threads.
        while live_threads(id..id + 1).len() < 3 {
            sleep(Duration::from_millis(1)).await;
        }

        let killed_at = Instant::now();
        terminal.kill();
        assert_eq!(terminal.wait().await, None);
        assert!(killed_at.elapsed() < Duration::from_secs(2));
        assert!(process_gone(child));
        assert!(process_gone(terminal.pid()));

        // Whatever was still in flight, the output does end.
        read_all(&mut output).await;
        threads_gone(id..id + 1).await;
        // Killing again is harmless, and the terminal takes nothing more.
        terminal.kill();
        assert!(matches!(terminal.write("x").await, Err(TermError::Closed)));
        assert!(matches!(terminal.resize(80, 24), Err(TermError::Closed)));
    })
    .await;
}

#[tokio::test]
async fn kill_reaches_jobs_in_their_own_process_group() {
    within(async {
        // `set -m` is what an interactive shell does: one process group per job.
        let script = r#"set -m; sleep 1000 & echo "child:$!."; wait"#;
        let (terminal, mut output) = spawn(&sh(script));
        let child = read_pid(&mut output, &mut String::new(), "child").await;
        let child_stat = stat(child).expect("child is running");
        assert_eq!(child_stat.group, i32::try_from(child).expect("pid"));
        assert_eq!(
            child_stat.session,
            i32::try_from(terminal.pid()).expect("pid")
        );

        terminal.kill();
        assert_eq!(terminal.wait().await, None);
        assert!(process_gone(child));
    })
    .await;
}

#[tokio::test]
async fn exit_of_the_program_kills_what_it_left_in_its_session() {
    within(async {
        // The child ignores the hang-up the kernel sends when the shell exits.
        let script = r#"trap "" HUP; sleep 1000 & echo "child:$!."; exit 7"#;
        let (terminal, mut output) = spawn(&sh(script));
        let child = read_pid(&mut output, &mut String::new(), "child").await;
        assert_eq!(terminal.wait().await, Some(7));
        assert!(process_gone(child));
        // Whatever was still in flight, the output does end.
        read_all(&mut output).await;
    })
    .await;
}

#[tokio::test]
async fn dropping_the_terminal_kills_the_program() {
    within(async {
        let script = r#"sleep 1000 & echo "child:$!."; echo "shell:$$."; wait"#;
        let (terminal, mut output) = spawn(&sh(script));
        let id = terminal.id;
        let mut seen = String::new();
        let child = read_pid(&mut output, &mut seen, "child").await;
        let shell = read_pid(&mut output, &mut seen, "shell").await;
        assert_eq!(shell, terminal.pid());

        drop(terminal);
        // Whatever was still in flight, the output does end.
        read_all(&mut output).await;
        // The tty closes a moment before its last process is done exiting, and
        // without the terminal there is no `wait` to rely on.
        while !(process_gone(shell) && process_gone(child)) {
            sleep(Duration::from_millis(5)).await;
        }
        threads_gone(id..id + 1).await;
    })
    .await;
}

#[tokio::test]
async fn slow_consumer_gets_every_byte_in_order() {
    within(async {
        const LINES: u32 = 750_000;
        let (terminal, mut output) = spawn(&sh(&format!("seq 1 {LINES}")));
        // Nothing is read yet: the program must block on the full queue rather
        // than run to completion into memory.
        assert!(
            timeout(Duration::from_millis(300), terminal.wait())
                .await
                .is_err()
        );
        assert!(output.len() <= OUTPUT_QUEUE);

        let mut received = Vec::new();
        let mut chunks = 0;
        while let Some(chunk) = output.recv().await {
            received.extend(chunk);
            chunks += 1;
            if chunks % 4 == 0 {
                sleep(Duration::from_millis(1)).await;
            }
        }
        let expected: String = (1..=LINES).map(|n| format!("{n}\r\n")).collect();
        assert!(expected.len() > 5_000_000);
        assert_eq!(received.len(), expected.len());
        let mismatch = received
            .iter()
            .zip(expected.as_bytes())
            .position(|(a, b)| a != b);
        assert_eq!(mismatch, None, "first differing byte");
        assert_eq!(terminal.wait().await, Some(0));
    })
    .await;
}

#[tokio::test]
async fn input_backpressure_and_kill_with_input_pending() {
    within(async {
        // Raw mode, so that the tty keeps unread input instead of editing it.
        let script = "stty raw -echo; echo ready; exec sleep 1000";
        let (terminal, mut output) = spawn(&sh(script));
        let id = terminal.id;
        read_until(&mut output, &mut String::new(), "ready").await;

        // The program never reads: the tty, then the queue, fill up.
        let mut stalled = false;
        for _ in 0..500 {
            let write = terminal.write(vec![b'x'; 64 * 1024]);
            if timeout(Duration::from_millis(200), write).await.is_err() {
                stalled = true;
                break;
            }
        }
        assert!(stalled, "writes never waited for the program");

        // The input thread is stuck on a full tty; the kill must release it.
        terminal.kill();
        assert_eq!(terminal.wait().await, None);
        assert_eq!(read_all(&mut output).await, "");
        threads_gone(id..id + 1).await;
        assert!(matches!(terminal.write("x").await, Err(TermError::Closed)));
    })
    .await;
}

#[tokio::test]
async fn output_ends_even_if_a_detached_process_keeps_the_tty() {
    within(async {
        // The child leaves the session with the tty still open, which `kill`
        // cannot reach; the shell waits for it to have done so.
        let script = r#"setsid sh -c 'echo detached; exec sleep 1000' & echo "child:$!."; read go; echo bye"#;
        let (terminal, mut output) = spawn(&sh(script));
        let id = terminal.id;
        let mut seen = String::new();
        let child = read_pid(&mut output, &mut seen, "child").await;
        read_until(&mut output, &mut seen, "detached").await;
        terminal.write("\n").await.expect("write");

        let code = terminal.wait().await;
        let rest = timeout(Duration::from_secs(5), read_all(&mut output)).await;
        let survived = !process_gone(child);
        // Clean up before asserting, so a failure leaves no process behind.
        let pid = Pid::from_raw(i32::try_from(child).expect("pid")).expect("pid");
        let _ = kill_process(pid, Signal::KILL);

        assert_eq!(code, Some(0));
        let rest = rest.expect("output ended although the tty is still held");
        assert!(rest.contains("bye"), "got {rest:?}");
        assert!(survived, "a process outside the session is not ours to kill");
        threads_gone(id..id + 1).await;
    })
    .await;
}

#[tokio::test]
async fn spawn_failure_reports_the_reason_and_leaves_no_thread() {
    within(async {
        let first = NEXT_ID.load(Ordering::Relaxed);
        let missing = TerminalConfig::new("/").command("/nonexistent/program", [""; 0]);
        match Terminal::spawn(&missing) {
            Err(TermError::Spawn(reason)) => {
                assert!(reason.contains("/nonexistent/program"), "got {reason:?}");
            }
            other => panic!("expected a spawn failure, got {other:?}"),
        }
        let nowhere = TerminalConfig::new("/nonexistent/directory").command("/bin/sh", [""; 0]);
        match Terminal::spawn(&nowhere) {
            Err(TermError::Spawn(reason)) => {
                assert!(reason.contains("/nonexistent/directory"), "got {reason:?}");
            }
            other => panic!("expected a spawn failure, got {other:?}"),
        }
        // Other tests may own some of these ids; their terminals end as well.
        threads_gone(first..NEXT_ID.load(Ordering::Relaxed)).await;
    })
    .await;
}
