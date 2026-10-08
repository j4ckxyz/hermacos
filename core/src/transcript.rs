//! Stored history rows -> display messages.
//!
//! The store keeps one row per model message (`user`, `assistant`, `tool`). A reader thinks in
//! turns: what they asked, then everything the agent did and said until the next question. This
//! folds assistant and tool rows between two user rows into one assistant message whose parts
//! keep the original order (text, tool call, more text, ...).

use serde_json::Value;

use crate::types::{
    AttachmentKind, ChatMessage, MessageAttachment, MessagePart, Role, ToolCall, ToolStatus,
};
use crate::util::{f64_of, humanize, one_line, str_of};

/// Argument keys that best describe what a tool call is doing, in priority order.
const DETAIL_KEYS: [&str; 12] = [
    "command", "query", "url", "path", "file_path", "pattern", "prompt", "goal", "question", "code",
    "name", "action",
];

/// One-line context for a tool call, from its arguments.
pub(crate) fn tool_detail(args: &Value) -> Option<String> {
    let args = match args {
        Value::String(raw) => serde_json::from_str::<Value>(raw).unwrap_or(Value::Null),
        other => other.clone(),
    };
    let object = args.as_object()?;
    DETAIL_KEYS
        .iter()
        .filter_map(|key| object.get(*key).and_then(Value::as_str))
        .chain(object.values().filter_map(Value::as_str))
        .map(|s| one_line(s, 140))
        .find(|s| !s.is_empty())
}

/// Text of a `content` field that is either a string or a list of typed parts.
pub(crate) fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(s) => Some(s.clone()),
                Value::Object(_) => {
                    let kind = str_of(part, "type");
                    match kind.as_str() {
                        "image_url" | "input_image" | "image" => Some("[image]".to_owned()),
                        _ => part.get("text").and_then(Value::as_str).map(str::to_owned),
                    }
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn row_text(row: &Value) -> String {
    for key in ["display_content", "content", "text"] {
        if let Some(value) = row.get(key).filter(|v| !v.is_null()) {
            let text = content_text(value);
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    String::new()
}

pub(crate) fn durable_row_id(row: &Value) -> Option<i64> {
    row.get("row_id").or_else(|| row.get("id")).and_then(Value::as_i64)
}

fn base_name(path: &str) -> String {
    let trimmed = path.trim_matches(|c| c == '`' || c == '"' || c == '\'');
    trimmed.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or(trimmed).to_owned()
}

/// What the user typed, separated from the attachment scaffolding stored around it.
///
/// A stored user message carries media three ways: image parts in a content list, `[image]`
/// stand-ins when the transcript was read without inline images, and reference lines such as
/// `@file:notes.md` that the gateway resolves when the prompt runs.
fn user_content(row: &Value) -> (String, Vec<MessageAttachment>) {
    let image = |name: String, server_path: Option<String>| MessageAttachment {
        name,
        kind: AttachmentKind::Image,
        ref_text: None,
        server_path,
    };
    let mut attachments: Vec<MessageAttachment> = Vec::new();
    let mut named_images: Vec<String> = Vec::new();
    let mut text = String::new();

    let content = ["display_content", "content", "text"]
        .iter()
        .filter_map(|key| row.get(*key))
        .find(|v| !v.is_null() && v.as_str() != Some(""))
        .cloned()
        .unwrap_or(Value::Null);
    match &content {
        Value::String(s) => text.push_str(s),
        Value::Array(parts) => {
            for part in parts {
                match part {
                    Value::String(s) => {
                        text.push_str(s);
                        text.push('\n');
                    }
                    Value::Object(_) => match str_of(part, "type").as_str() {
                        "image_url" | "input_image" | "image" => {
                            attachments.push(image("Image".into(), None));
                        }
                        _ => {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                text.push_str(t);
                                text.push('\n');
                            }
                        }
                    },
                    _ => {}
                }
            }
        }
        _ => {}
    }

    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[image]" {
            attachments.push(image("Image".into(), None));
        } else if let Some(name) = trimmed.strip_prefix("[User attached image: ").and_then(|r| r.strip_suffix(']')) {
            named_images.push(name.to_owned());
        } else if let Some(path) = trimmed.strip_prefix("[Image attached at: ").and_then(|r| r.strip_suffix(']')) {
            attachments.push(image(base_name(path), Some(path.to_owned())));
        } else if let Some(path) = trimmed.strip_prefix("@image:") {
            let path = path.trim_matches(|c| c == '`' || c == '"' || c == '\'');
            attachments.push(image(base_name(path), Some(path.to_owned())));
        } else if trimmed.starts_with("@file:") && !trimmed.contains(char::is_whitespace) {
            attachments.push(MessageAttachment {
                name: base_name(&trimmed["@file:".len()..]),
                kind: AttachmentKind::File,
                ref_text: Some(trimmed.to_owned()),
                server_path: None,
            });
        } else {
            kept.push(line);
        }
    }
    // `[User attached image: x.png]` names an image that is usually also present as a part.
    let mut unnamed = attachments.iter_mut().filter(|a| a.kind == AttachmentKind::Image && a.name == "Image");
    let mut extra: Vec<MessageAttachment> = Vec::new();
    for name in named_images {
        match unnamed.next() {
            Some(slot) => slot.name = name,
            None => extra.push(image(name, None)),
        }
    }
    drop(unnamed);
    attachments.extend(extra);
    // Images first, whatever order the store interleaved them in.
    attachments.sort_by_key(|a| a.kind != AttachmentKind::Image);
    (kept.join("\n").trim().to_owned(), attachments)
}

