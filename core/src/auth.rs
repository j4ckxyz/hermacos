//! Sign-in against a Hermes dashboard.
//!
//! The dashboard brokers an RFC 8252 native-app flow: `/auth/native/authorize` stashes our PKCE
//! challenge, the user proves who they are (password form or upstream OAuth), and the one-time
//! code is redeemed at `/auth/native/token` for bearer tokens. For password providers the whole
//! round trip runs headlessly here, so the shell only ever asks for a URL, a username and a
//! password. OAuth providers go through the system browser and a loopback listener.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::{Client, RequestBuilder, StatusCode, redirect::Policy};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

use crate::error::{HermesError, Result};
use crate::types::{
    AuthProvider, AuthTokens, ConnectionState, Listener, ServerInfo, TokenKind, UrlOpener,
};
use crate::util::{
    base64url, base_url_candidates, bool_of, display_base, encode, join, now_unix, random_token,
    str_of,
};

pub(crate) const USER_AGENT: &str = concat!("Hermacos/", env!("CARGO_PKG_VERSION"));
/// Refresh this long before the access token lapses.
const REFRESH_MARGIN_SECS: i64 = 120;
const BROWSER_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) fn http_client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(Policy::limited(4))
        .build()
        .map_err(HermesError::from)
}

fn auth_client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .cookie_store(true)
        .redirect(Policy::none())
        .build()
        .map_err(HermesError::from)
}

/// Find the dashboard behind whatever the user pasted and describe how to sign in.
pub(crate) async fn probe(input: &str) -> Result<ServerInfo> {
    let client = http_client()?;
    let mut first_error: Option<HermesError> = None;
    for candidate in base_url_candidates(input)? {
        let response = client
            .get(join(&candidate, "/api/status"))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        let status: Value = match response {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(v) => v,
                Err(_) => continue,
            },
            Ok(_) => continue,
            Err(e) => {
                first_error.get_or_insert(e.into());
                continue;
            }
        };
        if status.get("version").is_none() {
            continue;
        }
        let auth_required = bool_of(&status, "auth_required");
        let supports_native_flow = status
            .get("auth_flows")
            .and_then(Value::as_array)
            .is_some_and(|flows| flows.iter().any(|f| f == "native_pkce"));
        let mut providers = Vec::new();
        if auth_required {
            providers = fetch_providers(&client, &candidate).await;
            if providers.is_empty() {
                // Older builds only name the providers in /api/status.
                for name in status.get("auth_providers").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(name) = name.as_str() {
                        providers.push(AuthProvider {
                            name: name.to_owned(),
                            display_name: name.to_owned(),
                            supports_password: name == "basic",
                        });
                    }
                }
            }
        }
        return Ok(ServerInfo {
            base_url: display_base(&candidate),
            host: candidate.host_str().unwrap_or_default().to_owned(),
            version: str_of(&status, "version"),
            auth_required,
            providers,
            supports_native_flow,
            gateway_state: str_of(&status, "gateway_state"),
        });
    }
    Err(first_error.unwrap_or_else(|| HermesError::Unsupported {
        message: "No Hermes dashboard answered at that address. Paste the URL you open in your browser.".into(),
    }))
}

async fn fetch_providers(client: &Client, base: &Url) -> Vec<AuthProvider> {
    let Ok(response) = client.get(join(base, "/api/auth/providers")).send().await else {
        return Vec::new();
    };
    let Ok(body) = response.json::<Value>().await else { return Vec::new() };
    body.get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|p| AuthProvider {
            name: str_of(p, "name"),
            display_name: str_of(p, "display_name"),
            supports_password: bool_of(p, "supports_password"),
        })
        .filter(|p| !p.name.is_empty())
        .collect()
}

struct Pkce {
    verifier: String,
    challenge: String,
    state: String,
}

