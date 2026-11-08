//! Stored session catalog facade.
//!
//! `session/list` needs the stored sessions a client can attach to, with the
//! same titles, timestamps and bookmark handling the harness API bridge's
//! `list_sessions` reports. Rather than carrying its own copy of the bridge's
//! index and record readers, the adapter asks for that view here; the readers
//! themselves stay in [`crate::translate`], where upstream keeps them.
//!
//! Everything is best-effort: an unreadable or missing record degrades to
//! missing metadata rather than failing a listing.

use crate::translate::BridgeState;
use crate::translate::PersistedSessionMetadata;
use std::collections::{BTreeMap, BTreeSet};

/// One stored session as the catalog reports it.
///
/// Deliberately presentation-free: the ACP adapter decides how a directory,
/// title or timestamp reaches the wire, so this type stays a plain projection
/// of what the bridge already knows.
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
fn load_session_metadata(ids: &BTreeSet<String>) -> BTreeMap<String, PersistedSessionMetadata> {
    let indexed: BTreeMap<_, _> = BridgeState::recent_session_index_entries()
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
                        metadata.save_label = BridgeState::resolve_session_metadata(id)
                            .and_then(|record| record.save_label);
                    }
                    metadata
                })
                .or_else(|| BridgeState::resolve_session_metadata(id))
                .map(|metadata| (id.clone(), metadata))
        })
        .collect();
    BridgeState::backfill_prompt_titles(ids, &indexed, &mut metadata);
    metadata
}

/// Stored sessions, newest first: the ids the index and the sessions directory
/// know about plus the user's bookmarks, with their metadata attached.
///
/// `limit` bounds the recency scan, never the bookmark set: a picker that asks
/// for a short page must still be able to reach old work.
pub fn stored_session_catalog(limit: Option<usize>) -> Vec<SessionCatalogEntry> {
    let mut ids: BTreeSet<String> = BridgeState::stored_session_ids(limit).into_iter().collect();
    if limit.is_some() {
        // Bookmarks are how users find old work, so a recency limit must never
        // hide them.
        ids.extend(BridgeState::saved_session_ids());
    }
    let metadata = load_session_metadata(&ids);
    let mut entries: Vec<SessionCatalogEntry> = ids
        .into_iter()
        .map(|session_id| {
            let metadata = metadata.get(&session_id);
            SessionCatalogEntry {
                updated_at_ms: metadata
                    .and_then(|metadata| metadata.updated_at_ms)
                    .or_else(|| {
                        BridgeState::session_modified_ms(&session_id).map(|value| value as i64)
                    }),
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

#[cfg(test)]
#[path = "session_catalog_tests.rs"]
mod tests;
