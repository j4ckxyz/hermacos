//! `HermesClient`: one signed-in connection to one dashboard.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use url::Url;

use crate::auth::{self, AuthState};
use crate::error::{HermesError, Result};
use crate::gateway::Gateway;
use crate::on_runtime;
use crate::rest::Rest;
use crate::slash;
use crate::types::{
    AttachmentKind, AuthTokens, ChatMessage, HermesListener, LiveSession, PlanUsage, SearchHit,
    ServerStatus, SessionSummary, SlashCommand, SlashOutcome, StagedAttachment, UsageSummary,
};
use crate::util::{base64_standard, bool_of, opt_str, str_of};

const RPC_TIMEOUT: Duration = Duration::from_secs(120);
/// Width hint the gateway uses to wrap tool previews.
const COLUMNS: u32 = 96;
/// Sessions started here are listed alongside the official desktop app's.
const SOURCE: &str = "desktop";

struct Inner {
    rest: Rest,
    gateway: Arc<Gateway>,
}

#[derive(uniffi::Object)]
pub struct HermesClient {
    inner: Arc<Inner>,
}

/// JSON-RPC "invalid params": the gateway validates params strictly, so a field added after
/// the server's release makes it reject the whole call.
const INVALID_PARAMS: i64 = 4000;

impl Inner {
    /// Call with the full params; if this server predates one of the fields, retry with the
    /// minimal set every version accepts.
    async fn request_compat(&self, method: &str, full: Value, minimal: Value) -> Result<Value> {
        match self.gateway.request(method, full, RPC_TIMEOUT).await {
            Err(HermesError::Rpc { code, message })
                if code == INVALID_PARAMS || code == -32602 || message.contains("invalid params") =>
            {
                self.gateway.request(method, minimal, RPC_TIMEOUT).await
            }
            other => other,
        }
    }
}

fn staged_image(result: &Value) -> Result<StagedAttachment> {
    if !bool_of(result, "attached") {
        let message = opt_str(result, "message").unwrap_or_else(|| "Hermes couldn't attach that image.".into());
        return Err(HermesError::protocol(message));
    }
    Ok(StagedAttachment { kind: AttachmentKind::Image, ref_text: None, server_path: opt_str(result, "path") })
}

fn plan_usage(model: &Value) -> Option<PlanUsage> {
    if !bool_of(model, "available") {
        return None;
    }
    let bar = model.get("plan_bar").filter(|bar| bar.is_object())?;
    let fraction_used = bar
        .get("pct_used")
        .and_then(Value::as_f64)
        .map(|percent| percent / 100.0)
        .or_else(|| bar.get("fill_fraction").and_then(Value::as_f64))
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    Some(PlanUsage {
        plan_name: opt_str(model, "plan_name").unwrap_or_else(|| "Plan".into()),
        fraction_used,
        spent: str_of(bar, "spent_display"),
        total: str_of(bar, "total_display"),
        remaining: str_of(bar, "remaining_display"),
        renews: opt_str(model, "renews_display"),
    })
}

fn live_session(result: &Value, fallback_stored: &str) -> Result<LiveSession> {
    let session_id = str_of(result, "session_id");
    if session_id.is_empty() {
        return Err(HermesError::protocol("Hermes didn't return a session."));
    }
    let info = result.get("info").cloned().unwrap_or(Value::Null);
    let inflight = result.get("inflight").cloned().unwrap_or(Value::Null);
    Ok(LiveSession {
        session_id,
        stored_id: opt_str(result, "stored_session_id")
            .or_else(|| opt_str(&info, "stored_session_id"))
            .or_else(|| opt_str(result, "session_key"))
            .unwrap_or_else(|| fallback_stored.to_owned()),
        title: str_of(&info, "title"),
        model: opt_str(&info, "model"),
        running: bool_of(result, "running") || bool_of(&info, "running"),
        inflight_text: opt_str(&inflight, "assistant"),
    })
}

#[uniffi::export]
impl HermesClient {
    /// `base_url` comes from `probe_server`; `tokens` from a sign-in or the keychain.
    #[uniffi::constructor]
    pub fn new(base_url: String, tokens: AuthTokens, listener: Arc<dyn HermesListener>) -> Result<Arc<Self>> {
        let base = Url::parse(&base_url).map_err(|_| HermesError::protocol("Invalid server address."))?;
        let auth = Arc::new(AuthState::new(base, auth::http_client()?, Some(tokens), listener.clone()));
        Ok(Arc::new(Self {
            inner: Arc::new(Inner {
                rest: Rest::new(auth.clone(), listener.clone()),
                gateway: Gateway::new(auth, listener),
            }),
        }))
    }

    /// Open the live connection. Resolves once connected; keeps reconnecting afterwards.
    pub async fn connect(&self) -> Result<()> {
        let inner = self.inner.clone();
        on_runtime(async move {
            inner.gateway.start();
            inner.gateway.wait_connected().await
        })
        .await
    }

