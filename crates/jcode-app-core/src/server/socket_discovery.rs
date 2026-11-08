//! Bounded discovery for updates; never creates or starts a server.
use std::path::PathBuf;

pub fn daemon_socket_candidates() -> Vec<PathBuf> {
    candidates(
        super::socket_path(),
        |key| std::env::var_os(key).map(PathBuf::from),
        std::env::temp_dir(),
    )
}

fn candidates(
    current: PathBuf,
    env: impl Fn(&str) -> Option<PathBuf>,
    temp: PathBuf,
) -> Vec<PathBuf> {
    let mut paths = vec![current];
    for key in ["XDG_RUNTIME_DIR", "JCODE_RUNTIME_DIR"] {
        if let Some(dir) = env(key) {
            paths.push(dir.join("jcode.sock"));
        }
    }
    paths.push(crate::storage::fallback_runtime_dir_in(&temp).join("jcode.sock"));
    #[cfg(unix)]
    paths.push(
        crate::storage::fallback_runtime_dir_in(std::path::Path::new("/tmp")).join("jcode.sock"),
    );
    #[cfg(target_os = "macos")]
    if let Some(dir) = env("TMPDIR") {
        paths.push(dir.join("jcode.sock"));
    }
    if let Some(socket) = env("JCODE_SOCKET") {
        paths.push(socket);
    }
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(super::socket::resolved_socket_identity(path)));
    paths
}

/// Only sockets owned by this user and answering the jcode ping protocol.
pub async fn live_daemon_sockets() -> Vec<PathBuf> {
    let mut live = Vec::new();
    for path in daemon_socket_candidates() {
        if is_live_daemon(&path).await {
            live.push(path);
        }
    }
    live
}

pub(super) async fn is_live_daemon(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
            return false;
        }
    }
    super::is_server_ready(path).await
}

#[cfg(test)]
#[path = "socket_discovery_tests.rs"]
mod tests;
