//! Update-side fanout. The reload protocol and force semantics remain in the CLI.
use std::path::Path;

pub(super) fn reload_all(exe: &Path, reason: &str) {
    // Updates can be called from inside a Tokio runtime; probe on a dedicated thread.
    let exe = exe.to_path_buf();
    let reason = reason.to_owned();
    let result = std::thread::spawn(move || -> anyhow::Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let live = runtime.block_on(crate::server::live_daemon_sockets());
        reload_candidates(
            crate::server::daemon_socket_candidates(),
            &live,
            |socket| reload_command(&exe, socket).output(),
            &reason,
            report,
        );
        Ok(())
    })
    .join();
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => report(&format!("update: daemon discovery failed: {error}")),
        Err(_) => report("update: daemon discovery thread failed"),
    }
}

fn reload_candidates(
    sockets: Vec<std::path::PathBuf>,
    live: &[std::path::PathBuf],
    mut run: impl FnMut(&Path) -> std::io::Result<std::process::Output>,
    reason: &str,
    mut report: impl FnMut(&str),
) {
    let mut found = false;
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
                report(&format!(
                    "update: {} after {}: {}",
                    socket.display(),
                    reason,
                    String::from_utf8_lossy(&output.stdout).trim()
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
}

const NO_DAEMONS: &str = "update: no running jcode daemon found; nothing to reload.";

fn reload_command(exe: &Path, socket: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(exe);
    command
        .arg("--no-update")
        .arg("--socket")
        .arg(socket)
        .args(["server", "reload", "--force"]);
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
        let command = reload_command(Path::new("jcode-test"), socket);
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
    fn nothing_found_message_is_explicit() {
        assert_eq!(
            NO_DAEMONS,
            "update: no running jcode daemon found; nothing to reload."
        );
    }
}
