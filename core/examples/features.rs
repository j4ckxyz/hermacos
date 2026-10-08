//! Exercises attachments, message rewriting and usage against the mock dashboard.
//!
//!   cargo run -p hermes-mock -- --port 9119 &
//!   cargo run -p hermes-core --example features -- http://127.0.0.1:9119
//!
//! Exits non-zero when any step doesn't behave as the app relies on.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hermes_core::{
    AttachmentKind, AuthTokens, ChatEvent, ConnectionState, HermesClient, HermesListener, MessagePart, Role,
    TurnStatus, login_password, probe_server,
};
use tokio::sync::mpsc;

struct Turns {
    done: mpsc::UnboundedSender<(TurnStatus, String)>,
    text: Mutex<String>,
}

impl HermesListener for Turns {
    fn on_connection(&self, _state: ConnectionState) {}

    fn on_event(&self, event: ChatEvent) {
        match event {
            ChatEvent::TextDelta { text, .. } => self.text.lock().unwrap().push_str(&text),
            ChatEvent::TurnCompleted { status, .. } => {
                let text = std::mem::take(&mut *self.text.lock().unwrap());
                let _ = self.done.send((status, text));
            }
            _ => {}
        }
    }

    fn on_tokens(&self, _tokens: AuthTokens) {}
}

/// A 1x1 transparent PNG.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

async fn next_turn(done: &mut mpsc::UnboundedReceiver<(TurnStatus, String)>) -> (TurnStatus, String) {
    tokio::time::timeout(Duration::from_secs(60), done.recv()).await.ok().flatten().expect("turn finished")
}

