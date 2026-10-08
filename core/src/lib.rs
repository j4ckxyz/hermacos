//! Portable core of the Hermes client.
//!
//! Everything that is not pixels lives here so a shell on another OS only has to draw:
//! sign-in, the session list, the live gateway connection, turning half-streamed markdown into
//! render blocks, pacing the stream, and link previews. The API is exported through UniFFI.

uniffi::setup_scaffolding!();

mod auth;
mod client;
mod error;
mod gateway;
mod highlight;
pub mod markdown;
mod pacer;
mod preview;
mod rest;
mod slash;
mod transcript;
mod types;
mod util;

use std::future::Future;
use std::sync::{Arc, LazyLock};

pub use client::HermesClient;
pub use error::HermesError;
pub use markdown::{MdAlign, MdBlock, MdCell, MdCodeSpan, MdDocument, MdKind, MdRun, MdTokenKind};
pub use pacer::{StreamFrame, StreamPacer};
pub use preview::{LinkKind, LinkPreview};
pub use types::*;

use error::Result;

/// Two workers are plenty: the core is I/O bound and mostly idle.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("hermes-core")
        .enable_all()
        .build()
        .expect("failed to start the async runtime")
});

/// Run `future` on the core's runtime regardless of which executor polls the exported call.
pub(crate) async fn on_runtime<F, T>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    RUNTIME
        .spawn(future)
        .await
        .unwrap_or_else(|e| Err(HermesError::Protocol { message: format!("Internal task failed: {e}") }))
}

/// Find the Hermes dashboard behind a pasted address and learn how to sign in to it.
#[uniffi::export]
pub async fn probe_server(url: String) -> Result<ServerInfo> {
    on_runtime(async move { auth::probe(&url).await }).await
}

/// Sign in with a username and password.
#[uniffi::export]
pub async fn login_password(
    base_url: String,
    provider: String,
    username: String,
    password: String,
) -> Result<AuthTokens> {
    on_runtime(async move { auth::login_password(&base_url, &provider, &username, &password).await }).await
}

/// Sign in through the system browser (OAuth / OIDC providers).
#[uniffi::export]
pub async fn login_browser(
    base_url: String,
    provider: Option<String>,
    opener: Arc<dyn UrlOpener>,
) -> Result<AuthTokens> {
    on_runtime(async move { auth::login_browser(&base_url, provider, opener).await }).await
}

/// Credentials for a dashboard that runs without accounts (loopback / trusted network).
#[uniffi::export]
pub async fn login_open(base_url: String) -> Result<AuthTokens> {
    on_runtime(async move { auth::legacy_token(&base_url).await }).await
}

/// Parse agent markdown into render blocks. Pass `streaming` while more text is still coming.
#[uniffi::export]
pub fn parse_markdown(text: String, streaming: bool) -> MdDocument {
    markdown::parse(&text, streaming)
}

/// Title, description and image for a link.
#[uniffi::export]
pub async fn fetch_link_preview(url: String) -> Result<LinkPreview> {
    on_runtime(async move { preview::fetch(&url).await }).await
}
