//! Link previews: fetch a page's head and read its Open Graph / Twitter / plain meta tags.
//!
//! Only the first part of the document is downloaded and it is scanned by hand, so a preview
//! costs one small request and no HTML parser.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use url::Url;

use crate::error::{HermesError, Result};

const MAX_HEAD_BYTES: usize = 400 * 1024;
const CACHE_LIMIT: usize = 256;
/// Sites serve preview tags to browsers; identify as one.
const PREVIEW_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
(KHTML, like Gecko) Version/18.0 Safari/605.1.15 Hermacos/0.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LinkKind {
    Page,
    Image,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct LinkPreview {
    pub url: String,
    pub kind: LinkKind,
    pub title: String,
    pub description: Option<String>,
    pub site_name: String,
    pub image_url: Option<String>,
    pub icon_url: Option<String>,
}

static CACHE: LazyLock<Mutex<HashMap<String, LinkPreview>>> = LazyLock::new(Default::default);

static CLIENT: LazyLock<Option<reqwest::Client>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(PREVIEW_UA)
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(6))
        .build()
        .ok()
});

pub(crate) async fn fetch(raw_url: &str) -> Result<LinkPreview> {
    if let Some(hit) = CACHE.lock().unwrap().get(raw_url) {
        return Ok(hit.clone());
    }
    let url = Url::parse(raw_url).map_err(|_| HermesError::protocol("Not a valid link."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HermesError::protocol("Only web links have previews."));
    }
    let client = CLIENT.as_ref().ok_or_else(|| HermesError::network("HTTP client unavailable."))?;
    let mut response = client
        .get(url.clone())
        .header("Accept", "text/html,application/xhtml+xml,image/*;q=0.9,*/*;q=0.5")
        .header("Accept-Language", "en")
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(HermesError::Server {
            status: response.status().as_u16(),
            message: "The link didn't load.".into(),
        });
    }
    let final_url = response.url().clone();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let site = site_name(&final_url);

    let preview = if content_type.starts_with("image/") {
        LinkPreview {
            url: raw_url.to_owned(),
            kind: LinkKind::Image,
            title: final_url.path_segments().and_then(|mut s| s.next_back()).unwrap_or_default().to_owned(),
            description: None,
            site_name: site,
            image_url: Some(final_url.to_string()),
            icon_url: None,
        }
    } else if content_type.contains("html") || content_type.is_empty() {
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() >= MAX_HEAD_BYTES || contains(&body, b"</head>") {
                break;
            }
        }
        from_html(raw_url, &final_url, &String::from_utf8_lossy(&body))
    } else {
        LinkPreview {
            url: raw_url.to_owned(),
            kind: LinkKind::Page,
            title: final_url.path_segments().and_then(|mut s| s.next_back()).filter(|s| !s.is_empty()).unwrap_or(&site).to_owned(),
            description: None,
            site_name: site,
            image_url: None,
            icon_url: None,
        }
    };

    let mut cache = CACHE.lock().unwrap();
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(raw_url.to_owned(), preview.clone());
    Ok(preview)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

fn site_name(url: &Url) -> String {
    url.host_str().unwrap_or_default().trim_start_matches("www.").to_owned()
}

fn from_html(raw_url: &str, base: &Url, html: &str) -> LinkPreview {
    let mut meta: HashMap<String, String> = HashMap::new();
    let mut icon: Option<(u32, String)> = None;
    for tag in tags(html, "meta") {
        let attrs = attributes(tag);
        let key = attrs.get("property").or_else(|| attrs.get("name")).map(|k| k.to_ascii_lowercase());
        if let (Some(key), Some(content)) = (key, attrs.get("content")) {
            if !content.trim().is_empty() {
                meta.entry(key).or_insert_with(|| decode_entities(content.trim()));
            }
        }
    }
    for tag in tags(html, "link") {
        let attrs = attributes(tag);
        let rel = attrs.get("rel").map(|r| r.to_ascii_lowercase()).unwrap_or_default();
        let Some(href) = attrs.get("href").filter(|h| !h.is_empty()) else { continue };
        let rank = if rel.contains("apple-touch-icon") {
            3
        } else if rel.split_whitespace().any(|r| r == "icon") {
            if href.ends_with(".svg") { 1 } else { 2 }
        } else {
            continue;
        };
        if icon.as_ref().is_none_or(|(best, _)| rank > *best) {
            icon = Some((rank, decode_entities(href)));
        }
    }
    let title_tag = html
        .to_ascii_lowercase()
        .find("<title")
        .and_then(|start| html[start..].find('>').map(|gt| start + gt + 1))
        .and_then(|from| html[from..].find("</").map(|end| decode_entities(html[from..from + end].trim())));

    let pick = |keys: &[&str]| keys.iter().find_map(|k| meta.get(*k).cloned()).filter(|v| !v.is_empty());
    let resolve = |href: String| base.join(&href).ok().map(|u| u.to_string());
    let site = pick(&["og:site_name", "application-name"]).unwrap_or_else(|| site_name(base));
    let title = pick(&["og:title", "twitter:title"])
        .or(title_tag)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| site.clone());
    LinkPreview {
        url: raw_url.to_owned(),
        kind: LinkKind::Page,
        title: collapse(&title, 160),
        description: pick(&["og:description", "twitter:description", "description"]).map(|d| collapse(&d, 280)),
        site_name: collapse(&site, 60),
        image_url: pick(&["og:image:secure_url", "og:image", "og:image:url", "twitter:image", "twitter:image:src"])
            .and_then(resolve),
        icon_url: icon.map(|(_, href)| href).and_then(resolve).or_else(|| resolve("/favicon.ico".into())),
    }
}

