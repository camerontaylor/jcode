//! Stored session catalog: ids, metadata and titles read from the sessions a
//! daemon persisted.
//!
//! Shared by the harness API bridge's `list_sessions` handler and the ACP
//! adapter's `session/list`, so both report the same sessions with the same
//! titles and timestamps. Everything here is best-effort: an unreadable or
//! missing record degrades to missing metadata rather than failing a listing.

use rusqlite::{Connection, params};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::time::{SystemTime, UNIX_EPOCH};

/// The small, canonical subset of a persisted `Session` needed by list and
/// attach responses. Serde skips the heavyweight transcript fields without
/// materializing them.
#[derive(Debug, Default, Deserialize)]
pub struct PersistedSessionMetadata {
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Generated or imported title.
    #[serde(default)]
    pub title: Option<String>,
    /// User-provided rename, which is what `Session::display_title` prefers.
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub todo_title: Option<String>,
    #[serde(default)]
    pub saved: bool,
    #[serde(default)]
    pub save_label: Option<String>,
    #[serde(skip)]
    pub updated_at_ms: Option<i64>,
    #[serde(skip)]
    pub last_active_at_ms: Option<i64>,
}

impl PersistedSessionMetadata {
    pub fn display_title(&self) -> Option<String> {
        self.custom_title
            .as_deref()
            .and_then(Self::normalized_title)
            // Bookmarks labelled before labels doubled as titles.
            .or_else(|| self.save_label())
            .or_else(|| self.todo_title.as_deref().and_then(Self::normalized_title))
            .or_else(|| self.title.as_deref().and_then(Self::normalized_title))
    }

    pub fn save_label(&self) -> Option<String> {
        self.saved
            .then(|| self.save_label.as_deref().and_then(Self::normalized_title))
            .flatten()
    }

    fn normalized_title(title: &str) -> Option<String> {
        let title = title.trim();
        (!title.is_empty()).then(|| title.to_string())
    }
}

/// One row of the shared recent-session index.
#[derive(Clone, Debug)]
pub struct RecentSessionIndexEntry {
    pub session_id: String,
    pub working_dir: Option<String>,
    pub generated_title: Option<String>,
    pub custom_title: Option<String>,
    pub todo_title: Option<String>,
    pub saved: bool,
    pub save_label: Option<String>,
    pub updated_at_ms: i64,
    pub last_active_at_ms: Option<i64>,
}

impl From<&RecentSessionIndexEntry> for PersistedSessionMetadata {
    fn from(entry: &RecentSessionIndexEntry) -> Self {
        Self {
            working_dir: entry.working_dir.clone(),
            title: entry.generated_title.clone(),
            custom_title: entry.custom_title.clone(),
            todo_title: entry.todo_title.clone(),
            saved: entry.saved,
            save_label: entry.save_label.clone(),
            updated_at_ms: Some(entry.updated_at_ms),
            last_active_at_ms: entry.last_active_at_ms,
        }
    }
}

/// Path of a session's persisted record, or `None` if the id is not a
/// plain session id.
///
/// One funnel for three reasons. It honours `JCODE_HOME`, without which a
/// launched instance reads the *user's* sessions: `peek_session` served
/// the real transcripts of the jcode the user runs interactively, which
/// defeats the isolation an embedded instance exists to provide. It
/// rejects ids that are not bare session ids, since the id comes straight
/// off the wire and is interpolated into a path, so `../../.ssh/id_rsa`
/// would otherwise be read and returned. And it keeps the three callers
/// from drifting apart, which is how the first two problems survived
/// being fixed anywhere else.
pub fn session_record_path(session_id: &str) -> Option<std::path::PathBuf> {
    // Session ids are `session_<name>_<millis>_<hex>`; anything with a
    // separator or a parent reference is not one.
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return None;
    }
    let home = match std::env::var_os("JCODE_HOME") {
        Some(home) => std::path::PathBuf::from(home),
        None => std::path::Path::new(&std::env::var_os("HOME")?).join(".jcode"),
    };
    Some(home.join("sessions").join(format!("{session_id}.json")))
}

