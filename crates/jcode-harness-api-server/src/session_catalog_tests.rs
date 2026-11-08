//! Facade tests: the catalog the ACP adapter lists must order, page and name
//! stored sessions the way the harness API bridge already does.

use super::*;
use crate::translate::jcode_home_test_lock;
use rusqlite::Connection;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// An isolated `JCODE_HOME`, serialized against the other tests that move it.
struct ScopedJcodeHome {
    path: PathBuf,
    previous: Option<std::ffi::OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl ScopedJcodeHome {
    fn new(label: &str) -> Self {
        let guard = jcode_home_test_lock();
        let previous = std::env::var_os("JCODE_HOME");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "jcode-session-catalog-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create isolated JCODE_HOME");
        // SAFETY: every test that mutates JCODE_HOME holds the shared lock for
        // the whole scope and restores the prior value before releasing it.
        unsafe { std::env::set_var("JCODE_HOME", &path) };
        Self {
            path,
            previous,
            _guard: guard,
        }
    }
}

impl Drop for ScopedJcodeHome {
    fn drop(&mut self) {
        // SAFETY: see `new`.
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var("JCODE_HOME", previous),
                None => std::env::remove_var("JCODE_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Start the shared recent-session index the bridge maintains empty, so
/// ordering and titles are deterministic instead of depending on filesystem
/// timestamps.
fn empty_index() {
    // Creates the table with the schema the bridge expects.
    assert!(BridgeState::recent_session_index_entries().is_empty());
}

fn write_index_row(
    home: &Path,
    session_id: &str,
    working_dir: Option<&str>,
    generated_title: Option<&str>,
    updated_at_ms: i64,
    saved: bool,
) {
    let connection =
        Connection::open(home.join("session-metadata-v1.sqlite3")).expect("open index");
    connection
        .execute(
            "INSERT INTO recent_sessions (
                 session_id, working_dir, generated_title, custom_title,
                 todo_title, updated_at_ms, last_active_at_ms, saved, save_label
             ) VALUES (?1, ?2, ?3, NULL, NULL, ?4, NULL, ?5, NULL)",
            rusqlite::params![
                session_id,
                working_dir,
                generated_title,
                updated_at_ms,
                saved
            ],
        )
        .expect("insert index row");
}

fn set_modified(path: &Path, when: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(when))
        .expect("set record mtime");
}

fn ids(entries: &[SessionCatalogEntry]) -> Vec<&str> {
    entries
        .iter()
        .map(|entry| entry.session_id.as_str())
        .collect()
}

#[test]
fn catalog_reports_newest_first_with_indexed_titles() {
    let home = ScopedJcodeHome::new("ordering");
    empty_index();
    write_index_row(
        &home.path,
        "session_old",
        Some("/work/old"),
        Some("Old work"),
        1_000,
        false,
    );
    write_index_row(
        &home.path,
        "session_mid",
        Some("/work/mid"),
        Some("Mid work"),
        2_000,
        false,
    );
    write_index_row(
        &home.path,
        "session_new",
        Some("/work/new"),
        Some("New work"),
        3_000,
        false,
    );

    let entries = stored_session_catalog(Some(3));
    assert_eq!(ids(&entries), ["session_new", "session_mid", "session_old"]);
    assert_eq!(entries[0].title.as_deref(), Some("New work"));
    assert_eq!(entries[0].working_dir.as_deref(), Some("/work/new"));
    assert_eq!(entries[0].updated_at_ms, Some(3_000));

    // The limit is a recency bound, and the result is still newest first.
    let page = stored_session_catalog(Some(2));
    assert_eq!(ids(&page), ["session_new", "session_mid"]);
}

#[test]
fn catalog_keeps_bookmarks_beyond_the_recency_limit() {
    let home = ScopedJcodeHome::new("bookmarks");
    empty_index();
    write_index_row(
        &home.path,
        "session_new",
        Some("/work/new"),
        Some("New"),
        3_000,
        false,
    );
    write_index_row(
        &home.path,
        "session_saved",
        Some("/work/saved"),
        Some("Bookmarked"),
        1_000,
        true,
    );

    let page = stored_session_catalog(Some(1));
    assert_eq!(
        ids(&page),
        ["session_new", "session_saved"],
        "a one-entry page must not hide the user's bookmarks"
    );
    assert_eq!(page[1].title.as_deref(), Some("Bookmarked"));
}

#[test]
fn catalog_lists_stored_records_newest_first_and_tolerates_missing_titles() {
    let home = ScopedJcodeHome::new("records");
    let work = home.path.join("work");
    std::fs::create_dir_all(&work).expect("create working directory");
    let sessions = home.path.join("sessions");
    std::fs::create_dir_all(&sessions).expect("create sessions directory");
    let older = sessions.join("session_older.json");
    let newer = sessions.join("session_newer.json");
    for (session_id, title) in [
        ("session_older", Some("Generated title")),
        ("session_newer", None),
    ] {
        std::fs::write(
            sessions.join(format!("{session_id}.json")),
            json!({
                "working_dir": work,
                "title": title,
                "messages": [],
            })
            .to_string(),
        )
        .expect("write session record");
    }
    let now = SystemTime::now();
    set_modified(&older, now - Duration::from_secs(3_600));
    set_modified(&newer, now - Duration::from_secs(60));

    // No limit: the adapter's `session/list` call, which discovers records on
    // disk rather than trusting the index.
    let entries = stored_session_catalog(None);
    assert_eq!(ids(&entries), ["session_newer", "session_older"]);
    assert_eq!(
        entries[0].title, None,
        "a session with no prompt and no title has no title to report"
    );
    assert_eq!(entries[1].title.as_deref(), Some("Generated title"));
    assert_eq!(
        entries[0].working_dir.as_deref(),
        work.to_str(),
        "the stored directory is reported verbatim; the adapter canonicalises"
    );
    assert!(entries[0].updated_at_ms.is_some());
}