fn check(condition: bool, what: &str) {
    println!("{} {what}", if condition { "ok  " } else { "FAIL" });
    if !condition {
        std::process::exit(1);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args().nth(1).unwrap_or_else(|| "http://127.0.0.1:9119".into());
    let info = probe_server(url).await?;
    let tokens = login_password(info.base_url.clone(), "basic".into(), "admin".into(), "hermes".into()).await?;
    let (done_tx, mut done) = mpsc::unbounded_channel();
    let listener = Arc::new(Turns { done: done_tx, text: Mutex::new(String::new()) });
    let client = HermesClient::new(info.base_url.clone(), tokens, listener)?;
    client.connect().await?;
    // ── session list: the server serves at most 100 per request ──
    let listed = client.list_sessions(250, 0).await?;
    check(listed.len() >= 7, "sessions: asking for more than one page works");
    if listed.len() > 100 {
        let ids: std::collections::HashSet<&str> = listed.iter().map(|s| s.id.as_str()).collect();
        check(ids.len() == listed.len() && listed.len() == 157, "sessions: all 157 loaded across pages, no duplicates");
        let tail = client.list_sessions(100, 100).await?;
        check(tail.len() == 57 && tail[0].id == listed[100].id, "sessions: a later page continues where the first ended");
        let sources: std::collections::HashSet<&str> = listed.iter().map(|s| s.source.as_str()).collect();
        check(sources.len() >= 5, "sessions: chats from every surface are listed, not only this app's");
    }

    // ── usage ──
    let before = client.usage(30).await?;
    check(before.days.len() > 10, "usage: daily history returned");
    check(!before.models.is_empty(), "usage: per-model totals returned");
    let today = before.days.last().expect("today").clone();

    // ── attachments ──
    let live = client.create_session().await?;
    let image = client.attach(live.session_id.clone(), "pixel.png".into(), "image/png".into(), PIXEL.to_vec()).await?;
    check(image.kind == AttachmentKind::Image && image.server_path.is_some(), "attach: image queued, server path returned");
    let notes = client.attach(live.session_id.clone(), "notes.md".into(), "text/markdown".into(), b"# hi".to_vec()).await?;
    check(notes.ref_text.as_deref() == Some("@file:.attachments/notes.md"), "attach: file staged with @file ref");
    let pdf = client.attach(live.session_id.clone(), "paper.pdf".into(), "application/pdf".into(), b"%PDF-1.4".to_vec()).await?;
    check(pdf.kind == AttachmentKind::File && pdf.ref_text.is_some(), "attach: PDF falls back to a file when the server can't render it");

    let prompt = format!("{}\n{}\n\nwhat are these?", notes.ref_text.unwrap(), pdf.ref_text.unwrap());
    let first_row = client.send_prompt(live.session_id.clone(), prompt).await?;
    check(first_row.is_some(), "send: stored row id of the user message returned");
    let (status, text) = next_turn(&mut done).await;
    check(status == TurnStatus::Complete && text.contains("1 image") && text.contains("2 files"), "send: reply acknowledges 1 image and 2 files");

    let history = client.load_messages(live.stored_id.clone()).await?;
    let user = &history[0];
    check(user.role == Role::User && user.row_id == first_row, "history: user message carries its row id");
    check(user.parts == [MessagePart::Text { text: "what are these?".into() }], "history: attachment refs are stripped from the visible text");
    let kinds: Vec<AttachmentKind> = user.attachments.iter().map(|a| a.kind).collect();
    check(kinds == [AttachmentKind::Image, AttachmentKind::File, AttachmentKind::File], "history: 1 image + 2 files listed as attachments");
    check(user.timestamp > 0.0 && history[1].timestamp >= user.timestamp, "history: messages are timestamped");

    // ── rewrite: second message, then rewrite the first while the reply is still streaming ──
    client.send_prompt(live.session_id.clone(), "show me some code".into()).await?;
    next_turn(&mut done).await;
    check(client.load_messages(live.stored_id.clone()).await?.len() == 4, "rewrite: two turns stored before the rewrite");

    client.send_prompt(live.session_id.clone(), "plan an ohio trip".into()).await?;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let busy = client.rewrite_prompt(live.session_id.clone(), "hello".into(), first_row, None).await;
    check(busy.is_err(), "rewrite: refused while a turn is running");
    client.interrupt(live.session_id.clone()).await?;
    let (status, _) = next_turn(&mut done).await;
    check(status == TurnStatus::Interrupted, "rewrite: running turn stops on interrupt");

    // Media is kept: the image is re-queued from history, the file ref travels in the text.
    let reattached = client.reattach_images(live.session_id.clone(), live.stored_id.clone(), first_row.unwrap()).await?;
    check(reattached == 1, "rewrite: image re-attached from the stored message");
    let rewritten = client.rewrite_prompt(live.session_id.clone(), "@file:.attachments/notes.md\n\nhello again".into(), first_row, None).await?;
    check(rewritten.is_some() && rewritten != first_row, "rewrite: accepted, new row id returned");
    let (status, text) = next_turn(&mut done).await;
    check(status == TurnStatus::Complete && text.contains("1 image") && text.contains("1 file"), "rewrite: reply sees the kept image and file");
    let history = client.load_messages(live.stored_id.clone()).await?;
    check(history.len() == 2 && history[0].row_id == rewritten, "rewrite: history was cut back to the rewritten message");
    check(history[0].parts == [MessagePart::Text { text: "hello again".into() }], "rewrite: stored text is the new text");

    let by_ordinal = client.rewrite_prompt(live.session_id.clone(), "third version".into(), None, Some(0)).await?;
    next_turn(&mut done).await;
    check(by_ordinal.is_some() && client.load_messages(live.stored_id.clone()).await?.len() == 2, "rewrite: by position works without a row id");
    let missing = client.rewrite_prompt(live.session_id.clone(), "x".into(), Some(1), None).await;
    check(missing.is_err(), "rewrite: unknown row id is rejected, not sent as a new message");

    let server_image = client.attach_server_image(live.session_id.clone(), image.server_path.unwrap()).await?;
    check(server_image.kind == AttachmentKind::Image, "attach: staged image re-attached by server path");

    // ── usage moved ──
    let after = client.usage(30).await?;
    let now = after.days.last().expect("today");
    check(now.day == today.day && now.output_tokens > today.output_tokens && now.cost_usd > today.cost_usd, "usage: today's totals grew after the turns");
    check(after.plan.is_none(), "usage: no plan allowance reported by this server");

    client.disconnect();
    println!("all feature checks passed");
    Ok(())
}
