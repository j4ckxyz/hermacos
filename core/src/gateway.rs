//! The live connection: JSON-RPC over the dashboard's `/api/ws` WebSocket.
//!
//! Three kinds of frame travel on it. We send requests and get responses by id. The server sends
//! `event` notifications (streamed text, tool activity, turn lifecycle). And the server sends its
//! own requests when the agent needs the user (approve a command, answer a question), which we
//! surface as events and answer later by id.
//!
//! One task owns the socket. It reconnects with backoff, keeps the link alive with
//! `gateway.ping`, and fails every in-flight call when the socket drops so nothing hangs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::{self, Message};

use crate::auth::AuthState;
use crate::error::{HermesError, Result};
use crate::transcript::tool_detail;
use crate::types::{
    ApprovalRequest, ChatEvent, ClarifyQuestion, ClarifyRequest, ConnectionState, InputRequest,
    Listener, ToolCall, ToolStatus, TurnStatus,
};
use crate::util::{bool_of, encode, humanize, one_line, opt_str, str_of};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
/// No inbound frame for this long after pinging means the link is dead (sleep, VPN drop).
const HEARTBEAT_DEADLINE: Duration = Duration::from_secs(50);
const CONNECT_WAIT: Duration = Duration::from_secs(20);
const BACKOFF_MS: [u32; 6] = [500, 1_000, 2_000, 4_000, 8_000, 15_000];
const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    Connecting,
    Connected,
    Unauthorized,
}

type Pending = oneshot::Sender<Result<Value>>;

pub(crate) struct Gateway {
    auth: Arc<AuthState>,
    listener: Listener,
    pending: Mutex<HashMap<String, Pending>>,
    next_id: AtomicU64,
    outbound: Mutex<Option<mpsc::UnboundedSender<String>>>,
    phase: watch::Sender<Phase>,
    stop: watch::Sender<bool>,
    running: AtomicBool,
}

enum ConnectError {
    Unauthorized,
    Retry(String),
}

enum PumpEnd {
    Stopped,
    Dropped(String),
}

type Socket = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

impl Gateway {
    pub fn new(auth: Arc<AuthState>, listener: Listener) -> Arc<Self> {
        Arc::new(Self {
            auth,
            listener,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            outbound: Mutex::new(None),
            phase: watch::channel(Phase::Idle).0,
            stop: watch::channel(false).0,
            running: AtomicBool::new(false),
        })
    }

    /// Start (or restart) the connection task. Idempotent while it is running.
    pub fn start(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        self.stop.send_replace(false);
        self.phase.send_replace(Phase::Connecting);
        tokio::spawn(self.clone().run());
    }

    pub fn stop(&self) {
        self.stop.send_replace(true);
    }

    pub fn is_connected(&self) -> bool {
        *self.phase.borrow() == Phase::Connected
    }

    /// Resolve once the socket is up, or fail when it cannot be (bad credentials, timeout).
    pub async fn wait_connected(&self) -> Result<()> {
        let mut phase = self.phase.subscribe();
        let wait = async {
            loop {
                match *phase.borrow_and_update() {
                    Phase::Connected => return Ok(()),
                    Phase::Unauthorized => return Err(HermesError::unauthorized()),
                    Phase::Idle => return Err(HermesError::not_connected()),
                    Phase::Connecting => {}
                }
                if phase.changed().await.is_err() {
                    return Err(HermesError::not_connected());
                }
            }
        };
        tokio::time::timeout(CONNECT_WAIT, wait)
            .await
            .unwrap_or_else(|_| Err(HermesError::network("Couldn't connect to Hermes. Still trying…")))
    }

    pub async fn request(self: &Arc<Self>, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.start();
        self.wait_connected().await?;
        let id = format!("r{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if !self.send_raw(frame.to_string()) {
            self.pending.lock().unwrap().remove(&id);
            return Err(HermesError::not_connected());
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(HermesError::not_connected()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(HermesError::network(format!("Hermes didn't answer in time ({method}).")))
            }
        }
    }

    /// Answer a server→client request (approval, clarify, sudo, secret).
    pub fn respond(&self, request_id: &str, result: Value) -> Result<()> {
        let frame = json!({ "jsonrpc": "2.0", "id": request_id, "result": result });
        if self.send_raw(frame.to_string()) { Ok(()) } else { Err(HermesError::not_connected()) }
    }

    fn send_raw(&self, text: String) -> bool {
        self.outbound.lock().unwrap().as_ref().is_some_and(|tx| tx.send(text).is_ok())
    }

    fn stopped(&self) -> bool {
        *self.stop.borrow()
    }

    fn fail_pending(&self) {
        let pending: Vec<Pending> = self.pending.lock().unwrap().drain().map(|(_, tx)| tx).collect();
        for tx in pending {
            let _ = tx.send(Err(HermesError::NotConnected {
                message: "The connection to Hermes dropped. Reconnecting…".into(),
            }));
        }
    }