impl Pkce {
    fn new() -> Self {
        let verifier = random_token(48);
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge, state: random_token(18) }
    }

    fn authorize_url(&self, base: &Url, provider: Option<&str>, redirect_uri: &str) -> String {
        let mut url = format!(
            "{}?code_challenge={}&code_challenge_method=S256&redirect_uri={}&state={}",
            join(base, "/auth/native/authorize"),
            self.challenge,
            encode(redirect_uri),
            self.state,
        );
        if let Some(provider) = provider.filter(|p| !p.is_empty()) {
            url.push_str("&provider=");
            url.push_str(&encode(provider));
        }
        url
    }
}

fn parse_base(base_url: &str) -> Result<Url> {
    Url::parse(base_url).map_err(|_| HermesError::protocol("Invalid server address."))
}

async fn detail_of(response: reqwest::Response) -> String {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| format!("The server answered {status}."))
}

/// Username + password sign-in, start to finish, without a browser.
pub(crate) async fn login_password(
    base_url: &str,
    provider: &str,
    username: &str,
    password: &str,
) -> Result<AuthTokens> {
    let base = parse_base(base_url)?;
    let client = auth_client()?;
    let pkce = Pkce::new();
    // Never visited: the server only checks that it is a loopback literal and hands it back.
    let redirect_uri = "http://127.0.0.1:53682/callback";

    let started = client.get(pkce.authorize_url(&base, Some(provider), redirect_uri)).send().await?;
    match started.status() {
        s if s.is_redirection() => {}
        StatusCode::NOT_FOUND => {
            return Err(HermesError::Unsupported {
                message: "This Hermes version doesn't support app sign-in. Update Hermes and try again.".into(),
            });
        }
        s => {
            let message = detail_of(started).await;
            return Err(HermesError::Server { status: s.as_u16(), message });
        }
    }

    let login = client
        .post(join(&base, "/auth/password-login"))
        .json(&json!({ "provider": provider, "username": username, "password": password, "next": "" }))
        .send()
        .await?;
    match login.status() {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED => {
            return Err(HermesError::InvalidCredentials { message: "Wrong username or password.".into() });
        }
        StatusCode::TOO_MANY_REQUESTS => {
            return Err(HermesError::RateLimited {
                message: "Too many attempts. Wait a minute and try again.".into(),
            });
        }
        s => {
            let message = detail_of(login).await;
            return Err(HermesError::Server { status: s.as_u16(), message });
        }
    }
    let body: Value = login.json().await?;
    let next = str_of(&body, "next");
    let landing = Url::parse(&next)
        .ok()
        .filter(|_| next.starts_with(redirect_uri))
        .ok_or_else(|| HermesError::protocol("Sign-in didn't return an authorization code."))?;
    let (code, state) = callback_params(&landing);
    if state.as_deref() != Some(pkce.state.as_str()) {
        return Err(HermesError::protocol("Sign-in state mismatch. Try again."));
    }
    let code = code.ok_or_else(|| HermesError::protocol("Sign-in didn't return an authorization code."))?;
    redeem_code(&client, &base, &code, &pkce.verifier).await
}

fn callback_params(url: &Url) -> (Option<String>, Option<String>) {
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    (code, state)
}

async fn redeem_code(client: &Client, base: &Url, code: &str, verifier: &str) -> Result<AuthTokens> {
    let response = client
        .post(join(base, "/auth/native/token"))
        .json(&json!({ "code": code, "code_verifier": verifier }))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let message = detail_of(response).await;
        return Err(HermesError::Server { status, message });
    }
    tokens_from_payload(&response.json::<Value>().await?)
}

fn tokens_from_payload(payload: &Value) -> Result<AuthTokens> {
    let access_token = str_of(payload, "access_token");
    if access_token.is_empty() {
        return Err(HermesError::protocol("The server didn't return an access token."));
    }
    Ok(AuthTokens {
        kind: TokenKind::Bearer,
        access_token,
        refresh_token: payload
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        expires_at: payload.get("expires_at").and_then(Value::as_f64).unwrap_or(0.0) as i64,
        provider: str_of(payload, "provider"),
        user_id: str_of(payload, "user_id"),
    })
}

