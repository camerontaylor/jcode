//! Image-aware ACP history replay. Ownership must be explicit: never guess an
//! image's owner from its position or render it as assistant text.

use crate::protocol::HistoryMessage;
use crate::session::{RenderedImage, RenderedImageAnchor, RenderedImageSource};
use serde_json::{Value, json};

/// Replay history with image content attached to the same stable message/tool
/// identities used by the text mapper. Returns `session/update` payloads, not
/// JSON-RPC envelopes.
///
/// Unanchored images, inconsistent source/anchor pairs and missing or ambiguous
/// owners are omitted rather than attaching private image content to the wrong
/// prompt. A history boundary orders an image but does not identify its owner.
pub(super) fn history_replay_updates_with_images(
    session_id: &str,
    messages: &[HistoryMessage],
    images: &[RenderedImage],
    session_notices: bool,
) -> Vec<Value> {
    let mut updates = super::presentation::history_replay_updates_with_notices(
        session_id,
        messages,
        session_notices,
    );
    let mut stored_parts = std::collections::HashMap::<usize, usize>::new();
    let user_ids: Vec<_> = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            if !matches!(message.role.as_str(), "user" | "assistant") {
                return None;
            }
            let id = match message.stored_index {
                Some(stored) => {
                    let part = stored_parts.entry(stored).or_default();
                    let id = if *part == 0 {
                        format!("{session_id}:msg:{stored}")
                    } else {
                        format!("{session_id}:msg:{stored}:part:{part}")
                    };
                    *part += 1;
                    id
                }
                None => format!("{session_id}:history:{index}"),
            };
            (message.role == "user"
                && !crate::session::is_attached_image_label_text(&message.content))
            .then_some(id)
        })
        .collect();
    let mut user_images: Vec<Vec<Value>> = vec![Vec::new(); updates.len()];

    for image in images {
        let content = json!({
            "type": "image",
            "mimeType": image.media_type,
            "data": image.data,
        });
        match (&image.source, &image.anchor) {
            (RenderedImageSource::UserInput, Some(RenderedImageAnchor::UserPrompt { ordinal })) => {
                let Some(id) = user_ids.get(*ordinal) else {
                    continue;
                };
                let mut owners = updates.iter().enumerate().filter(|(_, update)| {
                    update["update"]["sessionUpdate"] == "user_message_chunk"
                        && update["update"]["messageId"].as_str() == Some(id.as_str())
                });
                let Some((index, _)) = owners.next() else {
                    continue;
                };
                if owners.next().is_some() {
                    continue;
                }
                user_images[index].push(json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "user_message_chunk",
                        "messageId": id,
                        "content": content,
                    }
                }));
            }
            (
                RenderedImageSource::ToolResult { .. },
                Some(RenderedImageAnchor::ToolCall { id }),
            ) if !id.is_empty() => {
                let mut owners = updates.iter().enumerate().filter(|(_, update)| {
                    update["update"]["sessionUpdate"] == "tool_call"
                        && update["update"]["toolCallId"].as_str() == Some(id.as_str())
                });
                let Some((index, _)) = owners.next() else {
                    continue;
                };
                if owners.next().is_some() {
                    continue;
                }
                // Enrich the original tool_call payload instead of sending a
                // tool_call_update whose content could replace its text result.
                if let Some(contents) = updates[index]["update"]["content"].as_array_mut() {
                    contents.push(json!({ "type": "content", "content": content }));
                }
            }
            _ => {}
        }
    }

    let mut replay = Vec::with_capacity(updates.len() + images.len());
    for (index, update) in updates.into_iter().enumerate() {
        replay.push(update);
        replay.append(&mut user_images[index]);
    }
    replay
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, stored_index: Option<usize>) -> HistoryMessage {
        serde_json::from_value(json!({
            "role": role, "content": "history text", "stored_index": stored_index,
        }))
        .unwrap()
    }

    fn tool(id: &str) -> HistoryMessage {
        let mut message = message("tool", Some(17));
        message.tool_data = Some(
            serde_json::from_value(json!({
                "id": id, "name": "read", "input": {},
            }))
            .unwrap(),
        );
        message
    }

    fn image(source: RenderedImageSource, anchor: Option<RenderedImageAnchor>) -> RenderedImage {
        RenderedImage {
            media_type: "image/png".into(),
            data: "aW1hZ2U=".into(),
            label: Some("not assistant prose".into()),
            source,
            anchor,
            history_message_index: None,
        }
    }

    fn user_image(ordinal: usize) -> RenderedImage {
        image(
            RenderedImageSource::UserInput,
            Some(RenderedImageAnchor::UserPrompt { ordinal }),
        )
    }

    fn tool_image(id: &str) -> RenderedImage {
        image(
            RenderedImageSource::ToolResult {
                tool_name: "read".into(),
            },
            Some(RenderedImageAnchor::ToolCall { id: id.into() }),
        )
    }

    #[test]
    fn user_images_share_stable_prompt_id_and_are_inline() {
        let messages = vec![
            message("system", None),
            message("user", Some(12)),
            message("assistant", Some(13)),
        ];
        let updates = history_replay_updates_with_images("s", &messages, &[user_image(0)], false);
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[1]["update"]["sessionUpdate"], "user_message_chunk");
        assert_eq!(updates[1]["update"]["messageId"], "s:msg:12");
        assert_eq!(
            updates[1]["update"]["content"],
            json!({
                "type": "image", "mimeType": "image/png", "data": "aW1hZ2U=",
            })
        );
        assert_eq!(updates[2]["update"]["messageId"], "s:msg:13");
    }

    #[test]
    fn synthetic_prompt_retains_mapper_fallback_identity() {
        let updates = history_replay_updates_with_images(
            "s",
            &[message("user", None)],
            &[user_image(0)],
            false,
        );
        assert_eq!(updates[1]["update"]["messageId"], "s:history:0");
    }

    #[test]
    fn tool_images_preserve_call_id_and_text_result_in_one_payload() {
        let messages = vec![tool("call-original")];
        let updates = history_replay_updates_with_images(
            "s",
            &messages,
            &[tool_image("call-original"), tool_image("call-original")],
            false,
        );
        assert_eq!(updates.len(), 1);
        let update = &updates[0]["update"];
        assert_eq!(update["sessionUpdate"], "tool_call");
        assert_eq!(update["toolCallId"], "call-original");
        assert_eq!(update["content"].as_array().unwrap().len(), 3);
        assert_eq!(update["content"][0]["content"]["text"], "history text");
        assert_eq!(update["content"][1]["content"]["type"], "image");
        assert_eq!(update["rawOutput"]["output"], "history text");
    }

    #[test]
    fn preserves_user_image_order_and_ordinal_across_nonuser_rows() {
        let messages = vec![
            message("user", Some(1)),
            tool("call"),
            message("assistant", Some(3)),
            message("user", Some(4)),
        ];
        let mut first = user_image(1);
        first.data = "first".into();
        let mut second = user_image(1);
        second.data = "second".into();
        let updates = history_replay_updates_with_images("s", &messages, &[first, second], false);
        assert_eq!(updates[4]["update"]["messageId"], "s:msg:4");
        assert_eq!(updates[4]["update"]["content"]["data"], "first");
        assert_eq!(updates[5]["update"]["content"]["data"], "second");
    }

    #[test]
    fn skips_missing_ambiguous_and_unanchored_owners() {
        let messages = vec![
            message("user", Some(1)),
            tool("duplicate"),
            tool("duplicate"),
        ];
        let mut unanchored = image(RenderedImageSource::UserInput, None);
        unanchored.history_message_index = Some(0);
        let images = vec![
            unanchored,
            user_image(99),
            tool_image("missing"),
            tool_image("duplicate"),
            tool_image(""),
        ];
        assert_eq!(
            history_replay_updates_with_images("s", &messages, &images, false),
            super::super::presentation::history_replay_updates("s", &messages)
        );
    }

    #[test]
    fn never_replays_assistant_or_inconsistent_source_images() {
        let messages = vec![message("user", Some(1)), tool("call")];
        let images = vec![
            image(
                RenderedImageSource::Other {
                    role: "assistant".into(),
                },
                Some(RenderedImageAnchor::UserPrompt { ordinal: 0 }),
            ),
            image(
                RenderedImageSource::UserInput,
                Some(RenderedImageAnchor::ToolCall { id: "call".into() }),
            ),
            image(
                RenderedImageSource::ToolResult {
                    tool_name: "read".into(),
                },
                Some(RenderedImageAnchor::UserPrompt { ordinal: 0 }),
            ),
        ];
        assert_eq!(
            history_replay_updates_with_images("s", &messages, &images, false),
            super::super::presentation::history_replay_updates("s", &messages)
        );
    }

    #[test]
    fn synthetic_tool_image_labels_do_not_shift_prompt_ordinals() {
        let mut label = message("user", None);
        label.content =
            "[Attached image associated with the preceding tool result: screenshot]".into();
        assert!(crate::session::is_attached_image_label_text(&label.content));
        let messages = vec![label, message("user", Some(9))];
        let updates = history_replay_updates_with_images("s", &messages, &[user_image(0)], false);
        assert_eq!(updates[2]["update"]["messageId"], "s:msg:9");
        assert_eq!(updates[2]["update"]["content"]["type"], "image");
    }

    #[test]
    fn system_and_reasoning_updates_do_not_shift_image_ownership() {
        let mut assistant = message("assistant", Some(2));
        assistant.content = format!(
            "{}\nanswer",
            jcode_render_core::reasoning_line_markup("private thought")
        );
        let messages = vec![
            message("system", None),
            assistant,
            tool("read-image"),
            message("user", Some(4)),
        ];
        for notices in [false, true] {
            let updates = history_replay_updates_with_images(
                "s",
                &messages,
                &[tool_image("read-image"), user_image(0)],
                notices,
            );
            let tool = updates
                .iter()
                .find(|update| update["update"]["toolCallId"] == "read-image")
                .unwrap();
            assert_eq!(tool["update"]["content"][1]["content"]["type"], "image");
            let user_index = updates
                .iter()
                .position(|update| update["update"]["messageId"] == "s:msg:4")
                .unwrap();
            assert_eq!(updates[user_index + 1]["update"]["messageId"], "s:msg:4");
            assert_eq!(
                updates[user_index + 1]["update"]["content"]["type"],
                "image"
            );
            assert_eq!(
                updates
                    .iter()
                    .filter(|update| { update["update"]["sessionUpdate"] == "notice" })
                    .count(),
                usize::from(notices)
            );
            assert!(
                updates
                    .iter()
                    .any(|update| { update["update"]["sessionUpdate"] == "agent_thought_chunk" })
            );
        }
    }

    #[test]
    fn split_stored_rows_preserve_user_part_identity() {
        let messages = vec![message("user", Some(3)), message("user", Some(3))];
        let updates = history_replay_updates_with_images("s", &messages, &[user_image(1)], false);
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[2]["update"]["messageId"], "s:msg:3:part:1");
    }

    #[test]
    fn no_images_preserves_existing_replay_exactly() {
        let messages = vec![
            message("user", None),
            tool("call"),
            message("assistant", Some(3)),
        ];
        assert_eq!(
            history_replay_updates_with_images("s", &messages, &[], false),
            super::super::presentation::history_replay_updates("s", &messages)
        );
    }
}