    async fn run(self: Arc<Self>) {
        let mut attempt: u32 = 0;
        self.listener.on_connection(ConnectionState::Connecting);
        while !self.stopped() {
            let reason = match self.connect_once().await {
                Ok(socket) => {
                    attempt = 0;
                    self.phase.send_replace(Phase::Connected);
                    self.listener.on_connection(ConnectionState::Connected);
                    let end = self.pump(socket).await;
                    *self.outbound.lock().unwrap() = None;
                    self.fail_pending();
                    match end {
                        PumpEnd::Stopped => break,
                        PumpEnd::Dropped(reason) => reason,
                    }
                }
                Err(ConnectError::Unauthorized) => {
                    self.phase.send_replace(Phase::Unauthorized);
                    self.listener.on_connection(ConnectionState::Unauthorized);
                    self.running.store(false, Ordering::SeqCst);
                    return;
                }
                Err(ConnectError::Retry(reason)) => reason,
            };
            if self.stopped() {
                break;
            }
            self.phase.send_replace(Phase::Connecting);
            let delay_ms = BACKOFF_MS[(attempt as usize).min(BACKOFF_MS.len() - 1)];
            attempt += 1;
            self.listener.on_connection(ConnectionState::Reconnecting { attempt, delay_ms, reason });
            let mut stop = self.stop.subscribe();
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(delay_ms as u64)) => {}
                _ = stopped_signal(&mut stop) => break,
            }
        }
        self.phase.send_replace(Phase::Idle);
        self.listener.on_connection(ConnectionState::Disconnected);
        self.running.store(false, Ordering::SeqCst);
    }

    fn socket_url(&self, credential: &str) -> String {
        let mut url = self.auth.base.clone();
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        let _ = url.set_scheme(scheme);
        format!("{}/api/ws?{credential}", url.as_str().trim_end_matches('/'))
    }

    async fn connect_once(&self) -> std::result::Result<Socket, ConnectError> {
        let mut tokens = self.auth.access().await.map_err(connect_error)?;
        let mut refreshed = false;
        loop {
            let credential = match self.auth.socket_ticket(&tokens).await {
                Ok(Some(ticket)) => format!("ticket={}", encode(&ticket)),
                Ok(None) => format!("token={}", encode(&tokens.access_token)),
                Err(HermesError::Unauthorized { .. }) if !refreshed => {
                    refreshed = true;
                    tokens = self.auth.refresh_after(&tokens.access_token).await.map_err(connect_error)?;
                    continue;
                }
                Err(e) => return Err(connect_error(e)),
            };
            match tokio_tungstenite::connect_async(self.socket_url(&credential)).await {
                Ok((socket, _)) => return Ok(socket),
                // The server closes before accepting when the credential is bad, which the
                // handshake reports as 403 (or 401 behind some proxies).
                Err(tungstenite::Error::Http(response))
                    if matches!(response.status().as_u16(), 401 | 403) =>
                {
                    if refreshed {
                        return Err(ConnectError::Retry("The server refused the chat connection.".into()));
                    }
                    refreshed = true;
                    tokens = self.auth.refresh_after(&tokens.access_token).await.map_err(connect_error)?;
                }
                Err(e) => return Err(ConnectError::Retry(friendly_socket_error(&e))),
            }
        }
    }

    async fn pump(&self, socket: Socket) -> PumpEnd {
        let (mut sink, mut stream) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *self.outbound.lock().unwrap() = Some(tx);
        let mut stop = self.stop.subscribe();
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let mut json_heartbeat = false;
        let mut last_inbound = Instant::now();
        let mut ping_seq: u64 = 0;

        loop {
            tokio::select! {
                frame = stream.next() => {
                    let text = match frame {
                        Some(Ok(Message::Text(text))) => text.as_str().to_owned(),
                        Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                        Some(Ok(Message::Close(_))) | None => return PumpEnd::Dropped("Connection closed.".into()),
                        Some(Ok(_)) => { last_inbound = Instant::now(); continue; }
                        Some(Err(e)) => return PumpEnd::Dropped(friendly_socket_error(&e)),
                    };
                    last_inbound = Instant::now();
                    if self.handle_frame(&text) {
                        json_heartbeat = true;
                    }
                }
                Some(text) = rx.recv() => {
                    if let Err(e) = sink.send(Message::Text(text.into())).await {
                        return PumpEnd::Dropped(friendly_socket_error(&e));
                    }
                }
                _ = heartbeat.tick() => {
                    if last_inbound.elapsed() > HEARTBEAT_DEADLINE {
                        return PumpEnd::Dropped("Connection went quiet.".into());
                    }
                    ping_seq += 1;
                    let ping = if json_heartbeat {
                        Message::Text(json!({
                            "jsonrpc": "2.0", "id": format!("heartbeat-{ping_seq}"),
                            "method": "gateway.ping", "params": {}
                        }).to_string().into())
                    } else {
                        Message::Ping(Default::default())
                    };
                    if let Err(e) = sink.send(ping).await {
                        return PumpEnd::Dropped(friendly_socket_error(&e));
                    }
                }
                _ = stopped_signal(&mut stop) => {
                    let _ = sink.close().await;
                    return PumpEnd::Stopped;
                }
            }
        }
    }

    /// Route one inbound frame. Returns true when it was a `gateway.ready` advertising the
    /// JSON heartbeat.
    fn handle_frame(&self, text: &str) -> bool {
        let Ok(frame) = serde_json::from_str::<Value>(text) else { return false };
        let method = frame.get("method").and_then(Value::as_str);
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        match (method, frame.get("id")) {
            (Some("event"), _) => return self.handle_event(&params),
            (Some(method), Some(Value::String(id))) => self.handle_server_request(id, method, &params),
            (None, Some(id)) => {
                let key = match id {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if let Some(tx) = self.pending.lock().unwrap().remove(&key) {
                    let outcome = match frame.get("error") {
                        Some(error) if !error.is_null() => Err(HermesError::Rpc {
                            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                            message: opt_str(error, "message").unwrap_or_else(|| "Hermes request failed.".into()),
                        }),
                        _ => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(outcome);
                }
            }
            _ => {}
        }
        false
    }

    fn handle_event(&self, params: &Value) -> bool {
        let kind = str_of(params, "type");
        let session_id = str_of(params, "session_id");
        let payload = params.get("payload").cloned().unwrap_or(Value::Null);
        let emit = |event: ChatEvent| self.listener.on_event(event);
        match kind.as_str() {
            "gateway.ready" => {
                // Tell the backend we answer its questions; otherwise approvals fail fast.
                let id = format!("r{}", self.next_id.fetch_add(1, Ordering::Relaxed));
                self.send_raw(
                    json!({ "jsonrpc": "2.0", "id": id, "method": "client.capabilities",
                            "params": { "server_requests": true } })
                    .to_string(),
                );
                return bool_of(&payload, "heartbeat");
            }
            "message.start" => emit(ChatEvent::TurnStarted { session_id }),
            "message.delta" => {
                let text = str_of(&payload, "text");
                if !text.is_empty() {
                    emit(ChatEvent::TextDelta { session_id, text });
                }
            }
            "message.interim" => {
                let text = str_of(&payload, "text");
                if !bool_of(&payload, "already_streamed") && !text.is_empty() {
                    emit(ChatEvent::TextDelta { session_id: session_id.clone(), text });
                }
                emit(ChatEvent::SegmentBreak { session_id });
            }
            "reasoning.delta" => {
                let text = str_of(&payload, "text");
                if !text.is_empty() {
                    emit(ChatEvent::ReasoningDelta { session_id, text });
                }
            }
            "reasoning.available" => {
                let text = str_of(&payload, "text");
                if !text.trim().is_empty() {
                    emit(ChatEvent::ReasoningAvailable { session_id, text });
                }
            }
            // The terminal UI's spinner line ("(´·_·`) mulling..."), not model reasoning. Only
            // the notices about a slow provider or a loading model are worth a status line.
            "thinking.delta" => {
                let text = str_of(&payload, "text");
                let notice = text.trim();
                if notice.starts_with('⏳') || notice.starts_with('⚙') {
                    emit(ChatEvent::Status { session_id, kind: "wait".into(), text: notice.to_owned() });
                }
            }
            "tool.start" => emit(ChatEvent::ToolStarted { session_id, call: tool_from(&payload, ToolStatus::Running) }),
            "tool.complete" => {
                let failed = payload
                    .get("result")
                    .and_then(|r| r.get("error"))
                    .is_some_and(|e| !e.is_null() && e != &Value::Bool(false) && e != "");
                let status = if failed { ToolStatus::Failed } else { ToolStatus::Done };
                emit(ChatEvent::ToolCompleted { session_id, call: tool_from(&payload, status) });
            }
            "status.update" => emit(ChatEvent::Status {
                session_id,
                kind: str_of(&payload, "kind"),
                text: str_of(&payload, "text"),
            }),
            "message.complete" => {
                let status = match str_of(&payload, "status").as_str() {
                    "interrupted" => TurnStatus::Interrupted,
                    "error" => TurnStatus::Failed,
                    _ => TurnStatus::Complete,
                };
                let error = opt_str(&payload, "error").or_else(|| opt_str(&payload, "warning").filter(|_| status == TurnStatus::Failed));
                emit(ChatEvent::TurnCompleted { session_id, text: str_of(&payload, "text"), status, error });
            }
            "session.title" => emit(ChatEvent::TitleChanged {
                stored_id: opt_str(&payload, "session_id").unwrap_or(session_id),
                title: str_of(&payload, "title"),
            }),
            "session.info" => {
                if let Some(model) = opt_str(&payload, "model") {
                    emit(ChatEvent::ModelChanged { session_id, model });
                }
            }
            "sessions.changed" => emit(ChatEvent::SessionsChanged),
            "request.cancel" => emit(ChatEvent::RequestCancelled { request_id: str_of(&payload, "id") }),
            "notice" => emit(ChatEvent::Notice { session_id, message: str_of(&payload, "message") }),
            "error" => emit(ChatEvent::Failure { session_id, message: str_of(&payload, "message") }),
            _ => {}
        }
        false
    }

    fn handle_server_request(&self, id: &str, method: &str, params: &Value) {
        let request_id = id.to_owned();
        let session_id = str_of(params, "session_id");
        let event = match method {
            "approval" => {
                let mut choices: Vec<String> = params
                    .get("choices")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect();
                if choices.is_empty() {
                    choices = vec!["once".into(), "session".into(), "always".into(), "deny".into()];
                    if params.get("allow_session") == Some(&Value::Bool(false)) {
                        choices.retain(|c| c != "session");
                    }
                    if params.get("allow_permanent") == Some(&Value::Bool(false)) {
                        choices.retain(|c| c != "always");
                    }
                }
                ChatEvent::Approval {
                    request: ApprovalRequest {
                        request_id,
                        session_id,
                        tool_name: opt_str(params, "tool_name"),
                        command: str_of(params, "command"),
                        description: str_of(params, "description"),
                        choices,
                    },
                }
            }
            "clarify" => ChatEvent::Clarify {
                request: ClarifyRequest {
                    request_id,
                    session_id,
                    questions: params
                        .get("questions")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|q| ClarifyQuestion {
                            qid: str_of(q, "qid"),
                            question: str_of(q, "question"),
                            choices: q
                                .get("choices")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(|c| c.as_str().map(str::to_owned))
                                .collect(),
                            multi_select: bool_of(q, "multi_select"),
                        })
                        .collect(),
                },
            },
            "sudo" => ChatEvent::Input {
                request: InputRequest {
                    request_id,
                    session_id,
                    title: "Administrator password".into(),
                    prompt: opt_str(params, "command")
                        .map(|c| format!("Hermes needs sudo to run: {c}"))
                        .unwrap_or_else(|| "Hermes needs the sudo password on the host.".into()),
                    masked: true,
                },
            },
            "secret" => ChatEvent::Input {
                request: InputRequest {
                    request_id,
                    session_id,
                    title: str_of(params, "env_var"),
                    prompt: str_of(params, "prompt"),
                    masked: true,
                },
            },
            // Desktop-only bridges (preview, terminal, window): decline so the agent moves on.
            _ => {
                self.send_raw(
                    json!({ "jsonrpc": "2.0", "id": id,
                            "error": { "code": METHOD_NOT_FOUND, "message": format!("no handler for server request: {method}") } })
                    .to_string(),
                );
                return;
            }
        };
        self.listener.on_event(event);
    }
}