/// Browser sign-in for OAuth / OIDC providers: system browser + loopback redirect.
pub(crate) async fn login_browser(
    base_url: &str,
    provider: Option<String>,
    opener: Arc<dyn UrlOpener>,
) -> Result<AuthTokens> {
    let base = parse_base(base_url)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| HermesError::network(format!("Couldn't open a local port for sign-in: {e}")))?;
    let port = listener.local_addr().map_err(|e| HermesError::network(e.to_string()))?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let pkce = Pkce::new();
    opener.open_url(pkce.authorize_url(&base, provider.as_deref(), &redirect_uri));

    let (code, state) = tokio::time::timeout(BROWSER_LOGIN_TIMEOUT, accept_callback(&listener))
        .await
        .map_err(|_| HermesError::network("Sign-in timed out. Try again."))??;
    if state != pkce.state {
        return Err(HermesError::protocol("Sign-in state mismatch. Try again."));
    }
    redeem_code(&http_client()?, &base, &code, &pkce.verifier).await
}

const CALLBACK_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Signed in</title>\
<body style=\"font:16px -apple-system,system-ui,sans-serif;display:grid;place-items:center;height:100vh;margin:0\">\
<div style=\"text-align:center\"><h2 style=\"font-weight:600\">You're signed in</h2>\
<p style=\"opacity:.6\">You can close this tab and return to the app.</p></div>";

async fn accept_callback(listener: &tokio::net::TcpListener) -> Result<(String, String)> {
    loop {
        let (mut stream, _) =
            listener.accept().await.map_err(|e| HermesError::network(e.to_string()))?;
        let mut buf = vec![0u8; 8192];
        let mut len = 0;
        while len < buf.len() {
            match stream.read(&mut buf[len..]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    len += n;
                    if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
        let head = String::from_utf8_lossy(&buf[..len]);
        let target = head.split_whitespace().nth(1).unwrap_or_default();
        let parsed = Url::parse(&format!("http://127.0.0.1{target}")).ok();
        let hit = parsed.as_ref().filter(|u| u.path() == "/callback");
        let (status, body) = if hit.is_some() { ("200 OK", CALLBACK_PAGE) } else { ("404 Not Found", "") };
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(reply.as_bytes()).await;
        let _ = stream.shutdown().await;
        let Some(url) = hit else { continue };
        if let Some((_, error)) = url.query_pairs().find(|(k, _)| k == "error") {
            return Err(HermesError::protocol(format!("Sign-in was refused: {error}")));
        }
        if let (Some(code), Some(state)) = callback_params(url) {
            return Ok((code, state));
        }
    }
}

/// Dashboards bound to loopback (or started `--insecure`) run without accounts and inject a
/// process-lifetime token into the page; that token is the credential.
pub(crate) async fn legacy_token(base_url: &str) -> Result<AuthTokens> {
    let base = parse_base(base_url)?;
    let page = http_client()?.get(join(&base, "/")).send().await?.text().await?;
    let token = page
        .split_once("__HERMES_SESSION_TOKEN__")
        .and_then(|(_, rest)| rest.split_once('"'))
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(token, _)| token.to_owned())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| HermesError::protocol("This dashboard didn't provide a session token."))?;
    Ok(AuthTokens {
        kind: TokenKind::Legacy,
        access_token: token,
        refresh_token: None,
        expires_at: 0,
        provider: "local".into(),
        user_id: "local".into(),
    })
}

