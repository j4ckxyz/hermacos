//! A stand-in Hermes dashboard.
//!
//! Speaks just enough of the real surface to exercise a client end to end without an agent or
//! credentials: the native sign-in flow, the session REST API, and the `/api/ws` JSON-RPC
//! gateway with streamed replies, tool activity and an approval round trip.
//!
//!   cargo run -p hermes-mock -- --port 9119        (sign in as admin / hermes)
//!   cargo run -p hermes-mock -- --port 9119 --open (no accounts, like a loopback dashboard)

mod script;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

const USERNAME: &str = "admin";
const PASSWORD: &str = "hermes";
const LEGACY_TOKEN: &str = "mock-legacy-session-token";

struct Session {
    id: String,
    title: String,
    source: String,
    started_at: f64,
    last_active: f64,
    rows: Vec<Value>,
}

struct Pending {
    challenge: String,
    redirect_uri: String,
    state: String,
}

#[derive(Default)]
struct Store {
    sessions: Vec<Session>,
    /// broker id -> pending native authorization
    pending: HashMap<String, Pending>,
    /// one-time code -> PKCE challenge
    codes: HashMap<String, String>,
    access_tokens: Vec<String>,
    refresh_tokens: Vec<String>,
    tickets: Vec<String>,
    /// live (runtime) session id -> stored id
    live: HashMap<String, String>,
    /// approval request id -> answer channel
    approvals: HashMap<String, tokio::sync::oneshot::Sender<String>>,
    /// live session id -> images queued for the next prompt, as (file name, base64)
    pending_images: HashMap<String, Vec<(String, String)>>,
    /// server path of a staged image -> (file name, base64), so it can be attached again by path
    staged_images: HashMap<String, (String, String)>,
    /// live sessions with a turn in flight
    running: Vec<String>,
    next_row: i64,
    /// Tokens and cost added by turns played since the mock started.
    spent_input: u64,
    spent_output: u64,
    spent_cost: f64,
    spent_calls: u64,
}

struct App {
    store: Mutex<Store>,
    open: bool,
    tickets_only: bool,
    /// `--plan`: report a metered plan allowance from `usage.bars`.
    plan: bool,
    counter: AtomicU64,
}

type Shared = Arc<App>;

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl App {
    fn next(&self, prefix: &str) -> String {
        format!("{prefix}{:06x}", self.counter.fetch_add(1, Ordering::Relaxed) * 2_654_435_761 % 0xff_ffff)
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        self.token_ok(bearer.unwrap_or_default())
    }

    fn token_ok(&self, token: &str) -> bool {
        if self.open {
            return token == LEGACY_TOKEN;
        }
        self.store.lock().unwrap().access_tokens.iter().any(|t| t == token)
    }

    fn mint(&self, store: &mut Store) -> Value {
        let access = self.next("at-");
        let refresh = self.next("rt-");
        store.access_tokens.push(access.clone());
        store.refresh_tokens.push(refresh.clone());
        json!({
            "access_token": access, "refresh_token": refresh, "token_type": "Bearer",
            "expires_at": now() as i64 + 12 * 3600, "provider": "basic", "user_id": USERNAME,
        })
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthenticated", "detail": "Unauthorized", "reason": "no_cookie", "login_url": "/login" })),
    )
        .into_response()
}

fn base64url(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    out
}

// ───────────────────────── auth ─────────────────────────

async fn status(State(app): State<Shared>) -> Json<Value> {
    Json(json!({
        "version": "0.21.5-mock", "release_date": "2026.9.24", "gateway_running": true,
        "gateway_state": "running", "auth_required": !app.open,
        "auth_providers": if app.open { json!([]) } else { json!(["basic"]) },
        "auth_flows": if app.open { json!([]) } else { json!(["cookie", "native_pkce"]) },
        "gateway_platforms": { "discord": { "state": "connected" }, "telegram": { "state": "connected" } },
        "active_agents": 0, "active_sessions": 1, "overall": "ok",
        "memory": { "system_total_mb": 3814, "system_available_mb": 1153 },
        "disk": { "total_mb": 38552, "free_mb": 28685 },
    }))
}