fn row_id(row: &Value, index: usize) -> String {
    row.get("row_id")
        .or_else(|| row.get("id"))
        .and_then(Value::as_i64)
        .map(|n| format!("m{n}"))
        .unwrap_or_else(|| format!("i{index}"))
}

/// The history endpoint may answer newest-first; put rows back in reading order.
pub(crate) fn sort_chronological(rows: &mut [Value]) {
    let key = |row: &Value| {
        row.get("row_id").or_else(|| row.get("id")).and_then(Value::as_i64)
    };
    if rows.iter().all(|r| key(r).is_some()) {
        rows.sort_by_key(|r| key(r).unwrap_or(0));
    } else if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
        if f64_of(first, "timestamp") > f64_of(last, "timestamp") {
            rows.reverse();
        }
    }
}

fn tool_failed(text: &str) -> bool {
    let Ok(Value::Object(result)) = serde_json::from_str::<Value>(text) else { return false };
    let errored = result.get("error").is_some_and(|e| match e {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        _ => true,
    });
    errored || result.get("success") == Some(&Value::Bool(false))
}

pub(crate) fn messages_from_rows(rows: &[Value]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let role = str_of(row, "role");
        let display_kind = str_of(row, "display_kind");
        if display_kind == "hidden" || role == "system" {
            continue;
        }
        let timestamp = f64_of(row, "timestamp");
        match role.as_str() {
            "user" => {
                let (text, attachments) = user_content(row);
                if text.is_empty() && attachments.is_empty() {
                    continue;
                }
                out.push(ChatMessage {
                    id: row_id(row, index),
                    role: Role::User,
                    parts: if text.is_empty() { Vec::new() } else { vec![MessagePart::Text { text }] },
                    timestamp,
                    row_id: durable_row_id(row),
                    attachments,
                });
            }
            "assistant" => {
                let message = assistant_tail(&mut out, row, index, timestamp);
                let reasoning = ["display_reasoning", "reasoning", "reasoning_content"]
                    .iter()
                    .filter_map(|k| row.get(*k).and_then(Value::as_str))
                    .find(|s| !s.trim().is_empty());
                if let Some(reasoning) = reasoning {
                    message.parts.push(MessagePart::Reasoning { text: reasoning.to_owned() });
                }
                let text = row_text(row);
                if !text.trim().is_empty() {
                    message.parts.push(MessagePart::Text { text });
                }
                for call in row.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                    let function = call.get("function").unwrap_or(call);
                    let name = str_of(function, "name");
                    if name.is_empty() {
                        continue;
                    }
                    message.parts.push(MessagePart::Tool {
                        call: ToolCall {
                            id: str_of(call, "id"),
                            title: humanize(&name),
                            detail: function.get("arguments").and_then(tool_detail),
                            name,
                            summary: None,
                            status: ToolStatus::Done,
                            duration: None,
                        },
                    });
                }
            }
            "tool" => {
                let message = assistant_tail(&mut out, row, index, timestamp);
                let call_id = str_of(row, "tool_call_id");
                let text = row_text(row);
                let failed = tool_failed(&text);
                let existing = message.parts.iter_mut().rev().find_map(|part| match part {
                    MessagePart::Tool { call } if !call_id.is_empty() && call.id == call_id => Some(call),
                    _ => None,
                });
                if let Some(call) = existing {
                    if failed {
                        call.status = ToolStatus::Failed;
                    }
                } else {
                    let name = ["tool_name", "name"]
                        .iter()
                        .map(|k| str_of(row, k))
                        .find(|s| !s.is_empty())
                        .unwrap_or_else(|| "tool".into());
                    message.parts.push(MessagePart::Tool {
                        call: ToolCall {
                            id: call_id,
                            title: humanize(&name),
                            detail: row
                                .get("args")
                                .and_then(tool_detail)
                                .or_else(|| row.get("context").and_then(Value::as_str).map(|s| one_line(s, 140))),
                            name,
                            summary: None,
                            status: if failed { ToolStatus::Failed } else { ToolStatus::Done },
                            duration: None,
                        },
                    });
                }
            }
            _ => {}
        }
    }
    out.retain(|m| !m.parts.is_empty());
    out
}