async fn refresh(client: &Client, base: &Url, tokens: &AuthTokens) -> Result<AuthTokens> {
    if tokens.kind == TokenKind::Legacy {
        return legacy_token(&display_base(base)).await;
    }
    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
        return Err(HermesError::unauthorized());
    };
    let response = client
        .post(join(base, "/auth/native/refresh"))
        .json(&json!({ "refresh_token": refresh_token, "provider": tokens.provider }))
        .send()
        .await?;
    match response.status() {
        s if s.is_success() => {
            let mut fresh = tokens_from_payload(&response.json::<Value>().await?)?;
            if fresh.refresh_token.is_none() {
                fresh.refresh_token = tokens.refresh_token.clone();
            }
            Ok(fresh)
        }
        StatusCode::UNAUTHORIZED | StatusCode::BAD_REQUEST => Err(HermesError::unauthorized()),
        s => {
            let message = detail_of(response).await;
            Err(HermesError::Server { status: s.as_u16(), message })
        }
    }
}

/// Holds the current credentials and refreshes them once, no matter how many callers ask.
pub(crate) struct AuthState {
    pub base: Url,
    pub http: Client,
    tokens: Mutex<Option<AuthTokens>>,
    refresh_gate: tokio::sync::Mutex<()>,
    listener: Listener,
}

impl AuthState {
    pub fn new(base: Url, http: Client, tokens: Option<AuthTokens>, listener: Listener) -> Self {
        Self { base, http, tokens: Mutex::new(tokens), refresh_gate: tokio::sync::Mutex::new(()), listener }
    }

    fn current(&self) -> Option<AuthTokens> {
        self.tokens.lock().unwrap().clone()
    }

    /// Valid credentials, refreshed first when they are about to lapse.
    pub async fn access(&self) -> Result<AuthTokens> {
        let tokens = self.current().ok_or_else(HermesError::unauthorized)?;
        let expiring = tokens.expires_at > 0 && tokens.expires_at - now_unix() < REFRESH_MARGIN_SECS;
        if expiring && tokens.refresh_token.is_some() {
            return self.refresh_after(&tokens.access_token).await;
        }
        Ok(tokens)
    }

    /// Replace credentials the server just rejected. `stale` is the access token that failed;
    /// if another caller already rotated it, the fresh set is returned without a second refresh.
    pub async fn refresh_after(&self, stale: &str) -> Result<AuthTokens> {
        let _gate = self.refresh_gate.lock().await;
        let tokens = self.current().ok_or_else(HermesError::unauthorized)?;
        if tokens.access_token != stale {
            return Ok(tokens);
        }
        match refresh(&self.http, &self.base, &tokens).await {
            Ok(fresh) => {
                *self.tokens.lock().unwrap() = Some(fresh.clone());
                self.listener.on_tokens(fresh.clone());
                Ok(fresh)
            }
            Err(e @ HermesError::Unauthorized { .. }) => {
                self.listener.on_connection(ConnectionState::Unauthorized);
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// A single-use ticket for the WebSocket upgrade. Every gated server version accepts a
    /// ticket; only newer ones also accept the access token itself, so the ticket is tried
    /// first. `Ok(None)` means this server has no ticket endpoint.
    pub async fn socket_ticket(&self, tokens: &AuthTokens) -> Result<Option<String>> {
        if tokens.kind == TokenKind::Legacy {
            return Ok(None);
        }
        let request = self.http.post(join(&self.base, "/api/auth/ws-ticket"));
        let response = self.authorize(request, tokens).send().await?;
        match response.status() {
            StatusCode::UNAUTHORIZED => Err(HermesError::unauthorized()),
            s if s.is_success() => {
                let body: Value = response.json().await.unwrap_or(Value::Null);
                Ok(body.get("ticket").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_owned))
            }
            _ => Ok(None),
        }
    }

    pub fn authorize(&self, request: RequestBuilder, tokens: &AuthTokens) -> RequestBuilder {
        let request = request.bearer_auth(&tokens.access_token);
        match tokens.kind {
            TokenKind::Bearer => request,
            TokenKind::Legacy => request.header("X-Hermes-Session-Token", &tokens.access_token),
        }
    }
}