    pub fn disconnect(&self) {
        self.inner.gateway.stop();
    }

    pub async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionSummary>> {
        let inner = self.inner.clone();
        on_runtime(async move { inner.rest.list_sessions(limit, offset).await }).await
    }

    pub async fn search_sessions(&self, query: String) -> Result<Vec<SearchHit>> {
        let inner = self.inner.clone();
        on_runtime(async move { inner.rest.search_sessions(&query).await }).await
    }

    /// Stored transcript of a conversation, oldest first.
    pub async fn load_messages(&self, session_id: String) -> Result<Vec<ChatMessage>> {
        let inner = self.inner.clone();
        on_runtime(async move { inner.rest.load_messages(&session_id).await }).await
    }

    pub async fn rename_session(&self, session_id: String, title: String) -> Result<()> {
        let inner = self.inner.clone();
        on_runtime(async move { inner.rest.rename_session(&session_id, &title).await }).await
    }

    pub async fn delete_session(&self, session_id: String) -> Result<()> {
        let inner = self.inner.clone();
        on_runtime(async move { inner.rest.delete_session(&session_id).await }).await
    }

    pub async fn status(&self) -> Result<ServerStatus> {
        let inner = self.inner.clone();
        on_runtime(async move { inner.rest.status().await }).await
    }

