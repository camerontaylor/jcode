use super::*;

#[test]
fn candidates_cover_environment_split_and_dedupe() {
    let temp = tempfile::tempdir().unwrap();
    let xdg = temp.path().join("xdg");
    std::fs::create_dir(&xdg).unwrap();
    let current = xdg.join("jcode.sock");
    let paths = candidates(
        current.clone(),
        |key| match key {
            "XDG_RUNTIME_DIR" | "JCODE_RUNTIME_DIR" => Some(xdg.clone()),
            "JCODE_SOCKET" => Some(current.clone()),
            _ => None,
        },
        temp.path().to_path_buf(),
    );
    assert_eq!(paths[0], current);
    assert_eq!(paths.iter().filter(|p| **p == current).count(), 1);
    assert!(
        paths.contains(&crate::storage::fallback_runtime_dir_in(temp.path()).join("jcode.sock"))
    );
    #[cfg(unix)]
    assert!(
        paths.contains(
            &crate::storage::fallback_runtime_dir_in(Path::new("/tmp")).join("jcode.sock")
        )
    );
    let override_path = temp.path().join("override.sock");
    let unset = candidates(override_path.clone(), |_| None, temp.path().to_path_buf());
    assert_eq!(unset[0], override_path);
}

#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
#[tokio::test]
async fn stale_socket_is_not_a_daemon() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(!is_live_daemon(&socket).await);
}

#[cfg(unix)]
#[tokio::test]
async fn fallback_daemon_answers_when_current_socket_is_elsewhere() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let temp = tempfile::tempdir().unwrap();
    let fallback = crate::storage::fallback_runtime_dir_in(temp.path());
    std::fs::create_dir(&fallback).unwrap();
    let socket = fallback.join("jcode.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let responder = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        BufReader::new(read).read_line(&mut line).await.unwrap();
        let ping: serde_json::Value = serde_json::from_str(&line).unwrap();
        let reply = serde_json::json!({"type":"pong", "id":ping["id"]});
        write
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
    });
    let paths = candidates(
        temp.path().join("elsewhere.sock"),
        |_| None,
        temp.path().to_path_buf(),
    );
    assert!(!is_live_daemon(&paths[0]).await);
    assert!(paths.contains(&socket));
    assert!(is_live_daemon(&socket).await);
    responder.await.unwrap();
}

#[cfg(unix)]
#[test]
fn candidates_dedupe_symlinked_parents_and_keep_explicit_override() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    let alias = temp.path().join("alias");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let custom = temp.path().join("custom.sock");
    let paths = candidates(
        real.join("jcode.sock"),
        |key| match key {
            "XDG_RUNTIME_DIR" => Some(alias.clone()),
            "JCODE_SOCKET" => Some(custom.clone()),
            _ => None,
        },
        temp.path().to_path_buf(),
    );
    assert!(!paths.contains(&alias.join("jcode.sock")));
    assert!(paths.contains(&custom));
}

#[cfg(target_os = "macos")]
#[test]
fn candidates_include_macos_tmpdir_default() {
    let paths = candidates(
        PathBuf::from("/current.sock"),
        |key| (key == "TMPDIR").then(|| PathBuf::from("/macos-temp")),
        PathBuf::from("/other-temp"),
    );
    assert!(paths.contains(&PathBuf::from("/macos-temp/jcode.sock")));
}