/// Metadata of a session, read from its persisted record.
///
/// The legacy `history` event lists session *ids* only, but the strip
/// groups by directory, so the bridge resolves them from the same files
/// the daemon persists. Best-effort by design: an unreadable or missing
/// record simply leaves the session ungrouped rather than failing the
/// list. Session records can be hundreds of megabytes, while the fields we
/// need are written in the small header and trailer. Reading bounded windows
/// avoids parsing every transcript message just to populate the sidebar.
pub fn resolve_session_metadata(session_id: &str) -> Option<PersistedSessionMetadata> {
    let path = session_record_path(session_id)?;
    // A missing or malformed record is expected (a session may predate the
    // fields, or be mid-write), and the only cost is missing metadata, so
    // this degrades rather than failing the whole session list or attach.
    const WINDOW: usize = 64 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let head_len = usize::try_from(len.min(WINDOW as u64)).ok()?;
    let mut head = vec![0; head_len];
    file.read_exact(&mut head).ok()?;
    let tail = if len > WINDOW as u64 {
        file.seek(SeekFrom::End(-(WINDOW as i64))).ok()?;
        let mut tail = vec![0; WINDOW];
        file.read_exact(&mut tail).ok()?;
        tail
    } else {
        Vec::new()
    };
    Some(PersistedSessionMetadata {
        title: metadata_string(&head, "title", false),
        custom_title: metadata_string(&tail, "custom_title", true)
            .or_else(|| metadata_string(&head, "custom_title", true)),
        todo_title: None,
        saved: metadata_bool(&tail, "saved", true)
            .or_else(|| metadata_bool(&head, "saved", false))
            .unwrap_or(false),
        save_label: metadata_string(&tail, "save_label", true)
            .or_else(|| metadata_string(&head, "save_label", true)),
        updated_at_ms: None,
        last_active_at_ms: None,
        working_dir: metadata_string(&tail, "working_dir", true)
            .or_else(|| metadata_string(&head, "working_dir", true)),
    })
}

pub fn metadata_string(bytes: &[u8], field: &str, last: bool) -> Option<String> {
    let needle = format!("\"{field}\":");
    let mut starts = bytes
        .windows(needle.len())
        .enumerate()
        .filter_map(|(at, window)| (window == needle.as_bytes()).then_some(at + needle.len()));
    let start = if last {
        starts.next_back()?
    } else {
        starts.next()?
    };
    Option::<String>::deserialize(&mut serde_json::Deserializer::from_slice(&bytes[start..]))
        .ok()
        .flatten()
}

pub fn metadata_bool(bytes: &[u8], field: &str, last: bool) -> Option<bool> {
    let needle = format!("\"{field}\":");
    let mut starts = bytes
        .windows(needle.len())
        .enumerate()
        .filter_map(|(at, window)| (window == needle.as_bytes()).then_some(at + needle.len()));
    let start = if last {
        starts.next_back()?
    } else {
        starts.next()?
    };
    bool::deserialize(&mut serde_json::Deserializer::from_slice(&bytes[start..])).ok()
}

pub fn resolve_working_dir(session_id: &str) -> Option<String> {
    resolve_session_metadata(session_id)?.working_dir
}