    /// Start a fresh conversation. Nothing is stored until the first prompt.
    pub async fn create_session(&self) -> Result<LiveSession> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let params = json!({ "cols": COLUMNS, "source": SOURCE });
            let result = inner.request_compat("session.create", params, json!({})).await?;
            live_session(&result, "")
        })
        .await
    }

    /// Bind a stored conversation to this connection so it can take prompts and stream events.
    pub async fn resume_session(&self, stored_id: String) -> Result<LiveSession> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let params = json!({
                "session_id": stored_id, "cols": COLUMNS, "source": SOURCE,
                "defer_history": true, "omit_messages": true,
            });
            let minimal = json!({ "session_id": stored_id });
            let result = inner.request_compat("session.resume", params, minimal).await?;
            live_session(&result, &stored_id)
        })
        .await
    }

    /// Send a prompt. Returns once the gateway accepted it; the reply arrives as events.
    /// The result is the stored id of the new user message, when the server reports it.
    pub async fn send_prompt(&self, session_id: String, text: String) -> Result<Option<i64>> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let params = json!({ "session_id": session_id, "text": text });
            let result = inner.gateway.request("prompt.submit", params, RPC_TIMEOUT).await?;
            Ok(result.get("user_row_id").and_then(Value::as_i64))
        })
        .await
    }

    /// Replace an earlier user message: the conversation is cut back to just before it and
    /// `text` is sent in its place. The message is addressed by `row_id` when known;
    /// `user_ordinal`, its zero-based position among the user's messages, is used when there is
    /// no row id or the server is too old to take one. The session must be idle.
    pub async fn rewrite_prompt(
        &self,
        session_id: String,
        text: String,
        row_id: Option<i64>,
        user_ordinal: Option<u32>,
    ) -> Result<Option<i64>> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let submit = |address: (&str, Value)| {
                let mut params = json!({
                    "session_id": session_id, "text": text,
                    "confirm_truncate": true, "confirm_empty_truncate": true,
                });
                params[address.0] = address.1;
                inner.gateway.request("prompt.submit", params, RPC_TIMEOUT)
            };
            let by_ordinal = user_ordinal.map(|ordinal| ("truncate_before_user_ordinal", json!(ordinal)));
            let result = match (row_id, by_ordinal) {
                (Some(row_id), fallback) => match submit(("truncate_before_row_id", json!(row_id))).await {
                    // A gateway that predates row-id addressing rejects the parameter itself;
                    // the position among the user's messages is the address it understands.
                    Err(HermesError::Rpc { code, message })
                        if fallback.is_some() && (code == INVALID_PARAMS || message.contains("invalid params")) =>
                    {
                        submit(fallback.expect("checked above")).await
                    }
                    other => other,
                },
                (None, Some(address)) => submit(address).await,
                (None, None) => {
                    return Err(HermesError::protocol("This message can't be rewritten: its position is unknown."));
                }
            }?;
            Ok(result.get("user_row_id").and_then(Value::as_i64))
        })
        .await
    }

    /// Upload one attachment to a live session so the next prompt carries it.
    ///
    /// Images are queued on the session. PDFs are rendered to page images when the server
    /// can, otherwise staged as files. Everything else is staged as a file and referenced
    /// from the prompt through the returned `ref_text`.
    pub async fn attach(
        &self,
        session_id: String,
        name: String,
        mime: String,
        data: Vec<u8>,
    ) -> Result<StagedAttachment> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let encoded = base64_standard(&data);
            drop(data);
            if mime.starts_with("image/") {
                let params = json!({ "session_id": session_id, "content_base64": encoded, "filename": name });
                let result = inner.gateway.request("image.attach_bytes", params, RPC_TIMEOUT).await?;
                return staged_image(&result);
            }
            if mime == "application/pdf" {
                let params = json!({ "session_id": session_id, "content_base64": encoded, "filename": name });
                if let Ok(result) = inner.gateway.request("pdf.attach", params, RPC_TIMEOUT).await {
                    if bool_of(&result, "attached") {
                        return Ok(StagedAttachment { kind: AttachmentKind::Image, ref_text: None, server_path: None });
                    }
                }
                // No PDF renderer on the server: hand the agent the file itself.
            }
            let mime = if mime.is_empty() { "application/octet-stream".to_owned() } else { mime };
            let params = json!({
                "session_id": session_id, "name": name,
                "data_url": format!("data:{mime};base64,{encoded}"),
            });
            let result = inner.gateway.request("file.attach", params, RPC_TIMEOUT).await?;
            match opt_str(&result, "ref_text") {
                Some(ref_text) if bool_of(&result, "attached") => Ok(StagedAttachment {
                    kind: AttachmentKind::File,
                    ref_text: Some(ref_text),
                    server_path: opt_str(&result, "path"),
                }),
                _ => Err(HermesError::protocol(format!("Hermes couldn't attach {name}."))),
            }
        })
        .await
    }

    /// Queue an image that is already on the server (staged earlier) for the next prompt.
    pub async fn attach_server_image(&self, session_id: String, path: String) -> Result<StagedAttachment> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let params = json!({ "session_id": session_id, "path": path });
            staged_image(&inner.gateway.request("image.attach", params, RPC_TIMEOUT).await?)
        })
        .await
    }

    /// Queue the images of a stored user message again, for rewriting that message.
    /// Returns how many were attached.
    pub async fn reattach_images(&self, session_id: String, stored_id: String, row_id: i64) -> Result<u32> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let mut attached = 0;
            for (filename, content) in inner.rest.row_images(&stored_id, row_id).await? {
                let params = json!({ "session_id": session_id, "content_base64": content, "filename": filename });
                let result = inner.gateway.request("image.attach_bytes", params, RPC_TIMEOUT).await?;
                attached += u32::from(bool_of(&result, "attached"));
            }
            Ok(attached)
        })
        .await
    }

    /// Every slash command available, for the command menu. `session_id` (a live session)
    /// adds the commands specific to that conversation's workspace.
    pub async fn commands(&self, session_id: Option<String>) -> Result<Vec<SlashCommand>> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let params = match session_id {
                Some(id) => json!({ "session_id": id }),
                None => json!({}),
            };
            let catalog = inner.gateway.request("commands.catalog", params, Duration::from_secs(30)).await?;
            Ok(slash::commands_from_catalog(&catalog))
        })
        .await
    }

    /// Run a slash command (`/usage`, `/model gpt`, a skill) in a live session.
    pub async fn run_slash(&self, session_id: String, command: String) -> Result<SlashOutcome> {
        let inner = self.inner.clone();
        on_runtime(async move { slash::run(&inner.gateway, &session_id, &command).await }).await
    }

    /// Daily and per-model usage for the last `days` days, plus the plan allowance if any.
    pub async fn usage(&self, days: u32) -> Result<UsageSummary> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let (days, models) = inner.rest.usage(days).await?;
            // Only metered plans answer this, and only over the live connection.
            let plan = if inner.gateway.is_connected() {
                inner.gateway.request("usage.bars", json!({}), Duration::from_secs(10)).await.ok().and_then(|v| plan_usage(&v))
            } else {
                None
            };
            Ok(UsageSummary { days, models, plan })
        })
        .await
    }

    /// Stop the turn in flight.
    pub async fn interrupt(&self, session_id: String) -> Result<()> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let params = json!({ "session_id": session_id });
            inner.gateway.request("session.interrupt", params, Duration::from_secs(20)).await.map(drop)
        })
        .await
    }

    /// Release a live session the shell no longer shows.
    pub async fn close_session(&self, session_id: String) -> Result<()> {
        let inner = self.inner.clone();
        on_runtime(async move {
            let params = json!({ "session_id": session_id });
            inner.gateway.request("session.close", params, Duration::from_secs(20)).await.map(drop)
        })
        .await
    }

    /// Answer an approval request: `once`, `session`, `always` or `deny`.
    pub fn respond_approval(&self, request_id: String, choice: String) -> Result<()> {
        self.inner.gateway.respond(&request_id, json!({ "choice": choice }))
    }

    /// Answer a clarify request with one answer per question id.
    pub fn respond_clarify(&self, request_id: String, answers: std::collections::HashMap<String, String>) -> Result<()> {
        self.inner.gateway.respond(&request_id, json!({ "answers": answers }))
    }

    /// Answer a sudo / secret prompt.
    pub fn respond_input(&self, request_id: String, value: String) -> Result<()> {
        self.inner.gateway.respond(&request_id, json!({ "value": value }))
    }
}