async fn providers() -> Json<Value> {
    Json(json!({ "providers": [{ "name": "basic", "display_name": "Username & Password", "supports_password": true }] }))
}

async fn index(State(app): State<Shared>) -> Response {
    if app.open {
        Html(format!("<html><script>window.__HERMES_SESSION_TOKEN__=\"{LEGACY_TOKEN}\";</script></html>")).into_response()
    } else {
        (StatusCode::FOUND, [(header::LOCATION, "/login?next=%2F")]).into_response()
    }
}

async fn authorize(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    if get("code_challenge_method").to_uppercase() != "S256" || get("code_challenge").is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "detail": "code_challenge_method must be S256" }))).into_response();
    }
    if !get("redirect_uri").starts_with("http://127.0.0.1") {
        return (StatusCode::BAD_REQUEST, Json(json!({ "detail": "native redirect_uri host must be a loopback IP literal" }))).into_response();
    }
    let broker = app.next("broker-");
    app.store.lock().unwrap().pending.insert(
        broker.clone(),
        Pending { challenge: get("code_challenge"), redirect_uri: get("redirect_uri"), state: get("state") },
    );
    (
        StatusCode::FOUND,
        [(header::LOCATION, "/login".to_owned()), (header::SET_COOKIE, format!("hermes_session_pkce={broker}; Path=/; HttpOnly; SameSite=Lax"))],
    )
        .into_response()
}

async fn password_login(State(app): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if body["username"] != USERNAME || body["password"] != PASSWORD {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "detail": "Invalid credentials" }))).into_response();
    }
    let broker = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| c.split(';').find_map(|kv| kv.trim().strip_prefix("hermes_session_pkce=")))
        .unwrap_or_default()
        .to_owned();
    let mut store = app.store.lock().unwrap();
    let Some(pending) = store.pending.remove(&broker) else {
        return Json(json!({ "ok": true, "next": "/" })).into_response();
    };
    let code = app.next("code-");
    store.codes.insert(code.clone(), pending.challenge);
    let next = format!("{}?code={code}&state={}", pending.redirect_uri, pending.state);
    Json(json!({ "ok": true, "next": next })).into_response()
}

async fn native_token(State(app): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut store = app.store.lock().unwrap();
    let challenge = store.codes.remove(body["code"].as_str().unwrap_or_default());
    let verifier = body["code_verifier"].as_str().unwrap_or_default();
    match challenge {
        Some(expected) if expected == base64url(&Sha256::digest(verifier.as_bytes())) => {
            Json(app.mint(&mut store)).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({ "detail": "Invalid or expired authorization code." }))).into_response(),
    }
}

async fn native_refresh(State(app): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut store = app.store.lock().unwrap();
    let presented = body["refresh_token"].as_str().unwrap_or_default().to_owned();
    match store.refresh_tokens.iter().position(|t| *t == presented) {
        Some(i) => {
            store.refresh_tokens.remove(i);
            Json(app.mint(&mut store)).into_response()
        }
        None => (StatusCode::UNAUTHORIZED, Json(json!({ "error": "session_expired", "detail": "Refresh token expired or invalid; start a new sign-in." }))).into_response(),
    }
}

/// Test hook: drop every access token so the next call must refresh.
async fn expire_tokens(State(app): State<Shared>) -> Json<Value> {
    app.store.lock().unwrap().access_tokens.clear();
    Json(json!({ "ok": true }))
}

// ───────────────────────── sessions REST ─────────────────────────

fn session_json(s: &Session) -> Value {
    let preview = s.rows.iter().find(|r| r["role"] == "user").and_then(|r| r["content"].as_str()).unwrap_or_default();
    json!({
        "id": s.id, "title": s.title, "source": s.source, "started_at": s.started_at,
        "last_active": s.last_active, "message_count": s.rows.len(), "preview": preview,
        "is_active": false, "model": "hermes-4-405b", "ended_at": null,
    })
}

