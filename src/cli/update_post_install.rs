//! Capability probe is side-effect free: only clap's usage exit permits fallback.
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

pub(super) const SUPPORTED: i32 = 73;
/// The hook ran but no owned daemon reloaded (0 means at least one did).
pub(super) const NOTHING_RELOADED: i32 = 74;
const MARKER: &str = "JCODE_POST_INSTALL_REEXEC";

#[derive(Clone, Copy, Debug)]
enum Outcome {
    Exited(Option<i32>),
    SpawnFailed,
    Indeterminate,
}

fn fallback(outcome: Outcome, reload_started: bool) -> bool {
    matches!(outcome, Outcome::SpawnFailed)
        || (!reload_started && matches!(outcome, Outcome::Exited(Some(2))))
}

/// Runs the new binary's post-install reload. `None` means the caller must
/// fall back to reloading itself; `Some(reloaded)` reports the hook's result.
pub(super) fn run(exe: &Path, reason: &str) -> Option<bool> {
    if std::env::var_os(MARKER).is_some() {
        return Some(false);
    }
    let command = || {
        let mut cmd = Command::new(exe);
        cmd.args(["--no-update", "update-post-install", "--reason", reason])
            .env(MARKER, "1");
        cmd
    };
    let probe = wait(command().arg("--probe"), Duration::from_secs(10));
    if fallback(probe, false) {
        return None;
    }
    if matches!(probe, Outcome::Exited(Some(SUPPORTED))) {
        // Once reload starts, even a timeout/crash may have reloaded some sockets.
        // Only failure to spawn proves no work happened.
        let outcome = wait(&mut command(), Duration::from_secs(120));
        if fallback(outcome, true) {
            return None;
        }
        return Some(reported_reload(outcome));
    }
    Some(false)
}

/// Only a clean exit proves a reload; anything else may not have reloaded.
fn reported_reload(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Exited(Some(0)))
}

fn wait(command: &mut Command, timeout: Duration) -> Outcome {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("update: could not spawn post-install command: {error}");
            return Outcome::SpawnFailed;
        }
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Outcome::Exited(status.code()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            result => {
                eprintln!(
                    "update: post-install command timed out or could not be waited on: {result:?}"
                );
                if let Err(error) = child.kill() {
                    eprintln!("update: could not stop post-install command: {error}");
                }
                if let Err(error) = child.wait() {
                    eprintln!("update: could not reap post-install command: {error}");
                }
                return Outcome::Indeterminate;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_and_spawn_failure_fall_back() {
        assert!(fallback(Outcome::Exited(Some(2)), false));
        assert!(fallback(Outcome::SpawnFailed, false));
    }

    #[test]
    fn only_a_clean_hook_exit_reports_a_reload() {
        assert!(reported_reload(Outcome::Exited(Some(0))));
        assert!(!reported_reload(Outcome::Exited(Some(NOTHING_RELOADED))));
        assert!(!reported_reload(Outcome::Exited(None)));
        assert!(!reported_reload(Outcome::Indeterminate));
    }

    #[test]
    fn ran_including_socket_failures_never_falls_back() {
        assert!(!fallback(Outcome::Exited(Some(2)), true));
        assert!(fallback(Outcome::SpawnFailed, true));
        // Socket failures are reported in stderr; the command still exits zero.
        assert!(!fallback(Outcome::Exited(Some(0)), false));
        assert!(!fallback(Outcome::Exited(Some(SUPPORTED)), false));
        assert!(!fallback(Outcome::Exited(Some(1)), false));
        assert!(!fallback(Outcome::Exited(None), false));
        assert!(!fallback(Outcome::Indeterminate, false));
    }
}
