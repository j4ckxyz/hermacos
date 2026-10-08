//! Plain data that crosses the FFI boundary.

use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AuthProvider {
    pub name: String,
    pub display_name: String,
    pub supports_password: bool,
}

/// What `probe_server` learned about a dashboard URL.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ServerInfo {
    /// Canonical base URL (scheme + host + optional mount prefix, no trailing slash).
    pub base_url: String,
    pub host: String,
    pub version: String,
    pub auth_required: bool,
    pub providers: Vec<AuthProvider>,
    /// The server brokers RFC 8252 native sign-in (bearer tokens instead of cookies).
    pub supports_native_flow: bool,
    pub gateway_state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TokenKind {
    /// Provider-minted access token from the native sign-in flow.
    Bearer,
    /// Process-lifetime dashboard token of a server that runs without auth.
    Legacy,
}

/// Credentials for one dashboard. The shell persists these in the OS keychain.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AuthTokens {
    pub kind: TokenKind,
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds; 0 when the token does not expire.
    pub expires_at: i64,
    pub provider: String,
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub preview: String,
    /// Where the conversation happened: desktop, cli, discord, telegram, cron, ...
    pub source: String,
    pub started_at: f64,
    pub last_active: f64,
    pub message_count: u32,
    pub is_active: bool,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SearchHit {
    pub session_id: String,
    pub title: String,
    pub snippet: String,
    pub source: String,
    pub last_active: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ToolStatus {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ToolCall {
    pub id: String,
    /// Raw tool name (`web_search`).
    pub name: String,
    /// Human label (`Web search`).
    pub title: String,
    /// One-line context: the query, command or path.
    pub detail: Option<String>,
    /// Short result summary once finished.
    pub summary: Option<String>,
    pub status: ToolStatus,
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum MessagePart {
    Text { text: String },
    Reasoning { text: String },
    Tool { call: ToolCall },
    Notice { text: String },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ChatMessage {
    pub id: String,
    pub role: Role,
    pub parts: Vec<MessagePart>,
    /// Unix seconds; 0 when the store has no time for the row.
    pub timestamp: f64,
    /// Durable store id of a user message; the address used to rewrite it.
    pub row_id: Option<i64>,
    /// Media the user attached to this message.
    pub attachments: Vec<MessageAttachment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AttachmentKind {
    Image,
    File,
}

/// Media on a stored user message.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MessageAttachment {
    pub name: String,
    pub kind: AttachmentKind,
    /// The `@file:` reference that carries a staged file into a prompt.
    pub ref_text: Option<String>,
    /// Where an attached image lives on the server, when the transcript says.
    pub server_path: Option<String>,
}

/// One attachment uploaded to a live session, ready for the next prompt.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct StagedAttachment {
    pub kind: AttachmentKind,
    /// Text to include in the prompt (files). Images ride with the session instead.
    pub ref_text: Option<String>,
    /// Server path of a staged image; re-attaches it later without uploading again.
    pub server_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DayUsage {
    /// `YYYY-MM-DD` in the server's local time.
    pub day: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost_usd: f64,
    pub sessions: u32,
    pub api_calls: u32,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ModelUsage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub sessions: u32,
}

/// Allowance of a metered plan, when the provider reports one.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PlanUsage {
    pub plan_name: String,
    /// 0..1 of the allowance spent.
    pub fraction_used: f64,
    pub spent: String,
    pub total: String,
    pub remaining: String,
    pub renews: Option<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct UsageSummary {
    /// One entry per day with activity, oldest first.
    pub days: Vec<DayUsage>,
    /// Models by total tokens over the same period, busiest first.
    pub models: Vec<ModelUsage>,
    pub plan: Option<PlanUsage>,
}

/// A session bound to the live gateway connection.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct LiveSession {
    /// Runtime id used for `send_prompt` / `interrupt`; valid for this connection only.
    pub session_id: String,
    /// Durable id used by the session list and history.
    pub stored_id: String,
    pub title: String,
    pub model: Option<String>,
    /// A turn is in flight (started here or by another client).
    pub running: bool,
    /// Assistant text streamed so far for the in-flight turn.
    pub inflight_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting { attempt: u32, delay_ms: u32, reason: String },
    /// Credentials were rejected; the shell should show sign-in.
    Unauthorized,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TurnStatus {
    Complete,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub session_id: String,
    pub tool_name: Option<String>,
    pub command: String,
    pub description: String,
    /// Subset of `once`, `session`, `always`, `deny`.
    pub choices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ClarifyQuestion {
    pub qid: String,
    pub question: String,
    pub choices: Vec<String>,
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ClarifyRequest {
    pub request_id: String,
    pub session_id: String,
    pub questions: Vec<ClarifyQuestion>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct InputRequest {
    pub request_id: String,
    pub session_id: String,
    pub title: String,
    pub prompt: String,
    pub masked: bool,
}

/// Everything the gateway pushes, already decoded into chat-level meaning.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum ChatEvent {
    TurnStarted { session_id: String },
    TextDelta { session_id: String, text: String },
    ReasoningDelta { session_id: String, text: String },
    /// The text of a model response, offered as reasoning for providers that don't stream.
    /// Redundant once that response has streamed as text or reasoning; shells show it only
    /// when nothing else arrived.
    ReasoningAvailable { session_id: String, text: String },
    /// The assistant finished one text segment (commentary beside tool calls).
    SegmentBreak { session_id: String },
    ToolStarted { session_id: String, call: ToolCall },
    ToolCompleted { session_id: String, call: ToolCall },
    Status { session_id: String, kind: String, text: String },
    TurnCompleted { session_id: String, text: String, status: TurnStatus, error: Option<String> },
    TitleChanged { stored_id: String, title: String },
    ModelChanged { session_id: String, model: String },
    SessionsChanged,
    Approval { request: ApprovalRequest },
    Clarify { request: ClarifyRequest },
    Input { request: InputRequest },
    RequestCancelled { request_id: String },
    Notice { session_id: String, message: String },
    Failure { session_id: String, message: String },
}

/// Implemented by the shell. Called from background threads.
#[uniffi::export(with_foreign)]
pub trait HermesListener: Send + Sync {
    fn on_connection(&self, state: ConnectionState);
    fn on_event(&self, event: ChatEvent);
    /// Tokens were refreshed; persist the new set.
    fn on_tokens(&self, tokens: AuthTokens);
}

/// Opens a URL in the user's browser (browser-based sign-in).
#[uniffi::export(with_foreign)]
pub trait UrlOpener: Send + Sync {
    fn open_url(&self, url: String);
}

pub(crate) type Listener = Arc<dyn HermesListener>;

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PlatformStatus {
    pub name: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ServerStatus {
    pub version: String,
    pub release_date: String,
    pub gateway_state: String,
    pub overall: String,
    pub active_sessions: u32,
    pub active_agents: u32,
    pub platforms: Vec<PlatformStatus>,
    pub memory_available_mb: u32,
    pub memory_total_mb: u32,
    pub disk_free_mb: u32,
    pub disk_total_mb: u32,
}