async fn list_sessions(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !app.authorized(&headers) {
        return unauthorized();
    }
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(20);
    let offset: usize = q.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    // The real endpoint declares `limit <= 100` and FastAPI answers 422 beyond it.
    if limit > 100 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "detail": [{ "type": "less_than_equal", "loc": ["query", "limit"],
                                      "msg": "Input should be less than or equal to 100", "input": limit.to_string() }] })),
        )
            .into_response();
    }
    let store = app.store.lock().unwrap();
    let mut rows: Vec<&Session> = store.sessions.iter().filter(|s| !s.rows.is_empty()).collect();
    rows.sort_by(|a, b| b.last_active.total_cmp(&a.last_active));
    let page: Vec<Value> = rows.iter().skip(offset).take(limit).map(|s| session_json(s)).collect();
    Json(json!({ "sessions": page, "total": rows.len(), "limit": limit, "offset": offset })).into_response()
}

async fn search_sessions(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !app.authorized(&headers) {
        return unauthorized();
    }
    let needle = q.get("q").cloned().unwrap_or_default().to_lowercase();
    let store = app.store.lock().unwrap();
    let results: Vec<Value> = store
        .sessions
        .iter()
        .filter_map(|s| {
            let hit = s.rows.iter().filter_map(|r| r["content"].as_str()).find(|c| c.to_lowercase().contains(&needle));
            let in_title = s.title.to_lowercase().contains(&needle);
            (hit.is_some() || in_title).then(|| {
                json!({
                    "session_id": s.id, "title": s.title, "source": s.source, "last_active": s.last_active,
                    "session_started": s.started_at,
                    "snippet": hit.map(|c| c.chars().take(120).collect::<String>()).unwrap_or_else(|| s.title.clone()),
                })
            })
        })
        .collect();
    Json(json!({ "results": results })).into_response()
}

/// `inline_images=false`: image parts become `[image]` stand-ins, as the real dashboard does.
fn without_inline_images(row: &Value) -> Value {
    let Some(parts) = row["content"].as_array() else { return row.clone() };
    let text: Vec<String> = parts
        .iter()
        .map(|part| match part["type"].as_str() {
            Some("text") => part["text"].as_str().unwrap_or_default().to_owned(),
            _ => "[image]".to_owned(),
        })
        .collect();
    let mut flat = row.clone();
    flat["content"] = json!(text.join("\n"));
    flat
}

async fn session_messages(State(app): State<Shared>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !app.authorized(&headers) {
        return unauthorized();
    }
    let inline = q.get("inline_images").map(String::as_str) != Some("false");
    let store = app.store.lock().unwrap();
    match store.sessions.iter().find(|s| s.id == id) {
        Some(s) => {
            let rows: Vec<Value> = if inline { s.rows.clone() } else { s.rows.iter().map(without_inline_images).collect() };
            Json(json!({ "messages": rows, "pagination": { "order": "latest", "returned": rows.len() } })).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(json!({ "detail": "Session not found" }))).into_response(),
    }
}

async fn patch_session(State(app): State<Shared>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    if !app.authorized(&headers) {
        return unauthorized();
    }
    let mut store = app.store.lock().unwrap();
    match store.sessions.iter_mut().find(|s| s.id == id) {
        Some(s) => {
            if let Some(title) = body["title"].as_str() {
                s.title = title.to_owned();
            }
            Json(json!({ "ok": true, "title": s.title })).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(json!({ "detail": "Session not found" }))).into_response(),
    }
}

async fn delete_session(State(app): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if !app.authorized(&headers) {
        return unauthorized();
    }
    app.store.lock().unwrap().sessions.retain(|s| s.id != id);
    Json(json!({ "ok": true })).into_response()
}