/// The assistant message currently being assembled, starting one if the last row was the user's.
fn assistant_tail<'a>(
    out: &'a mut Vec<ChatMessage>,
    row: &Value,
    index: usize,
    timestamp: f64,
) -> &'a mut ChatMessage {
    if out.last().is_none_or(|m| m.role != Role::Assistant) {
        out.push(ChatMessage {
            id: row_id(row, index),
            role: Role::Assistant,
            parts: Vec::new(),
            timestamp,
            row_id: None,
            attachments: Vec::new(),
        });
    }
    let message = out.last_mut().expect("assistant message was just ensured");
    // A turn is stamped with the time of its latest row: when the reply finished.
    message.timestamp = message.timestamp.max(timestamp);
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn folds_tool_rows_into_one_assistant_turn() {
        let rows = vec![
            json!({"id": 1, "role": "system", "content": "you are"}),
            json!({"id": 2, "role": "user", "content": "weather?"}),
            json!({"id": 3, "role": "assistant", "content": "Checking.", "tool_calls": [
                {"id": "c1", "function": {"name": "web_search", "arguments": "{\"query\":\"weather london\"}"}}
            ]}),
            json!({"id": 4, "role": "tool", "tool_call_id": "c1", "content": "{\"error\": \"timeout\"}"}),
            json!({"id": 5, "role": "assistant", "content": [{"type": "text", "text": "It's sunny."}]}),
            json!({"id": 6, "role": "user", "content": "thanks", "display_kind": "hidden"}),
        ];
        let messages = messages_from_rows(&rows);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        let parts = &messages[1].parts;
        assert_eq!(parts.len(), 3);
        match &parts[1] {
            MessagePart::Tool { call } => {
                assert_eq!(call.title, "Web search");
                assert_eq!(call.detail.as_deref(), Some("weather london"));
                assert_eq!(call.status, ToolStatus::Failed);
            }
            other => panic!("expected tool part, got {other:?}"),
        }
        assert_eq!(parts[2], MessagePart::Text { text: "It's sunny.".into() });
    }

    #[test]
    fn user_messages_separate_text_from_attachments() {
        let rows = vec![
            json!({"id": 7, "role": "user", "timestamp": 100.0, "content": [
                {"type": "text", "text": "@file:.attachments/notes.md\n\nWhat is in these?\n[User attached image: shot.png]"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
            ]}),
            json!({"id": 8, "role": "assistant", "content": "A screenshot.", "timestamp": 130.0}),
            json!({"id": 9, "role": "user", "content": "look\n[image]\n[Image attached at: /srv/up/cat.jpg]"}),
        ];
        let messages = messages_from_rows(&rows);
        assert_eq!(messages[0].row_id, Some(7));
        assert_eq!(messages[0].parts, [MessagePart::Text { text: "What is in these?".into() }]);
        let names: Vec<(&str, AttachmentKind)> =
            messages[0].attachments.iter().map(|a| (a.name.as_str(), a.kind)).collect();
        assert_eq!(names, [("shot.png", AttachmentKind::Image), ("notes.md", AttachmentKind::File)]);
        assert_eq!(messages[0].attachments[1].ref_text.as_deref(), Some("@file:.attachments/notes.md"));
        assert_eq!(messages[1].timestamp, 130.0);
        assert_eq!(messages[2].attachments.len(), 2);
        assert_eq!(messages[2].attachments[1].server_path.as_deref(), Some("/srv/up/cat.jpg"));
    }

    #[test]
    fn newest_first_pages_are_reordered() {
        let mut rows = vec![json!({"id": 9, "role": "user"}), json!({"id": 3, "role": "user"})];
        sort_chronological(&mut rows);
        assert_eq!(rows[0]["id"], 3);
    }
}
