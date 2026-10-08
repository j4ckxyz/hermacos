//! Dashboard REST calls: session list, search, history, rename, delete, status.

use std::sync::Arc;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use crate::auth::AuthState;
use crate::error::{HermesError, Result};
use crate::transcript;
use crate::types::{
    ChatMessage, ConnectionState, DayUsage, Listener, ModelUsage, PlatformStatus, SearchHit,
    ServerStatus, SessionSummary,
};
use crate::util::{bool_of, encode, f64_of, join, opt_str, str_of, u32_of, u64_of};

/// Newest rows fetched when a conversation is opened.
const HISTORY_LIMIT: u32 = 400;
/// Most sessions the dashboard returns per request; it answers 422 to anything larger.
const SESSION_PAGE: u32 = 100;
/// Upper bound on pages walked for servers that can only page from the oldest row.
const MAX_HISTORY_PAGES: u32 = 8;

pub(crate) struct Rest {
    auth: Arc<AuthState>,
    listener: Listener,
}

impl Rest {
    pub fn new(auth: Arc<AuthState>, listener: Listener) -> Self {
        Self { auth, listener }
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut tokens = self.auth.access().await?;
        let mut retried = false;
        loop {
            let mut request = self.auth.http.request(method.clone(), join(&self.auth.base, path));
            request = self.auth.authorize(request, &tokens);
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request.send().await?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED {
                if retried {
                    self.listener.on_connection(ConnectionState::Unauthorized);
                    return Err(HermesError::unauthorized());
                }
                retried = true;
                tokens = self.auth.refresh_after(&tokens.access_token).await?;
                continue;
            }
            let text = response.text().await?;
            let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if !status.is_success() {
                let message = error_detail(&value).unwrap_or_else(|| format!("The server answered {status}."));
                return Err(HermesError::Server { status: status.as_u16(), message });
            }
            return Ok(value);
        }
    }

    /// Up to `limit` sessions from every Hermes surface, most recently active first.
    ///
    /// The dashboard serves at most `SESSION_PAGE` rows per request and rejects larger asks
    /// outright, so longer lists are assembled from pages.
    pub async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionSummary>> {
        let mut sessions = Vec::new();
        let mut fetched = 0;
        while fetched < limit {
            let page = (limit - fetched).min(SESSION_PAGE);
            let path = format!(
                "/api/sessions?limit={page}&offset={}&min_messages=1&archived=exclude&order=recent",
                offset + fetched
            );
            let value = self.call(Method::GET, &path, None).await?;
            let rows = value.get("sessions").or(Some(&value)).and_then(Value::as_array).cloned().unwrap_or_default();
            sessions.extend(rows.iter().filter_map(session_from));
            fetched += rows.len() as u32;
            if (rows.len() as u32) < page {
                break;
            }
        }
        Ok(sessions)
    }

