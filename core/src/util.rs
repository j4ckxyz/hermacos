//! Small helpers: randomness, base64url, JSON accessors, URL shaping.

use serde_json::Value;
use url::Url;

use crate::error::{HermesError, Result};

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// RFC 4648 base64url without padding.
pub(crate) fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = match chunk.len() {
            3 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[2] as u32,
            2 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8,
            _ => (chunk[0] as u32) << 16,
        };
        out.push(B64URL[(n >> 18) as usize & 63] as char);
        out.push(B64URL[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64URL[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[n as usize & 63] as char);
        }
    }
    out
}

/// RFC 4648 base64 with padding (payloads of the attach RPCs).
pub(crate) fn base64_standard(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub(crate) fn u64_of(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_f64).map(|n| n.max(0.0) as u64).unwrap_or(0)
}

pub(crate) fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("OS random source unavailable");
    base64url(&buf)
}

pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

pub(crate) fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_owned)
}

pub(crate) fn f64_of(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

pub(crate) fn u32_of(v: &Value, key: &str) -> u32 {
    v.get(key).and_then(Value::as_f64).map(|n| n.max(0.0) as u32).unwrap_or(0)
}

pub(crate) fn bool_of(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Turn whatever the user pasted into candidate base URLs, most specific first.
///
/// People paste the address bar: `host`, `https://host/`, `https://host/login?next=%2F`,
/// `https://host/hermes/chat`. The dashboard may also be mounted under a path prefix, so every
/// leading slice of the path is a candidate and the caller probes them in order.
pub(crate) fn base_url_candidates(input: &str) -> Result<Vec<Url>> {
    let trimmed = input.trim().trim_matches(|c| c == '<' || c == '>' || c == '"' || c == '\'');
    if trimmed.is_empty() {
        return Err(HermesError::protocol("Enter the address of your Hermes dashboard."));
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        let host = trimmed.split(['/', ':']).next().unwrap_or_default();
        let local = host == "localhost"
            || host.parse::<std::net::Ipv4Addr>().is_ok()
            || host.ends_with(".local");
        format!("{}://{trimmed}", if local { "http" } else { "https" })
    };
    let parsed = Url::parse(&with_scheme)
        .map_err(|_| HermesError::protocol("That doesn't look like a web address."))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(HermesError::protocol("Use an http:// or https:// address."));
    }
    let mut origin = parsed.clone();
    origin.set_path("");
    origin.set_query(None);
    origin.set_fragment(None);

    let segments: Vec<&str> = parsed.path().split('/').filter(|s| !s.is_empty()).take(3).collect();
    let mut out = Vec::new();
    for n in (0..=segments.len()).rev() {
        let mut candidate = origin.clone();
        candidate.set_path(&segments[..n].join("/"));
        out.push(candidate);
    }
    Ok(out)
}

/// `base` + `path` where `path` starts with `/`; keeps any mount prefix in `base`.
pub(crate) fn join(base: &Url, path: &str) -> String {
    format!("{}{}", base.as_str().trim_end_matches('/'), path)
}

pub(crate) fn display_base(base: &Url) -> String {
    base.as_str().trim_end_matches('/').to_owned()
}

/// Percent-encode a path segment or query value.
pub(crate) fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// `web_search` -> `Web search`.
pub(crate) fn humanize(name: &str) -> String {
    let spaced = name.replace(['_', '-', '.'], " ");
    let mut chars = spaced.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// First line, trimmed, capped at `max` characters.
pub(crate) fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    if line.chars().count() <= max {
        line.to_owned()
    } else {
        let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_rfc_vectors() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn base64_standard_matches_rfc_vectors() {
        assert_eq!(base64_standard(b""), "");
        assert_eq!(base64_standard(b"f"), "Zg==");
        assert_eq!(base64_standard(b"fo"), "Zm8=");
        assert_eq!(base64_standard(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_standard(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn candidates_cover_pasted_shapes() {
        let urls = |s: &str| -> Vec<String> {
            base_url_candidates(s).unwrap().iter().map(display_base).collect()
        };
        assert_eq!(urls("hermes-vps.example.ts.net"), ["https://hermes-vps.example.ts.net"]);
        assert_eq!(
            urls("https://h.ts.net/login?next=%2F"),
            ["https://h.ts.net/login", "https://h.ts.net"]
        );
        assert_eq!(urls("localhost:9119"), ["http://localhost:9119"]);
        assert_eq!(urls("192.168.1.4:9119/chat")[1], "http://192.168.1.4:9119");
        assert!(base_url_candidates("  ").is_err());
        assert!(base_url_candidates("ftp://x").is_err());
    }

    #[test]
    fn humanize_tool_names() {
        assert_eq!(humanize("web_search"), "Web search");
        assert_eq!(one_line("\n  hello world  \nmore", 5), "hell…");
    }
}