pub fn recent_session_index_entries() -> Vec<RecentSessionIndexEntry> {
    let Some(path) = recent_session_index_path() else {
        return Vec::new();
    };
    let Ok(connection) = Connection::open(path) else {
        return Vec::new();
    };
    if connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS recent_sessions (
                 session_id TEXT PRIMARY KEY NOT NULL,
                 working_dir TEXT,
                 generated_title TEXT,
                 custom_title TEXT,
                 todo_title TEXT,
                 updated_at_ms INTEGER NOT NULL,
                 last_active_at_ms INTEGER,
                 saved INTEGER NOT NULL DEFAULT 0,
                 save_label TEXT
             );
             CREATE INDEX IF NOT EXISTS recent_sessions_activity
             ON recent_sessions(COALESCE(last_active_at_ms, updated_at_ms) DESC);",
        )
        .is_err()
    {
        return Vec::new();
    }
    let _ = connection.execute(
        "ALTER TABLE recent_sessions ADD COLUMN saved INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = connection.execute("ALTER TABLE recent_sessions ADD COLUMN save_label TEXT", []);
    let Ok(mut statement) = connection.prepare(
        "SELECT session_id, working_dir, generated_title, custom_title,
                todo_title, saved, updated_at_ms, last_active_at_ms, save_label
         FROM recent_sessions
         ORDER BY COALESCE(last_active_at_ms, updated_at_ms) DESC
         LIMIT 500",
    ) else {
        return Vec::new();
    };
    statement
        .query_map([], |row| {
            Ok(RecentSessionIndexEntry {
                session_id: row.get(0)?,
                working_dir: row.get(1)?,
                generated_title: row.get(2)?,
                custom_title: row.get(3)?,
                todo_title: row.get(4)?,
                saved: row.get(5)?,
                updated_at_ms: row.get(6)?,
                last_active_at_ms: row.get(7)?,
                save_label: row.get(8)?,
            })
        })
        .and_then(|rows| rows.collect())
        .unwrap_or_default()
}

pub fn saved_session_ids() -> Vec<String> {
    let Some(path) = recent_session_index_path() else {
        return Vec::new();
    };
    let Ok(connection) = Connection::open(path) else {
        return Vec::new();
    };
    let Ok(mut statement) =
        connection.prepare("SELECT session_id FROM recent_sessions WHERE saved != 0")
    else {
        return Vec::new();
    };
    statement
        .query_map([], |row| row.get(0))
        .and_then(|rows| rows.collect())
        .unwrap_or_default()
}

pub fn write_bootstrap_recent_session_index(ids: &[(SystemTime, String)]) {
    let Some(path) = recent_session_index_path() else {
        return;
    };
    let Ok(mut connection) = Connection::open(path) else {
        return;
    };
    let Ok(transaction) = connection.transaction() else {
        return;
    };
    for (modified, session_id) in ids.iter().take(500) {
        let metadata = resolve_session_metadata(session_id).unwrap_or_default();
        let updated_at_ms = modified
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or_default();
        let _ = transaction.execute(
            "INSERT INTO recent_sessions (
                 session_id, working_dir, generated_title, custom_title,
                 todo_title, updated_at_ms, last_active_at_ms
             ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?5)
             ON CONFLICT(session_id) DO NOTHING",
            params![
                session_id,
                metadata.working_dir,
                metadata.title,
                metadata.custom_title,
                updated_at_ms,
            ],
        );
    }
    let _ = transaction.commit();
}