/// `YYYY-MM-DD` (UTC) for a unix time. Days-to-civil after Howard Hinnant.
fn day_string(unix: f64) -> String {
    let days = (unix / 86_400.0).floor() as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

async fn usage_analytics(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !app.authorized(&headers) {
        return unauthorized();
    }
    let days: i64 = q.get("days").and_then(|v| v.parse().ok()).unwrap_or(30);
    let store = app.store.lock().unwrap();
    let daily: Vec<Value> = (0..days.min(30))
        .rev()
        .filter(|back| back % 6 != 4) // a few idle days
        .map(|back| {
            // Deterministic, uneven history; today starts light and grows with each turn.
            let wave = ((back * 37 + 11) % 17) as u64;
            let (mut input, mut output, mut cost, mut calls) = if back == 0 {
                (61_000, 9_400, 0.38, 14)
            } else {
                (90_000 + wave * 41_000, 14_000 + wave * 5_200, 0.55 + wave as f64 * 0.21, 20 + wave * 3)
            };
            if back == 0 {
                input += store.spent_input;
                output += store.spent_output;
                cost += store.spent_cost;
                calls += store.spent_calls;
            }
            json!({
                "day": day_string(now() - back as f64 * 86_400.0),
                "input_tokens": input, "output_tokens": output, "cache_read_tokens": input / 3,
                "reasoning_tokens": output / 5, "estimated_cost": cost, "actual_cost": 0,
                "sessions": 2 + wave % 5, "api_calls": calls,
            })
        })
        .collect();
    Json(json!({
        "daily": daily,
        "by_model": [
            { "model": "hermes-4-405b", "input_tokens": 2_410_000u64, "output_tokens": 388_000, "estimated_cost": 14.2, "sessions": 61, "api_calls": 540 },
            { "model": "claude-sonnet-4.6", "input_tokens": 930_000, "output_tokens": 121_000, "estimated_cost": 6.9, "sessions": 18, "api_calls": 160 },
            { "model": "gemini-3-flash (vision)", "input_tokens": 210_000, "output_tokens": 19_000, "estimated_cost": 0.4, "sessions": 9, "api_calls": 31 },
        ],
        "totals": {}, "period_days": days,
    }))
    .into_response()
}

// ───────────────────────── gateway WebSocket ─────────────────────────

/// Single-use upgrade ticket, as minted for the dashboard's own browser client.
async fn ws_ticket(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if !app.authorized(&headers) {
        return unauthorized();
    }
    let ticket = app.next("ticket-");
    app.store.lock().unwrap().tickets.push(ticket.clone());
    Json(json!({ "ticket": ticket, "ttl_seconds": 30 })).into_response()
}

async fn gateway(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>, upgrade: WebSocketUpgrade) -> Response {
    let ticket_ok = q.get("ticket").is_some_and(|ticket| {
        let mut store = app.store.lock().unwrap();
        let found = store.tickets.iter().position(|t| t == ticket);
        found.map(|i| store.tickets.remove(i)).is_some()
    });
    // `--legacy-ws` mimics servers that only take tickets (no access token on the upgrade).
    let token_ok = !app.tickets_only && app.token_ok(q.get("token").map(String::as_str).unwrap_or_default());
    if !ticket_ok && !token_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    upgrade.on_upgrade(move |socket| serve_socket(app, socket))
}

fn event(kind: &str, session_id: &str, payload: Value) -> String {
    json!({ "jsonrpc": "2.0", "method": "event", "params": { "type": kind, "session_id": session_id, "payload": payload } }).to_string()
}

async fn serve_socket(app: Shared, mut socket: WebSocket) {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let _ = tx.send(event("gateway.ready", "", json!({ "skin": {}, "change_events": true, "replay_epoch": "mock", "heartbeat": true })));
    let interrupts: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> = Arc::default();
    loop {
        tokio::select! {
            Some(text) = rx.recv() => {
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break };
                let Message::Text(text) = message else { continue };
                let Ok(frame) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                handle_frame(&app, &tx, &interrupts, frame);
            }
        }
    }
}

