//! Update-side fanout. The reload protocol and force semantics remain in the CLI.
use std::path::Path;

/// Returns true when at least one running daemon accepted the reload.
pub(super) fn reload_all(exe: &Path, reason: &str) -> bool {
    // Updates can be called from inside a Tokio runtime; probe on a dedicated thread.
    let exe = exe.to_path_buf();
    let reason = reason.to_owned();
    let result = std::thread::spawn(move || -> anyhow::Result<bool> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let live = runtime.block_on(crate::server::live_daemon_sockets());
        Ok(reload_candidates(
            crate::server::daemon_socket_candidates(),
            &live,
            |socket| {
                let output = reload_command(&exe, socket, true).output()?;
                // A build without `server reload --json` exits with clap's usage code.
                if output.status.code() == Some(2) {
                    return reload_command(&exe, socket, false).output();
                }
                Ok(output)
            },
            &reason,
            report,
        ))
    })
    .join();
    match result {
        Ok(Ok(reloaded)) => reloaded,
        Ok(Err(error)) => {
            report(&format!("update: daemon discovery failed: {error}"));
            false
        }
        Err(_) => {
            report("update: daemon discovery thread failed");
            false
        }
    }
}

fn reload_candidates(
    sockets: Vec<std::path::PathBuf>,
    live: &[std::path::PathBuf],
    mut run: impl FnMut(&Path) -> std::io::Result<std::process::Output>,
    reason: &str,
    mut report: impl FnMut(&str),
) -> bool {
    let mut found = false;
    let mut reloaded = false;
    for socket in sockets {
        let owned_live = live.contains(&socket);
        if !owned_live {
            report(&format!(
                "update: {}: skipped (no responsive owned jcode server)",
                socket.display()
            ));
            continue;
        }
        found = true;
        match run(&socket) {
            Ok(output) if output.status.success() => {
                let (socket_reloaded, detail) = decode_reload(&output.stdout);
                reloaded |= socket_reloaded;
                report(&format!(
                    "update: {} after {}: {}",
                    socket.display(),
                    reason,
                    detail
                ));
            }
            Ok(output) => report(&format!(
                "update: {}: failed: {}",
                socket.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(error) => report(&format!("update: {}: failed: {error}", socket.display())),
        }
    }
    if !found {
        report(NO_DAEMONS);
    }
    reloaded
}

/// `server reload --json` exits 0 even when no listener remained to reload, so
/// read its `reloaded` field. Plain-text output (a build without `--json`)
/// keeps the legacy meaning: a zero exit is a reload.
fn decode_reload(stdout: &[u8]) -> (bool, String) {
    let text = String::from_utf8_lossy(stdout).trim().to_owned();
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(report) => (
            report
                .get("reloaded")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            report
                .get("detail")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| text.clone(), str::to_owned),
        ),
        Err(_) => (true, text),
    }
}

const NO_DAEMONS: &str = "update: no running jcode daemon found; nothing to reload.";

fn reload_command(exe: &Path, socket: &Path, json: bool) -> std::process::Command {
    let mut command = std::process::Command::new(exe);
    command
        .arg("--no-update")
        .arg("--socket")
        .arg(socket)
        .args(["server", "reload", "--force"]);
    if json {
        command.arg("--json");
    }
    command
}

fn report(message: &str) {
    crate::logging::info(message);
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_subprocess_explicitly_targets_fallback_socket() {
        let socket = Path::new("/temporary/fallback/jcode.sock");
        let command = reload_command(Path::new("jcode-test"), socket, false);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "--no-update",
                "--socket",
                "/temporary/fallback/jcode.sock",
                "server",
                "reload",
                "--force"
            ]
        );
    }

    #[test]
    fn fallback_target_reaches_runner_and_empty_discovery_reports_nothing() {
        let current = std::path::PathBuf::from("/test/current.sock");
        let fallback = std::path::PathBuf::from("/test/fallback.sock");
        let mut called = Vec::new();
        let mut messages = Vec::new();
        reload_candidates(
            vec![current.clone(), fallback.clone()],
            std::slice::from_ref(&fallback),
            |path| {
                called.push(path.to_path_buf());
                Err(std::io::Error::other("injected"))
            },
            "test",
            |message| messages.push(message.to_owned()),
        );
        assert_eq!(called, vec![fallback]);
        assert!(messages[0].contains("skipped"));
        assert!(messages[1].contains("failed"));
        messages.clear();
        reload_candidates(
            vec![current],
            &[],
            |_| panic!("must never start a daemon"),
            "test",
            |message| messages.push(message.to_owned()),
        );
        assert_eq!(messages.last().unwrap(), NO_DAEMONS);
    }

    #[test]
    fn json_report_decides_reloaded_and_plain_text_keeps_legacy_meaning() {
        let reloaded = br#"{"socket":"/s","reloaded":true,"detail":"Reloaded."}"#;
        assert_eq!(decode_reload(reloaded), (true, "Reloaded.".to_owned()));
        // Exit 0 with no listener left: not a reload.
        let idle = br#"{"socket":"/s","reloaded":false,"detail":"No running jcode server found; nothing to reload."}"#;
        assert!(!decode_reload(idle).0);
        assert_eq!(
            decode_reload(b"Server reloaded\n"),
            (true, "Server reloaded".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn successful_runs_report_reloaded_only_when_a_daemon_reloaded() {
        use std::os::unix::process::ExitStatusExt;
        let ok = |stdout: &[u8]| std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        };
        let socket = std::path::PathBuf::from("/test/a.sock");
        let live = [socket.clone()];
        let idle = br#"{"reloaded":false,"detail":"nothing to reload"}"#;
        assert!(!reload_candidates(
            vec![socket.clone()],
            &live,
            |_| Ok(ok(idle)),
            "t",
            |_| {}
        ));
        let done = br#"{"reloaded":true,"detail":"reloaded"}"#;
        assert!(reload_candidates(
            vec![socket],
            &live,
            |_| Ok(ok(done)),
            "t",
            |_| {}
        ));
    }

    #[test]
    fn nothing_found_message_is_explicit() {
        assert_eq!(
            NO_DAEMONS,
            "update: no running jcode daemon found; nothing to reload."
        );
    }
}
