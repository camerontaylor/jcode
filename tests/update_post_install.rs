#![cfg(target_os = "linux")]
use std::process::Command;

#[test]
fn hidden_post_install_reports_ran_without_daemons() {
    let binary = env!("CARGO_BIN_EXE_jcode");
    let help = Command::new(binary).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(!String::from_utf8_lossy(&help.stdout).contains("update-post-install"));
    let dir = tempfile::tempdir().unwrap();
    // Discovery deliberately includes /tmp regardless of runtime env overrides.
    // A private mount namespace prevents even probing the host fallback socket.
    let output = Command::new("unshare")
        .args(["-Urnm", "sh", "-c", "mount -t tmpfs tmpfs /tmp && exec \"$1\" --no-update update-post-install --reason integration", "sh", binary])
        .env("JCODE_RUNTIME_DIR", dir.path())
        .env("XDG_RUNTIME_DIR", dir.path())
        .env("TMPDIR", dir.path())
        .env("JCODE_SOCKET", dir.path().join("absent.sock"))
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("no running jcode daemon found"));
    let probe = Command::new(binary)
        .args([
            "--no-update",
            "update-post-install",
            "--reason",
            "test",
            "--probe",
        ])
        .output()
        .unwrap();
    assert_eq!(probe.status.code(), Some(73));
}