fn handle_frame(
    app: &Shared,
    tx: &mpsc::UnboundedSender<String>,
    interrupts: &Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    frame: Value,
) {
    let id = frame["id"].clone();
    let params = frame["params"].clone();
    let reply = |result: Value| {
        let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string());
    };
    let Some(method) = frame["method"].as_str() else {
        // A response to one of our server→client requests (approval).
        if let Some(request_id) = id.as_str() {
            if let Some(answer) = app.store.lock().unwrap().approvals.remove(request_id) {
                let _ = answer.send(frame["result"]["choice"].as_str().unwrap_or("deny").to_owned());
            }
        }
        return;
    };
    let info = json!({ "model": "hermes-4-405b", "provider": "nous", "title": "", "running": false });
    match method {
        "client.capabilities" => reply(json!({ "server_requests": ["approval", "clarify", "sudo", "secret"] })),
        "gateway.ping" => reply(json!({})),
        "session.create" => {
            let live = app.next("live");
            let stored = app.next("2026");
            let mut store = app.store.lock().unwrap();
            store.live.insert(live.clone(), stored.clone());
            store.sessions.push(Session { id: stored.clone(), title: String::new(), source: "desktop".into(), started_at: now(), last_active: now(), rows: Vec::new() });
            reply(json!({ "session_id": live, "stored_session_id": stored, "message_count": 0, "messages": [], "info": info }));
        }
        "session.resume" => {
            let stored = params["session_id"].as_str().unwrap_or_default().to_owned();
            let mut store = app.store.lock().unwrap();
            if !store.sessions.iter().any(|s| s.id == stored) {
                let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": 4007, "message": "session not found" } }).to_string());
                return;
            }
            let live = app.next("live");
            store.live.insert(live.clone(), stored.clone());
            reply(json!({ "session_id": live, "stored_session_id": stored, "message_count": 0, "messages": [], "messages_omitted": true, "info": info, "running": false }));
        }
        "prompt.submit" => {
            let live = params["session_id"].as_str().unwrap_or_default().to_owned();
            let text = params["text"].as_str().unwrap_or_default().to_owned();
            let fail = |code: i64, message: &str| {
                let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string());
            };
            let mut store = app.store.lock().unwrap();
            let Some(stored) = store.live.get(&live).cloned() else {
                return fail(4001, "unknown session");
            };
            if store.running.contains(&live) {
                return fail(4009, "session busy");
            }
            let by_row = params.get("truncate_before_row_id").filter(|v| !v.is_null());
            let by_ordinal = params.get("truncate_before_user_ordinal").filter(|v| !v.is_null());
            if (by_row.is_some() || by_ordinal.is_some()) && params["confirm_truncate"] != true {
                return fail(4031, "truncation parameters require confirm_truncate=true");
            }
            let images = store.pending_images.remove(&live).unwrap_or_default();
            store.next_row += 1;
            let row_id = store.next_row;
            let Some(session) = store.sessions.iter_mut().find(|s| s.id == stored) else {
                return fail(4007, "session not found");
            };
            // Rewind: drop the addressed user message and everything after it.
            let cut = match (by_row.and_then(Value::as_i64), by_ordinal.and_then(Value::as_u64)) {
                (Some(target), _) => Some(session.rows.iter().position(|r| r["id"] == target && r["role"] == "user")),
                (None, Some(ordinal)) => Some(
                    session.rows.iter().enumerate().filter(|(_, r)| r["role"] == "user").nth(ordinal as usize).map(|(i, _)| i),
                ),
                (None, None) => None,
            };
            match cut {
                Some(Some(index)) => session.rows.truncate(index),
                Some(None) => return fail(4030, "truncation target not found in this session"),
                None => {}
            }
            let image_names: Vec<String> = images.iter().map(|(name, _)| name.clone()).collect();
            let content = if images.is_empty() {
                json!(text)
            } else {
                let mut parts = vec![json!({ "type": "text", "text": text })];
                parts.extend(images.iter().map(|(_, data)| {
                    json!({ "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{data}") } })
                }));
                json!(parts)
            };
            session.rows.push(json!({ "id": row_id, "role": "user", "content": content, "timestamp": now() }));
            session.last_active = now();
            store.running.push(live.clone());
            drop(store);
            reply(json!({ "status": "streaming", "user_row_id": row_id }));
            let stop = Arc::new(AtomicBool::new(false));
            interrupts.lock().unwrap().insert(live.clone(), stop.clone());
            tokio::spawn(run_turn(app.clone(), tx.clone(), live, stored, text, image_names, stop));
        }
        "image.attach_bytes" => {
            let live = params["session_id"].as_str().unwrap_or_default().to_owned();
            let data = params["content_base64"].as_str().unwrap_or_default().to_owned();
            let name = params["filename"].as_str().filter(|n| !n.is_empty()).unwrap_or("image.png").to_owned();
            if data.is_empty() {
                let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": 4015, "message": "content_base64 required" } }).to_string());
                return;
            }
            let path = format!("/home/hermes/.hermes/uploads/{}_{name}", app.next("upload"));
            let bytes = data.len() * 3 / 4;
            let mut store = app.store.lock().unwrap();
            store.staged_images.insert(path.clone(), (name.clone(), data.clone()));
            let queue = store.pending_images.entry(live).or_default();
            queue.push((name.clone(), data));
            reply(json!({ "attached": true, "path": path, "name": name, "count": queue.len(), "bytes": bytes,
                          "text": format!("[User attached image: {name}]") }));
        }
        "image.attach" => {
            let live = params["session_id"].as_str().unwrap_or_default().to_owned();
            let path = params["path"].as_str().unwrap_or_default().to_owned();
            let mut store = app.store.lock().unwrap();
            match store.staged_images.get(&path).cloned() {
                Some(staged) => {
                    let queue = store.pending_images.entry(live).or_default();
                    queue.push(staged);
                    reply(json!({ "attached": true, "path": path, "count": queue.len() }));
                }
                None => reply(json!({ "attached": false, "message": format!("no such image: {path}") })),
            }
        }
        "image.detach" => reply(json!({ "detached": true, "count": 0 })),
        // No PDF renderer here, like a server without poppler: clients fall back to file.attach.
        "pdf.attach" => {
            let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": 5028, "message": "pdftoppm not installed (poppler-utils package required)" } }).to_string());
        }
        "file.attach" => {
            let name = params["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("file").to_owned();
            if !params["data_url"].as_str().is_some_and(|d| d.starts_with("data:") && d.contains(";base64,")) {
                let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": 4015, "message": "path or data_url required" } }).to_string());
                return;
            }
            let ref_path = format!(".attachments/{name}");
            let quoted = if name.contains(' ') { format!("`{ref_path}`") } else { ref_path.clone() };
            reply(json!({ "attached": true, "name": name, "path": format!("/home/hermes/workspace/{ref_path}"),
                          "ref_path": ref_path, "ref_text": format!("@file:{quoted}"), "uploaded": true }));
        }
        "commands.catalog" => reply(script::command_catalog()),
        "slash.exec" => {
            let command = params["command"].as_str().unwrap_or_default().trim().trim_start_matches('/').to_owned();
            let (name, arg) = command.split_once(' ').map_or((command.as_str(), ""), |(n, a)| (n, a.trim()));
            if !app.store.lock().unwrap().live.contains_key(params["session_id"].as_str().unwrap_or_default()) {
                let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": 4001, "message": "unknown session" } }).to_string());
                return;
            }
            match script::slash_output(name, arg) {
                Some(output) => reply(json!({ "output": output })),
                // Skills and composer directives are refused here, as the real gateway does.
                None => {
                    let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": 4018, "message": format!("skill command: use command.dispatch for /{name}") } }).to_string());
                }
            }
        }
        "command.dispatch" => {
            let name = params["name"].as_str().unwrap_or_default().trim_start_matches('/');
            let arg = params["arg"].as_str().unwrap_or_default();
            match script::slash_dispatch(name, arg) {
                Some(directive) => reply(directive),
                None => {
                    let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": 4018, "message": format!("not a quick/plugin/bundle/skill command: {name}") } }).to_string());
                }
            }
        }
        "usage.bars" => {
            if app.plan {
                reply(json!({ "available": true, "plan_name": "Nous Pro", "renews_display": "renews 1 Nov",
                              "plan_bar": { "kind": "plan", "pct_used": 62.0, "fill_fraction": 0.62,
                                            "spent_display": "$12.40", "total_display": "$20.00", "remaining_display": "$7.60" } }));
            } else {
                reply(json!({ "available": false }));
            }
        }
        "session.interrupt" => {
            let live = params["session_id"].as_str().unwrap_or_default();
            if let Some(stop) = interrupts.lock().unwrap().get(live) {
                stop.store(true, Ordering::SeqCst);
            }
            reply(json!({ "status": "interrupted", "interrupted": true }));
        }
        "session.close" => reply(json!({ "closed": true })),
        other => {
            let _ = tx.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("unknown method: {other}") } }).to_string());
        }
    }
}

