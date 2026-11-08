//! Capability probe is side-effect free: only clap's usage exit permits fallback.
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

pub(super) const SUPPORTED: i32 = 73;
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

pub(super) fn needs_fallback(exe: &Path, reason: &str) -> bool {
    if std::env::var_os(MARKER).is_some() {
        return false;
    }
    let command = || {
        let mut cmd = Command::new(exe);
        cmd.args(["--no-update", "update-post-install", "--reason", reason])
            .env(MARKER, "1");
        cmd
    };
    let probe = wait(command().arg("--probe"), Duration::from_secs(10));
    if fallback(probe, false) {
        return true;
    }
    if matches!(probe, Outcome::Exited(Some(SUPPORTED))) {
        // Once reload starts, even a timeout/crash may have reloaded some sockets.
        // Only failure to spawn proves no work happened.
        return fallback(wait(&mut command(), Duration::from_secs(120)), true);
    }
    false
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
