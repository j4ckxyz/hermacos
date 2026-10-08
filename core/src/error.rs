//! Error type shared by every exported API.

#[derive(Debug, Clone, thiserror::Error, uniffi::Error)]
pub enum HermesError {
    /// The server could not be reached (DNS, TLS, timeout, offline VPN, ...).
    #[error("{message}")]
    Network { message: String },
    /// The stored session is missing or no longer accepted; the user must sign in again.
    #[error("{message}")]
    Unauthorized { message: String },
    /// Username or password was rejected.
    #[error("{message}")]
    InvalidCredentials { message: String },
    #[error("{message}")]
    RateLimited { message: String },
    /// The dashboard answered with an unexpected HTTP status.
    #[error("{message}")]
    Server { status: u16, message: String },
    /// The gateway answered a JSON-RPC call with an error.
    #[error("{message}")]
    Rpc { code: i64, message: String },
    /// The server spoke something this client does not understand.
    #[error("{message}")]
    Protocol { message: String },
    #[error("{message}")]
    NotConnected { message: String },
    /// The server is too old (or configured in a way) this client cannot use.
    #[error("{message}")]
    Unsupported { message: String },
}

impl HermesError {
    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol { message: message.into() }
    }

    pub(crate) fn network(message: impl Into<String>) -> Self {
        Self::Network { message: message.into() }
    }

    pub(crate) fn unauthorized() -> Self {
        Self::Unauthorized { message: "Your session has expired. Sign in again.".into() }
    }

    pub(crate) fn not_connected() -> Self {
        Self::NotConnected { message: "Not connected to Hermes.".into() }
    }
}

impl From<reqwest::Error> for HermesError {
    fn from(e: reqwest::Error) -> Self {
        let host = e.url().and_then(|u| u.host_str().map(str::to_owned));
        let target = host.unwrap_or_else(|| "the server".into());
        let message = if e.is_timeout() {
            format!("{target} took too long to respond.")
        } else if e.is_connect() {
            format!("Couldn't reach {target}. Check the address and that your VPN or Tailscale is connected.")
        } else if e.is_decode() {
            format!("{target} sent a response this app couldn't read.")
        } else {
            format!("Network error talking to {target}: {e}")
        };
        Self::Network { message }
    }
}

pub type Result<T> = std::result::Result<T, HermesError>;