/// Play one scripted assistant turn: bursty text deltas, tool activity, maybe an approval.
async fn run_turn(app: Shared, tx: mpsc::UnboundedSender<String>, live: String, stored: String, prompt: String, images: Vec<String>, stop: Arc<AtomicBool>) {
    let send = |kind: &str, payload: Value| {
        let _ = tx.send(event(kind, &live, payload));
    };
    let mut seed = prompt.len() as u64 * 7919 + 17;
    let mut random = move |max: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % max.max(1)
    };
    let mut turn = script::turn_for(&prompt);
    // Say back what arrived, so a client can see its uploads made the round trip.
    let files: Vec<&str> = prompt.lines().filter_map(|l| l.trim().strip_prefix("@file:")).collect();
    if !images.is_empty() || !files.is_empty() {
        let mut seen = Vec::new();
        if !images.is_empty() {
            seen.push(format!("**{} image{}** ({})", images.len(), if images.len() == 1 { "" } else { "s" }, images.join(", ")));
        }
        if !files.is_empty() {
            seen.push(format!("**{} file{}** ({})", files.len(), if files.len() == 1 { "" } else { "s" },
                              files.iter().map(|f| format!("`{}`", f.trim_matches('`'))).collect::<Vec<_>>().join(", ")));
        }
        turn.insert(0, script::Step::Say(format!("I received {}.\n\n", seen.join(" and "))));
    }
    send("message.start", json!({}));
    tokio::time::sleep(Duration::from_millis(350)).await;

    let mut spoken = String::new();
    let mut interrupted = false;
    'steps: for step in &turn {
        match step {
            script::Step::Say(text) => {
                let chars: Vec<char> = text.chars().collect();
                let mut i = 0;
                while i < chars.len() {
                    if stop.load(Ordering::SeqCst) {
                        interrupted = true;
                        break 'steps;
                    }
                    let n = (1 + random(9) as usize).min(chars.len() - i);
                    let chunk: String = chars[i..i + n].iter().collect();
                    i += n;
                    spoken.push_str(&chunk);
                    // "nostream" in the prompt mimics a provider that only delivers whole replies.
                    if prompt.contains("nostream") {
                        continue;
                    }
                    send("message.delta", json!({ "text": chunk }));
                    // Mostly quick, sometimes a stall followed by a burst, like a real model.
                    let pause = match random(20) {
                        0 => 260,
                        1..=3 => 0,
                        _ => 12 + random(26),
                    };
                    tokio::time::sleep(Duration::from_millis(pause)).await;
                }
            }
            script::Step::Think(text) => {
                for chunk in text.split_inclusive(' ') {
                    if stop.load(Ordering::SeqCst) {
                        interrupted = true;
                        break 'steps;
                    }
                    send("reasoning.delta", json!({ "text": chunk }));
                    tokio::time::sleep(Duration::from_millis(28)).await;
                }
            }
            script::Step::Tool { name, context, summary, millis } => {
                send("message.interim", json!({ "text": "", "already_streamed": true }));
                let tool_id = app.next("call-");
                send("tool.start", json!({ "tool_id": tool_id, "name": name, "context": context, "args": { "query": context } }));
                tokio::time::sleep(Duration::from_millis(*millis)).await;
                send("tool.complete", json!({ "tool_id": tool_id, "name": name, "duration_s": *millis as f64 / 1000.0, "summary": summary }));
                spoken.push_str("\n\n");
            }
            script::Step::Approve { command, description } => {
                let request_id = app.next("srq-");
                let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
                app.store.lock().unwrap().approvals.insert(request_id.clone(), answer_tx);
                let _ = tx.send(json!({
                    "jsonrpc": "2.0", "id": request_id, "method": "approval",
                    "params": { "session_id": live, "request_id": request_id, "command": command,
                                "description": description, "tool_name": "terminal",
                                "choices": ["once", "session", "always", "deny"] }
                }).to_string());
                let choice = answer_rx.await.unwrap_or_else(|_| "deny".into());
                let verdict = if choice == "deny" { "Okay, I won't run that.\n\n" } else { "Approved. Running it now.\n\n" };
                spoken.push_str(verdict);
                send("message.delta", json!({ "text": verdict }));
                if choice == "deny" {
                    break 'steps;
                }
            }
        }
    }

    let title = {
        let mut store = app.store.lock().unwrap();
        store.running.retain(|id| *id != live);
        store.next_row += 1;
        let row_id = store.next_row;
        store.spent_input += 1_200 + prompt.len() as u64 / 4;
        store.spent_output += spoken.len() as u64 / 4;
        store.spent_cost += 0.004 + spoken.len() as f64 * 0.000_02;
        store.spent_calls += 1;
        let Some(session) = store.sessions.iter_mut().find(|s| s.id == stored) else { return };
        session.rows.push(json!({ "id": row_id, "role": "assistant", "content": spoken, "timestamp": now() }));
        session.last_active = now();
        let fresh = session.title.is_empty();
        if fresh {
            session.title = script::title_for(&prompt);
        }
        fresh.then(|| session.title.clone())
    };
    // The real gateway relays every model response as `reasoning.available` (for providers
    // that don't stream) and rewrites a spinner line through `thinking.delta`.
    send("thinking.delta", json!({ "text": "(´·_·`) mulling..." }));
    send("reasoning.available", json!({ "text": spoken.chars().take(500).collect::<String>() }));
    send("message.complete", json!({
        "text": spoken, "status": if interrupted { "interrupted" } else { "complete" },
        "usage": { "input": 1200, "output": spoken.len() / 4 },
    }));
    if let Some(title) = title {
        tokio::time::sleep(Duration::from_millis(400)).await;
        send("session.title", json!({ "session_id": stored, "title": title }));
    }
    let _ = tx.send(event("sessions.changed", "", json!({})));
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port = args.iter().position(|a| a == "--port").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok()).unwrap_or(9119u16);
    let open = args.iter().any(|a| a == "--open");
    let app = Arc::new(App {
        store: Mutex::new(Store {
            sessions: script::seed_sessions(now(), args.iter().any(|a| a == "--many")),
            next_row: 5000,
            ..Store::default()
        }),
        open,
        tickets_only: args.iter().any(|a| a == "--legacy-ws"),
        plan: args.iter().any(|a| a == "--plan"),
        counter: AtomicU64::new(1),
    });
    let router = Router::new()
        .route("/", get(index))
        .route("/api/status", get(status))
        .route("/api/auth/providers", get(providers))
        .route("/auth/native/authorize", get(authorize))
        .route("/auth/password-login", post(password_login))
        .route("/auth/native/token", post(native_token))
        .route("/auth/native/refresh", post(native_refresh))
        .route("/__test/expire", post(expire_tokens))
        .route("/api/auth/ws-ticket", post(ws_ticket))
        .route("/api/analytics/usage", get(usage_analytics))
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/search", get(search_sessions))
        .route("/api/sessions/{id}", axum::routing::patch(patch_session).delete(delete_session))
        .route("/api/sessions/{id}/messages", get(session_messages))
        .route("/api/ws", get(gateway))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.expect("port in use");
    println!("hermes-mock listening on http://127.0.0.1:{port} ({})", if open { "open" } else { "sign in: admin / hermes" });
    axum::serve(listener, router).await.expect("server failed");
}