/// Resolves when the gateway is asked to stop (or its owner is gone).
async fn stopped_signal(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopped| *stopped).await;
}

fn connect_error(e: HermesError) -> ConnectError {
    match e {
        HermesError::Unauthorized { .. } => ConnectError::Unauthorized,
        other => ConnectError::Retry(other.to_string()),
    }
}

fn friendly_socket_error(e: &tungstenite::Error) -> String {
    match e {
        tungstenite::Error::Io(_) | tungstenite::Error::Tls(_) => {
            "Couldn't reach Hermes. Check your network or VPN.".into()
        }
        tungstenite::Error::Http(response) => {
            format!("The server answered {} to the chat connection.", response.status())
        }
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => "Connection closed.".into(),
        other => format!("Connection error: {other}"),
    }
}

fn tool_from(payload: &Value, status: ToolStatus) -> ToolCall {
    let name = str_of(payload, "name");
    let label = payload
        .get("labels")
        .and_then(Value::as_array)
        .and_then(|labels| labels.first())
        .cloned()
        .unwrap_or(Value::Null);
    let title = opt_str(&label, "text").unwrap_or_else(|| humanize(&name));
    let detail = ["preview", "context", "args_text"]
        .iter()
        .find_map(|key| opt_str(payload, key))
        .map(|s| one_line(&s, 140))
        .or_else(|| payload.get("args").and_then(tool_detail))
        .filter(|detail| *detail != title);
    ToolCall {
        id: str_of(payload, "tool_id"),
        name,
        title,
        detail,
        summary: opt_str(payload, "summary").map(|s| one_line(&s, 160)),
        status,
        duration: payload.get("duration_s").and_then(Value::as_f64),
    }
}