pub fn stored_session_ids(limit: Option<usize>) -> Vec<String> {
    let indexed = recent_session_index_entries();
    if !indexed.is_empty() && limit.is_some_and(|limit| indexed.len() >= limit) {
        return indexed
            .into_iter()
            .take(limit.unwrap_or(usize::MAX))
            .map(|entry| entry.session_id)
            .collect();
    }
    let Some(home) = jcode_home() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(home.join("sessions")) else {
        return Vec::new();
    };
    let candidates: Vec<(String, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            let path = entry.path();
            if !kind.is_file() || path.extension()?.to_str()? != "json" {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_string();
            session_record_path(&id).is_some().then_some((id, path))
        })
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    // `stat` is the dominant cost with 100k+ sessions. Match the TUI picker
    // by doing those independent filesystem calls concurrently rather than
    // serially blocking the API reply long enough for clients to time out.
    // Stat is I/O-bound, so a few threads capture the parallelism; one per
    // core only multiplied allocator arenas in this long-lived process.
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(4)
        .min(candidates.len().max(1));
    let chunk_size = candidates.len().div_ceil(workers);
    let mut ids = std::thread::scope(|scope| {
        let handles = candidates.chunks(chunk_size).map(|chunk| {
            scope.spawn(move || {
                chunk
                    .iter()
                    .map(|(id, path)| {
                        let modified = path
                            .metadata()
                            .and_then(|metadata| metadata.modified())
                            .unwrap_or(UNIX_EPOCH);
                        (modified, id.clone())
                    })
                    .collect::<Vec<_>>()
            })
        });
        handles
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    ids.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
    write_bootstrap_recent_session_index(&ids);
    if let Some(limit) = limit {
        ids.truncate(limit);
    }
    ids.into_iter().map(|(_, id)| id).collect()
}

pub fn session_modified_ms(session_id: &str) -> Option<u64> {
    session_record_path(session_id)?
        .metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

/// Name sessions recorded before first-prompt titles existed. Each
/// session is derived once and cached in the shared index (an empty
/// value marks "no usable prompt"). The work is time-boxed per request,
/// newest first, so a large backlog is spread across list calls.
pub fn backfill_prompt_titles(
    ids: &BTreeSet<String>,
    indexed: &BTreeMap<String, RecentSessionIndexEntry>,
    metadata: &mut BTreeMap<String, PersistedSessionMetadata>,
) {
    const BUDGET: std::time::Duration = std::time::Duration::from_millis(300);
    let started = std::time::Instant::now();
    let mut pending: Vec<_> = ids
        .iter()
        .filter_map(|id| indexed.get(id))
        .filter(|entry| entry.generated_title.is_none())
        .collect();
    pending.sort_by_key(|entry| {
        std::cmp::Reverse(entry.last_active_at_ms.unwrap_or(entry.updated_at_ms))
    });
    for entry in pending {
        if started.elapsed() > BUDGET {
            break;
        }
        let id = &entry.session_id;
        let title = first_prompt_title(id);
        // Empty marks "no usable prompt" so the record is not rescanned.
        if let Some(path) = recent_session_index_path()
            && let Ok(connection) = Connection::open(path)
        {
            let _ = connection.execute(
                "UPDATE recent_sessions SET generated_title = ?2
                 WHERE session_id = ?1 AND generated_title IS NULL",
                params![id, title.clone().unwrap_or_default()],
            );
        }
        if let Some(title) = title
            && let Some(value) = metadata.get_mut(id)
        {
            value.title = Some(title);
        }
    }
}

/// Title derived from a session's first visible user prompt, matching the
/// runtime's own fallback. Streams the record so only that prompt's text
/// is materialized, then falls back to the append journal.
pub fn first_prompt_title(session_id: &str) -> Option<String> {
    use serde::de::{IgnoredAny, SeqAccess, Visitor};

    #[derive(Deserialize)]
    struct LiteBlock {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        text: Option<String>,
    }
    #[derive(Deserialize)]
    struct LiteMessage {
        role: String,
        #[serde(default)]
        display_role: Option<IgnoredAny>,
        #[serde(default)]
        content: Vec<LiteBlock>,
    }
    impl LiteMessage {
        fn title(&self) -> Option<String> {
            if self.role != "user" || self.display_role.is_some() {
                return None;
            }
            let mut texts = self
                .content
                .iter()
                .filter(|block| block.kind == "text")
                .filter_map(|block| block.text.as_deref());
            let first = texts.next()?;
            if first.trim_start().starts_with("<system-reminder>") {
                return None;
            }
            std::iter::once(first)
                .chain(texts)
                .find_map(jcode_session_types::prompt_title)
        }
    }
    #[derive(Default)]
    struct FirstPrompt(Option<String>);
    impl<'de> Deserialize<'de> for FirstPrompt {
        fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
            struct Scan;
            impl<'de> Visitor<'de> for Scan {
                type Value = FirstPrompt;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a message list")
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<FirstPrompt, A::Error> {
                    let mut found = None;
                    while found.is_none() {
                        match seq.next_element::<LiteMessage>()? {
                            Some(message) => found = message.title(),
                            None => return Ok(FirstPrompt(None)),
                        }
                    }
                    while seq.next_element::<IgnoredAny>()?.is_some() {}
                    Ok(FirstPrompt(found))
                }
            }
            de.deserialize_seq(Scan)
        }
    }
    #[derive(Deserialize)]
    struct Record {
        #[serde(default)]
        messages: FirstPrompt,
    }
    #[derive(Deserialize)]
    struct JournalEntry {
        #[serde(default)]
        append_messages: FirstPrompt,
    }

    let path = session_record_path(session_id)?;
    let file = std::fs::File::open(&path).ok()?;
    let mut de = serde_json::Deserializer::from_reader(std::io::BufReader::new(file));
    if let Some(title) = Record::deserialize(&mut de).ok().and_then(|r| r.messages.0) {
        return Some(title);
    }
    let journal = path.with_file_name(format!("{session_id}.journal.jsonl"));
    let reader = std::io::BufReader::new(std::fs::File::open(journal).ok()?);
    std::io::BufRead::lines(reader)
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<JournalEntry>(&line).ok())
        .find_map(|entry| entry.append_messages.0)
}

pub fn jcode_home() -> Option<std::path::PathBuf> {
    std::env::var_os("JCODE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".jcode")))
}

pub fn recent_session_index_path() -> Option<std::path::PathBuf> {
    Some(jcode_home()?.join("session-metadata-v1.sqlite3"))
}

/// One stored session as the catalog reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionCatalogEntry {
    pub session_id: String,
    pub working_dir: Option<String>,
    pub title: Option<String>,
    pub updated_at_ms: Option<i64>,
}

/// Metadata for `ids`, from the shared recent-session index plus bounded reads
/// of the records the index does not cover. Titles are backfilled from first
/// prompts within a small time budget.
pub fn load_session_metadata(ids: &BTreeSet<String>) -> BTreeMap<String, PersistedSessionMetadata> {
    let indexed: BTreeMap<_, _> = recent_session_index_entries()
        .into_iter()
        .map(|entry| (entry.session_id.clone(), entry))
        .collect();
    let mut metadata: BTreeMap<String, PersistedSessionMetadata> = ids
        .iter()
        .filter_map(|id| {
            indexed
                .get(id)
                .map(|entry| {
                    let mut metadata = PersistedSessionMetadata::from(entry);
                    // Rows indexed before labels were tracked have no label.
                    // Only saved sessions can carry one, so the record read
                    // stays rare.
                    if metadata.saved && metadata.save_label.is_none() {
                        metadata.save_label =
                            resolve_session_metadata(id).and_then(|record| record.save_label);
                    }
                    metadata
                })
                .or_else(|| resolve_session_metadata(id))
                .map(|metadata| (id.clone(), metadata))
        })
        .collect();
    backfill_prompt_titles(ids, &indexed, &mut metadata);
    metadata
}

/// Stored sessions, newest first: the ids the index and the sessions directory
/// know about plus the user's bookmarks, with their metadata attached.
pub fn stored_session_catalog(limit: Option<usize>) -> Vec<SessionCatalogEntry> {
    let mut ids: BTreeSet<String> = stored_session_ids(limit).into_iter().collect();
    if limit.is_some() {
        // Bookmarks are how users find old work, so a recency limit must never
        // hide them.
        ids.extend(saved_session_ids());
    }
    let metadata = load_session_metadata(&ids);
    let mut entries: Vec<SessionCatalogEntry> = ids
        .into_iter()
        .map(|session_id| {
            let metadata = metadata.get(&session_id);
            SessionCatalogEntry {
                updated_at_ms: metadata
                    .and_then(|metadata| metadata.updated_at_ms)
                    .or_else(|| session_modified_ms(&session_id).map(|value| value as i64)),
                working_dir: metadata.and_then(|metadata| metadata.working_dir.clone()),
                title: metadata.and_then(PersistedSessionMetadata::display_title),
                session_id,
            }
        })
        .collect();
    entries.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    entries
}