/// Every `<name ...>` opening tag in `html` (case-insensitive), without the angle brackets.
fn tags<'a>(html: &'a str, name: &str) -> Vec<&'a str> {
    let lower = html.to_ascii_lowercase();
    let needle = format!("<{name}");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find(&needle) {
        let start = from + i + needle.len();
        let boundary = lower[start..].chars().next().is_some_and(|c| c.is_whitespace() || c == '/');
        let Some(end) = lower[start..].find('>') else { break };
        if boundary {
            out.push(&html[start..start + end]);
        }
        from = start + end;
    }
    out
}

/// Parse `key="value" other='v' bare=v` attribute text.
fn attributes(tag: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let chars: Vec<char> = tag.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == '/') {
            i += 1;
        }
        let key_start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '=' {
            i += 1;
        }
        let key: String = chars[key_start..i].iter().collect::<String>().to_ascii_lowercase();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            continue;
        }
        i += 1;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let value: String = match chars.get(i) {
            Some(q @ ('"' | '\'')) => {
                let start = i + 1;
                let end = chars[start..].iter().position(|c| c == q).map_or(chars.len(), |p| start + p);
                i = end + 1;
                chars[start..end].iter().collect()
            }
            _ => {
                let start = i;
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                chars[start..i].iter().collect()
            }
        };
        if !key.is_empty() {
            out.entry(key).or_insert(value);
        }
    }
    out
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let Some(semi) = tail.find(';').filter(|s| *s <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "ndash" => Some('–'),
            "mdash" => Some('—'),
            "hellip" => Some('…'),
            "rsquo" => Some('’'),
            "lsquo" => Some('‘'),
            "rdquo" => Some('”'),
            "ldquo" => Some('“'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn collapse(text: &str, max: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= max {
        joined
    } else {
        let mut cut: String = joined.chars().take(max - 1).collect();
        cut.push('…');
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_open_graph_tags() {
        let html = r#"<!doctype html><html><head>
            <title>Fallback &amp; title</title>
            <meta property="og:title" content="Hermes Agent &#8212; docs">
            <meta name='description' content='Plain description'>
            <meta property="og:description" content="  The agent   that grows with you. ">
            <meta property=og:image content=/img/card.png>
            <META PROPERTY="og:site_name" CONTENT="Nous Research">
            <link rel="icon" href="/favicon.svg"><link rel="apple-touch-icon" href="/touch.png">
            </head><body>"#;
        let base = Url::parse("https://docs.example.com/guide/intro").unwrap();
        let p = from_html("https://docs.example.com/guide/intro", &base, html);
        assert_eq!(p.title, "Hermes Agent — docs");
        assert_eq!(p.description.as_deref(), Some("The agent that grows with you."));
        assert_eq!(p.site_name, "Nous Research");
        assert_eq!(p.image_url.as_deref(), Some("https://docs.example.com/img/card.png"));
        assert_eq!(p.icon_url.as_deref(), Some("https://docs.example.com/touch.png"));
    }

    #[test]
    fn falls_back_to_title_and_host() {
        let base = Url::parse("https://www.example.com/a").unwrap();
        let p = from_html("https://www.example.com/a", &base, "<head><title> Hi   there </title></head>");
        assert_eq!(p.title, "Hi there");
        assert_eq!(p.site_name, "example.com");
        assert_eq!(p.icon_url.as_deref(), Some("https://www.example.com/favicon.ico"));
        assert_eq!(from_html("u", &base, "").title, "example.com");
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#39;q&#x27; &unknown; &"), "a & b <c> 'q' &unknown; &");
    }
}
