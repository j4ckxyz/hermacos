//! End-to-end check of the core against a dashboard, from the terminal.
//!
//!   cargo run -p hermes-core --example smoke -- http://127.0.0.1:9119 admin hermes "plan a trip"
//!
//! Probes the address, signs in, lists sessions, opens the newest transcript, then starts a
//! new conversation and prints the streamed reply.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hermes_core::{
    AuthTokens, ChatEvent, ConnectionState, HermesClient, HermesListener, MessagePart, login_open,
    login_password, probe_server,
};

#[derive(Default)]
struct Printer {
    done: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    approve: Mutex<Option<Arc<HermesClient>>>,
}

impl HermesListener for Printer {
    fn on_connection(&self, state: ConnectionState) {
        eprintln!("[connection] {state:?}");
    }

    fn on_event(&self, event: ChatEvent) {
        match event {
            ChatEvent::TextDelta { text, .. } => print!("{text}"),
            ChatEvent::TurnCompleted { status, error, text, .. } => {
                println!("\n[turn {status:?}] {} chars, error={error:?}", text.len());
                if let Some(done) = self.done.lock().unwrap().take() {
                    let _ = done.send(());
                }
            }
            ChatEvent::Approval { request } => {
                println!("\n[approval] {} -> approving once", request.command);
                if let Some(client) = self.approve.lock().unwrap().as_ref() {
                    client.respond_approval(request.request_id, "once".into()).expect("respond");
                }
            }
            other => println!("\n[event] {other:?}"),
        }
    }

    fn on_tokens(&self, tokens: AuthTokens) {
        eprintln!("[tokens] refreshed, expires_at={}", tokens.expires_at);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = args.first().cloned().unwrap_or_else(|| "http://127.0.0.1:9119".into());
    let prompt = args.get(3).cloned().unwrap_or_else(|| "hello".into());

    let info = probe_server(url).await?;
    println!("server: {} v{} auth_required={} providers={:?}", info.base_url, info.version, info.auth_required, info.providers);

    let tokens = if info.auth_required {
        let provider = info.providers.iter().find(|p| p.supports_password).ok_or("no password provider")?;
        let username = args.get(1).ok_or("username required")?.clone();
        let password = args.get(2).ok_or("password required")?.clone();
        login_password(info.base_url.clone(), provider.name.clone(), username, password).await?
    } else {
        login_open(info.base_url.clone()).await?
    };
    println!("signed in as {:?} via {} ({:?})", tokens.user_id, tokens.provider, tokens.kind);

    let printer = Arc::new(Printer::default());
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    *printer.done.lock().unwrap() = Some(done_tx);
    let client = HermesClient::new(info.base_url.clone(), tokens, printer.clone())?;
    *printer.approve.lock().unwrap() = Some(client.clone());

    let sessions = client.list_sessions(20, 0).await?;
    println!("{} sessions:", sessions.len());
    for s in &sessions {
        println!("  {:<24} {:<10} {:>3} msgs  {}", s.id, s.source, s.message_count, s.title);
    }
    if let Some(first) = sessions.first() {
        let messages = client.load_messages(first.id.clone()).await?;
        let parts: usize = messages.iter().map(|m| m.parts.len()).sum();
        let tools = messages.iter().flat_map(|m| &m.parts).filter(|p| matches!(p, MessagePart::Tool { .. })).count();
        println!("newest transcript: {} messages, {parts} parts, {tools} tool calls", messages.len());
    }
    let hits = client.search_sessions("gateway".into()).await?;
    println!("search 'gateway': {} hits", hits.len());

    // Against the mock, `SMOKE_EXPIRE=1` invalidates every access token first, so the next
    // calls must go through a refresh.
    if std::env::var("SMOKE_EXPIRE").is_ok() {
        let expire = format!("{}/__test/expire", info.base_url);
        tokio::process::Command::new("curl").args(["-s", "-X", "POST", &expire]).output().await?;
        let after = client.list_sessions(5, 0).await?;
        println!("after expiry: {} sessions listed", after.len());
    }

    client.connect().await?;
    let live = client.create_session().await?;
    println!("live session {} (stored {}) model={:?}", live.session_id, live.stored_id, live.model);
    let row = client.send_prompt(live.session_id.clone(), prompt).await?;
    println!("user row id: {row:?}");
    tokio::time::timeout(Duration::from_secs(120), done_rx).await??;
    tokio::time::sleep(Duration::from_millis(700)).await;
    client.disconnect();
    Ok(())
}