    pub async fn search_sessions(&self, query: &str) -> Result<Vec<SearchHit>> {
        let path = format!("/api/sessions/search?q={}", encode(query));
        let value = self.call(Method::GET, &path, None).await?;
        let rows = value.get("results").or(Some(&value)).and_then(Value::as_array);
        Ok(rows
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let session_id = str_of(row, "session_id");
                (!session_id.is_empty()).then(|| SearchHit {
                    session_id,
                    title: str_of(row, "title"),
                    snippet: str_of(row, "snippet"),
                    source: str_of(row, "source"),
                    last_active: row
                        .get("last_active")
                        .and_then(Value::as_f64)
                        .unwrap_or_else(|| f64_of(row, "session_started")),
                })
            })
            .collect())
    }

    pub async fn load_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let page = |offset: u32| {
            format!(
                "/api/sessions/{}/messages?limit={HISTORY_LIMIT}&offset={offset}&order=latest&include_compacted=true&inline_images=false",
                encode(session_id)
            )
        };
        let value = self.call(Method::GET, &page(0), None).await?;
        let rows_of = |value: &Value| -> Vec<Value> {
            value.get("messages").or(Some(value)).and_then(Value::as_array).cloned().unwrap_or_default()
        };
        let mut rows = rows_of(&value);
        // Servers that predate `order=latest` answer from the oldest row. Walk forward so a
        // long conversation still opens at its end.
        let newest_first_supported =
            value.get("pagination").and_then(|p| p.get("order")).and_then(Value::as_str) == Some("latest");
        if !newest_first_supported {
            let mut offset = HISTORY_LIMIT;
            let mut last_page = rows.len();
            while last_page as u32 == HISTORY_LIMIT && offset < HISTORY_LIMIT * MAX_HISTORY_PAGES {
                let more = rows_of(&self.call(Method::GET, &page(offset), None).await?);
                last_page = more.len();
                rows.extend(more);
                offset += HISTORY_LIMIT;
            }
        }
        transcript::sort_chronological(&mut rows);
        let keep_from = rows.len().saturating_sub(HISTORY_LIMIT as usize);
        Ok(transcript::messages_from_rows(&rows[keep_from..]))
    }

    /// The images attached to one stored user message, as `(file name, base64)`.
    ///
    /// History is read without inline images to stay light, so the bytes are fetched only
    /// when a message that carries them is rewritten.
    pub async fn row_images(&self, session_id: &str, row_id: i64) -> Result<Vec<(String, String)>> {
        let path = format!(
            "/api/sessions/{}/messages?limit={HISTORY_LIMIT}&order=latest&include_compacted=true&inline_images=true",
            encode(session_id)
        );
        let value = self.call(Method::GET, &path, None).await?;
        let rows = value.get("messages").or(Some(&value)).and_then(Value::as_array);
        let row = rows.into_iter().flatten().find(|row| transcript::durable_row_id(row) == Some(row_id));
        let parts = row.and_then(|row| row.get("content")).and_then(Value::as_array);
        let mut images = Vec::new();
        for part in parts.into_iter().flatten() {
            let url = part
                .get("image_url")
                .map(|u| u.get("url").unwrap_or(u))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some((header, payload)) = url.strip_prefix("data:image/").and_then(|r| r.split_once(";base64,")) else {
                continue;
            };
            let extension = header.split(['+', ';']).next().unwrap_or("png");
            images.push((format!("image-{}.{extension}", images.len() + 1), payload.to_owned()));
        }
        Ok(images)
    }

    /// Token and cost totals per day and per model for the last `days` days.
    pub async fn usage(&self, days: u32) -> Result<(Vec<DayUsage>, Vec<ModelUsage>)> {
        let value = self.call(Method::GET, &format!("/api/analytics/usage?days={}", days.clamp(1, 365)), None).await?;
        // The provider's own figure when it quotes one, else Hermes's price-table estimate.
        let cost = |row: &Value| {
            let actual = f64_of(row, "actual_cost");
            if actual > 0.0 { actual } else { f64_of(row, "estimated_cost") }
        };
        let rows = |key: &str| value.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        let days = rows("daily")
            .iter()
            .map(|row| DayUsage {
                day: str_of(row, "day"),
                input_tokens: u64_of(row, "input_tokens"),
                output_tokens: u64_of(row, "output_tokens"),
                cache_read_tokens: u64_of(row, "cache_read_tokens"),
                reasoning_tokens: u64_of(row, "reasoning_tokens"),
                cost_usd: cost(row),
                sessions: u32_of(row, "sessions"),
                api_calls: u32_of(row, "api_calls"),
            })
            .filter(|day| !day.day.is_empty())
            .collect();
        let models = rows("by_model")
            .iter()
            .map(|row| ModelUsage {
                model: str_of(row, "model"),
                input_tokens: u64_of(row, "input_tokens"),
                output_tokens: u64_of(row, "output_tokens"),
                cost_usd: cost(row),
                sessions: u32_of(row, "sessions"),
            })
            .filter(|model| !model.model.is_empty())
            .collect();
        Ok((days, models))
    }

    pub async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        let path = format!("/api/sessions/{}", encode(session_id));
        self.call(Method::PATCH, &path, Some(json!({ "title": title }))).await.map(drop)
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let path = format!("/api/sessions/{}", encode(session_id));
        self.call(Method::DELETE, &path, None).await.map(drop)
    }

    pub async fn status(&self) -> Result<ServerStatus> {
        let v = self.call(Method::GET, "/api/status", None).await?;
        let platforms = v
            .get("gateway_platforms")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(name, p)| PlatformStatus { name: name.clone(), state: str_of(p, "state") })
            .collect();
        let memory = v.get("memory").cloned().unwrap_or(Value::Null);
        let disk = v.get("disk").cloned().unwrap_or(Value::Null);
        Ok(ServerStatus {
            version: str_of(&v, "version"),
            release_date: str_of(&v, "release_date"),
            gateway_state: str_of(&v, "gateway_state"),
            overall: str_of(&v, "overall"),
            active_sessions: u32_of(&v, "active_sessions"),
            active_agents: u32_of(&v, "active_agents"),
            platforms,
            memory_available_mb: u32_of(&memory, "system_available_mb"),
            memory_total_mb: u32_of(&memory, "system_total_mb"),
            disk_free_mb: u32_of(&disk, "free_mb"),
            disk_total_mb: u32_of(&disk, "total_mb"),
        })
    }
}

/// The `detail` of an error body: a sentence, or FastAPI's list of validation problems.
fn error_detail(body: &Value) -> Option<String> {
    match body.get("detail")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(problems) => {
            let lines: Vec<String> = problems
                .iter()
                .map(|problem| {
                    let field = problem
                        .get("loc")
                        .and_then(Value::as_array)
                        .and_then(|loc| loc.last())
                        .map(|part| part.as_str().map(str::to_owned).unwrap_or_else(|| part.to_string()));
                    match field {
                        Some(field) => format!("{field}: {}", str_of(problem, "msg")),
                        None => str_of(problem, "msg"),
                    }
                })
                .filter(|line| !line.is_empty())
                .collect();
            (!lines.is_empty()).then(|| format!("The server rejected the request ({}).", lines.join("; ")))
        }
        Value::Object(detail) => detail.get("message").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

fn session_from(row: &Value) -> Option<SessionSummary> {
    let id = str_of(row, "id");
    if id.is_empty() {
        return None;
    }
    let started_at = f64_of(row, "started_at");
    Some(SessionSummary {
        id,
        title: str_of(row, "title"),
        preview: str_of(row, "preview"),
        source: str_of(row, "source"),
        started_at,
        last_active: row.get("last_active").and_then(Value::as_f64).unwrap_or(started_at),
        message_count: u32_of(row, "message_count"),
        is_active: bool_of(row, "is_active"),
        model: opt_str(row, "model"),
    })
}
