//! Markdown -> a flat list of render blocks.
//!
//! The shell draws native views, so it wants structure, not HTML: paragraphs made of styled
//! runs, code blocks, tables, images. Nesting (lists, quotes) is flattened into per-block
//! `indent` / `quote` / `marker` so a renderer is one loop with no recursion.
//!
//! Agent output arrives a few characters at a time and is often half-written markdown: an
//! unclosed `**`, a table whose separator row has not arrived, a link missing its `)`.
//! `repair_streaming` rewrites only the unfinished tail so the text on screen never flashes raw
//! syntax and never jumps between layouts.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::highlight;

#[derive(Debug, Clone, PartialEq, Default, uniffi::Record)]
pub struct MdRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
    /// A reference marker such as `[2]`: `text` is the number and `link` its source.
    pub citation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MdTokenKind {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
    Type,
    Function,
    Property,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MdCodeSpan {
    pub text: String,
    pub kind: MdTokenKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MdAlign {
    Leading,
    Center,
    Trailing,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MdCell {
    pub runs: Vec<MdRun>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum MdKind {
    Paragraph { runs: Vec<MdRun> },
    Heading { level: u8, runs: Vec<MdRun> },
    Code { language: Option<String>, code: String, spans: Vec<MdCodeSpan>, closed: bool },
    Table { alignments: Vec<MdAlign>, header: Vec<MdCell>, rows: Vec<Vec<MdCell>> },
    Image { url: String, alt: String },
    Rule,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MdBlock {
    /// Stable while the block grows during streaming: position + kind.
    pub id: String,
    pub kind: MdKind,
    /// List nesting depth (0 = not in a list).
    pub indent: u8,
    /// Block-quote nesting depth.
    pub quote: u8,
    /// List marker for the first block of an item: `•`, `◦`, `3.`, `[ ]`, `[x]`.
    pub marker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, uniffi::Record)]
pub struct MdDocument {
    pub blocks: Vec<MdBlock>,
    /// Distinct http(s) links in reading order, for preview cards.
    pub links: Vec<String>,
}

/// Parse agent markdown. `streaming` means more text is still coming, so the unfinished tail is
/// repaired before parsing.
pub fn parse(text: &str, streaming: bool) -> MdDocument {
    let normalized = normalize(text, streaming);
    let (source, open_fence) = if streaming {
        repair_streaming(&normalized)
    } else {
        (normalized, false)
    };
    let mut doc = Builder::default().run(&source);
    link_citations(&mut doc);
    if open_fence {
        if let Some(MdBlock { kind: MdKind::Code { closed, .. }, .. }) = doc.blocks.last_mut() {
            *closed = false;
        }
    }
    doc
}

// ───────────────────────── citations ─────────────────────────

/// `[12]` -> 12, for a marker of up to three digits and nothing else.
fn citation_number(marker: &str) -> Option<u32> {
    let digits = marker.strip_prefix('[')?.strip_suffix(']')?;
    (!digits.is_empty() && digits.len() <= 3 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// Turn numbered reference markers into links to their sources.
///
/// Agents cite as `…as reported.[2]` and close with a list such as `[2] [Forbes review](url)`.
/// The list gives each number a destination; every `[n]` elsewhere in the text then becomes a
/// citation run pointing at it, so a reader can go from a claim to its source in one click.
fn link_citations(doc: &mut MdDocument) {
    let mut sources: Vec<(u32, String)> = Vec::new();
    let mut note = |number: u32, url: &str| {
        if !sources.iter().any(|(n, _)| *n == number) {
            sources.push((number, url.to_owned()));
        }
    };
    let mut in_sources_section = false;
    for block in &doc.blocks {
        match &block.kind {
            MdKind::Heading { runs, .. } => {
                let title: String = runs.iter().map(|r| r.text.to_lowercase()).collect();
                in_sources_section = ["source", "reference", "citation", "further reading", "links"]
                    .iter()
                    .any(|word| title.contains(word));
            }
            MdKind::Paragraph { runs } => {
                // `[n] <link>` at the start of a line.
                for (i, run) in runs.iter().enumerate().skip(1) {
                    let (Some(url), false) = (&run.link, run.code) else { continue };
                    let before = &runs[i - 1];
                    if before.link.is_some() || before.code {
                        continue;
                    }
                    let line_start = before.text.rsplit('\n').next().unwrap_or_default().trim();
                    let marker = line_start.trim_end_matches([':', '-', '–', '—', ' ']);
                    if let Some(number) = citation_number(marker) {
                        note(number, url);
                    }
                }
                // `3. <link>` in a numbered list under a "Sources" heading.
                let ordinal = block.marker.as_deref().and_then(|m| m.strip_suffix('.')).and_then(|m| m.parse::<u32>().ok());
                if let (true, Some(number)) = (in_sources_section, ordinal) {
                    if let Some(url) = runs.iter().find_map(|r| r.link.as_deref().filter(|_| !r.code)) {
                        note(number, url);
                    }
                }
            }
            _ => {}
        }
    }
    // `[1]: url` definitions make the parser resolve `[1]` on its own, as a link whose text
    // is the bare number; those are citations as well.
    let numeric_link = |run: &mut MdRun| {
        let numeric = !run.text.is_empty() && run.text.len() <= 3 && run.text.bytes().all(|b| b.is_ascii_digit());
        if numeric && run.link.is_some() && !run.code {
            run.citation = true;
        }
    };
    for block in &mut doc.blocks {
        match &mut block.kind {
            MdKind::Paragraph { runs } | MdKind::Heading { runs, .. } => runs.iter_mut().for_each(numeric_link),
            MdKind::Table { header, rows, .. } => header
                .iter_mut()
                .chain(rows.iter_mut().flatten())
                .flat_map(|cell| cell.runs.iter_mut())
                .for_each(numeric_link),
            _ => {}
        }
    }
    if sources.is_empty() {
        return;
    }
    let lookup = |number: u32| sources.iter().find(|(n, _)| *n == number).map(|(_, url)| url.clone());
    let rewrite = |runs: &mut Vec<MdRun>| {
        if !runs.iter().any(|r| r.link.is_none() && !r.code && r.text.contains('[')) {
            return;
        }
        let mut out: Vec<MdRun> = Vec::with_capacity(runs.len() + 4);
        for run in runs.drain(..) {
            if run.link.is_some() || run.code || !run.text.contains('[') {
                out.push(run);
                continue;
            }
            let mut rest = run.text.as_str();
            let mut plain = String::new();
            while let Some(open) = rest.find('[') {
                let close = rest[open..].find(']').map(|c| open + c);
                // `[1]`, or a group such as `[1, 3]`: every number must be a known source.
                let urls: Option<Vec<(String, String)>> = close.and_then(|close| {
                    rest[open + 1..close]
                        .split(',')
                        .map(|part| {
                            let part = part.trim();
                            citation_number(&format!("[{part}]")).and_then(&lookup).map(|url| (part.to_owned(), url))
                        })
                        .collect()
                });
                match (urls, close) {
                    (Some(urls), Some(close)) if !urls.is_empty() => {
                        plain.push_str(&rest[..open]);
                        if !plain.is_empty() {
                            out.push(MdRun { text: std::mem::take(&mut plain), ..run.clone() });
                        }
                        for (number, url) in urls {
                            out.push(MdRun { text: number, link: Some(url), citation: true, ..MdRun::default() });
                        }
                        rest = &rest[close + 1..];
                    }
                    _ => {
                        plain.push_str(&rest[..=open]);
                        rest = &rest[open + 1..];
                    }
                }
            }
            plain.push_str(rest);
            if !plain.is_empty() {
                out.push(MdRun { text: plain, ..run });
            }
        }
        *runs = out;
    };
    for block in &mut doc.blocks {
        match &mut block.kind {
            MdKind::Paragraph { runs } | MdKind::Heading { runs, .. } => rewrite(runs),
            MdKind::Table { header, rows, .. } => {
                header.iter_mut().chain(rows.iter_mut().flatten()).for_each(|cell| rewrite(&mut cell.runs));
            }
            _ => {}
        }
    }
}

// ───────────────────────── normalisation ─────────────────────────

pub(crate) fn strip_ansi(text: &str) -> String {
    if !text.contains('\u{1b}') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI: ESC [ params final-byte
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... BEL | ESC \
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

/// Remove `<think>…</think>` scaffolding some models leak into the reply. While streaming, an
/// unclosed block hides everything after its opening tag.
fn strip_think(text: &str, streaming: bool) -> String {
    const OPEN: [&str; 2] = ["<think>", "<thinking>"];
    const CLOSE: [&str; 2] = ["</think>", "</thinking>"];
    if !text.contains("<think") {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some((start, tag)) = OPEN.iter().filter_map(|t| rest.find(t).map(|i| (i, *t))).min() else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let after = &rest[start + tag.len()..];
        match CLOSE.iter().filter_map(|t| after.find(t).map(|i| (i, t.len()))).min() {
            Some((end, len)) => rest = after[end + len..].trim_start_matches('\n'),
            None => {
                if !streaming {
                    out.push_str(after);
                }
                break;
            }
        }
    }
    out
}

struct Fence {
    ch: char,
    len: usize,
}

/// A fence marker on this line: `(char, run length, rest after the run)`.
fn fence_marker(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_start();
    let ch = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = trimmed.chars().take_while(|c| *c == ch).count();
    (len >= 3).then(|| (ch, len, &trimmed[len..]))
}

/// Track fenced-code state across lines. Returns true when `line` is inside or delimits a fence.
fn step_fence(open: &mut Option<Fence>, line: &str) -> bool {
    let marker = fence_marker(line);
    match (&*open, marker) {
        (None, Some((ch, len, rest))) => {
            // A backtick fence's info string cannot contain backticks (that is inline code).
            if ch == '`' && rest.contains('`') {
                return false;
            }
            *open = Some(Fence { ch, len });
            true
        }
        (Some(fence), Some((ch, len, rest))) => {
            if ch == fence.ch && len >= fence.len && rest.trim().is_empty() {
                *open = None;
            }
            true
        }
        (Some(_), None) => true,
        (None, None) => false,
    }
}

fn is_delimiter_row(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('-') || !t.contains('|') && !t.contains(':') {
        return false;
    }
    t.trim_matches('|').split('|').all(|cell| {
        let c = cell.trim();
        let dashes = c.trim_start_matches(':').trim_end_matches(':');
        !dashes.is_empty() && dashes.chars().all(|ch| ch == '-')
    })
}

fn cell_count(line: &str) -> usize {
    line.trim().trim_matches('|').split('|').count()
}

/// Fix up habits of model output that CommonMark reads differently from how they were meant.
fn normalize(text: &str, streaming: bool) -> String {
    let cleaned = strip_think(&strip_ansi(text), streaming);
    let cleaned = cleaned.replace("\r\n", "\n").replace(['\r', '\u{feff}', '\0'], "");
    let lines: Vec<&str> = cleaned.split('\n').collect();
    let mut out = String::with_capacity(cleaned.len() + 16);
    let mut fence: Option<Fence> = None;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if step_fence(&mut fence, line) {
            out.push_str(line);
            continue;
        }
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        // `##Title` -> `## Title`
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (2..=6).contains(&hashes) {
            let rest = &trimmed[hashes..];
            if rest.chars().next().is_some_and(|c| !c.is_whitespace() && c != '#') {
                out.push_str(indent);
                out.push_str(&trimmed[..hashes]);
                out.push(' ');
                out.push_str(rest);
                continue;
            }
        }
        // Typographic bullets -> list items.
        if let Some(rest) = ["• ", "● ", "◦ ", "▪ ", "– ", "— "].iter().find_map(|b| trimmed.strip_prefix(b)) {
            if !trimmed.starts_with(['–', '—']) || i == 0 || lines[i - 1].trim().is_empty()
                || lines[i - 1].trim_start().starts_with(['–', '—', '•'])
            {
                out.push_str(indent);
                out.push_str("- ");
                out.push_str(rest);
                continue;
            }
        }
        // A table directly under a paragraph needs a blank line to start.
        if trimmed.starts_with('|')
            && i > 0
            && !lines[i - 1].trim().is_empty()
            && !lines[i - 1].trim_start().starts_with('|')
            && lines.get(i + 1).is_some_and(|next| is_delimiter_row(next))
        {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

// ───────────────────────── streaming repair ─────────────────────────

/// Rewrite the unfinished tail of streaming markdown. Returns the text to parse and whether it
/// ends inside an open code fence.
pub fn repair_streaming(text: &str) -> (String, bool) {
    let mut fence: Option<Fence> = None;
    for line in text.split('\n') {
        step_fence(&mut fence, line);
    }
    let (head, last) = match text.rfind('\n') {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => ("", text),
    };
    let last_trim = last.trim();

    if let Some(open) = &fence {
        // Closing fence being typed: don't show `` as code for a frame.
        let partial_close = !last_trim.is_empty()
            && last_trim.len() < open.len
            && last_trim.chars().all(|c| c == open.ch)
            && !head.is_empty();
        return (if partial_close { head.to_owned() } else { text.to_owned() }, true);
    }
    if let Some(held) = hold_unconfirmed_table(text) {
        return (held, false);
    }
    if last_trim.is_empty() {
        return (text.to_owned(), false);
    }

    let prev = head.trim_end_matches('\n').rsplit('\n').next().unwrap_or("");
    let prev_blank = head.is_empty() || head.ends_with("\n\n") || prev.trim().is_empty();
    let all = |pred: fn(char) -> bool| last_trim.chars().all(pred);

    // A fence that has only one or two of its three characters.
    if last_trim.len() < 3 && (all(|c| c == '`') || all(|c| c == '~')) {
        return (head.to_owned(), false);
    }
    // `-` / `=` under text would turn the paragraph into a heading; alone it may become a rule.
    if all(|c| c == '-') || all(|c| c == '=') {
        if !prev_blank || (last_trim.len() < 3 && last_trim.starts_with('-')) {
            return (head.to_owned(), false);
        }
    }
    if is_bare_marker(last_trim) {
        return (head.to_owned(), false);
    }
    if last.trim_start().starts_with('|') {
        // A row of a confirmed table: the parser fills cells in as they arrive. A lone pipe
        // is not a row yet and would end the table.
        let lone_pipe = last_trim.chars().all(|c| c == '|');
        return (if lone_pipe { head.to_owned() } else { text.to_owned() }, false);
    }

    let (prefix, content) = split_prefix(last);
    let mut out = String::with_capacity(text.len() + 8);
    out.push_str(head);
    out.push_str(prefix);
    out.push_str(&repair_inline(content));
    (out, false)
}

/// Tables: hold the header row back until the separator row proves it is a table, so a
/// half-arrived table never shows as a paragraph of pipes. Returns the text to show instead.
fn hold_unconfirmed_table(text: &str) -> Option<String> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() || body.ends_with('\n') {
        return None;
    }
    // The trailing run of lines that all start with a pipe.
    let rows: Vec<&str> = body.rsplit('\n').take_while(|l| l.trim_start().starts_with('|')).collect();
    let header = *rows.last()?;
    let rows_len: usize = rows.iter().map(|l| l.len() + 1).sum::<usize>() - 1;
    let before = &body[..body.len() - rows_len];
    match rows.len() {
        1 => Some(before.to_owned()),
        n => {
            let delimiter = rows[n - 2];
            let confirmed = is_delimiter_row(delimiter) && cell_count(delimiter) == cell_count(header);
            let still_typing = n == 2 && delimiter.chars().all(|c| matches!(c, '|' | ':' | '-' | ' '));
            (!confirmed && still_typing).then(|| before.to_owned())
        }
    }
}

/// A line that is only a block marker so far: `#`, `-`, `3.`, `>`, `- [ ]`.
fn is_bare_marker(line: &str) -> bool {
    let t = line.trim();
    if t.chars().all(|c| c == '#') && t.len() <= 6 {
        return true;
    }
    if t.chars().all(|c| c == '>' || c == ' ') {
        return true;
    }
    let rest = t.trim_start_matches(['>', ' ']);
    let after_bullet = rest
        .strip_prefix(['-', '*', '+'])
        .or_else(|| {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            (digits > 0 && digits <= 9).then(|| rest[digits..].strip_prefix(['.', ')'])).flatten()
        });
    match after_bullet {
        Some(tail) => {
            let tail = tail.trim();
            tail.is_empty() || matches!(tail, "[" | "[ " | "[x" | "[X" | "[ ]" | "[x]" | "[X]" | "[]")
        }
        None => false,
    }
}

/// Split a line into its block prefix (quote markers, list marker, heading hashes) and content.
fn split_prefix(line: &str) -> (&str, &str) {
    let mut i = 0;
    let bytes = line.as_bytes();
    let skip_ws = |mut i: usize| {
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        i
    };
    i = skip_ws(i);
    while i < bytes.len() && bytes[i] == b'>' {
        i = skip_ws(i + 1);
    }
    let rest = &line[i..];
    if let Some(after) = rest.strip_prefix(['-', '*', '+']).filter(|a| a.starts_with(' ')) {
        i = skip_ws(line.len() - after.len());
        for task in ["[ ] ", "[x] ", "[X] "] {
            if line[i..].starts_with(task) {
                i += task.len();
            }
        }
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && rest[digits..].starts_with(['.', ')']) && rest[digits + 1..].starts_with(' ') {
            i = skip_ws(i + digits + 1);
        } else {
            let hashes = rest.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&hashes) && rest[hashes..].starts_with(' ') {
                i = skip_ws(i + hashes);
            }
        }
    }
    (&line[..i], &line[i..])
}

fn run_len(chars: &[char], at: usize) -> usize {
    chars[at..].iter().take_while(|c| **c == chars[at]).count()
}

/// Close or drop unfinished inline syntax at the end of a line.
fn repair_inline(line: &str) -> String {
    let fixed = repair_code_and_links(line);
    repair_emphasis(&fixed)
}

fn repair_code_and_links(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let take = |range: std::ops::Range<usize>| chars[range].iter().collect::<String>();
    let mut brackets: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < n {
        match chars[i] {
            '\\' => {
                if i + 1 == n {
                    return take(0..i);
                }
                i += 2;
            }
            '`' => {
                let run = run_len(&chars, i);
                let mut j = i + run;
                let mut close = None;
                while j < n {
                    if chars[j] == '`' {
                        let r = run_len(&chars, j);
                        if r == run {
                            close = Some(j);
                            break;
                        }
                        j += r;
                    } else {
                        j += 1;
                    }
                }
                match close {
                    Some(j) => i = j + run,
                    None => {
                        // Unclosed code span: nothing after the opener -> drop it, else close it.
                        if take(i + run..n).trim().is_empty() {
                            return take(0..i);
                        }
                        let mut out = take(0..n);
                        out.extend(std::iter::repeat_n('`', run));
                        return out;
                    }
                }
            }
            '[' => {
                brackets.push(i);
                i += 1;
            }
            ']' => {
                let Some(open) = brackets.pop() else {
                    i += 1;
                    continue;
                };
                let image = open > 0 && chars[open - 1] == '!';
                let cut = if image { open - 1 } else { open };
                if chars.get(i + 1) == Some(&'(') {
                    let mut depth = 0usize;
                    let mut close = None;
                    for (k, c) in chars.iter().enumerate().skip(i + 1) {
                        match c {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    close = Some(k);
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    match close {
                        Some(k) => i = k + 1,
                        // Destination still streaming: show the link text only (hide images).
                        None if image => return take(0..cut),
                        None => return take(0..open) + &take(open + 1..i),
                    }
                } else if i + 1 == n {
                    // `[text]` at the very end may still grow a `(url)`.
                    return if image { take(0..cut) } else { take(0..open) + &take(open + 1..i) };
                } else {
                    i += 1;
                }
            }
            '<' => {
                let tail = &chars[i + 1..];
                let tag_like = tail.first().is_some_and(|c| c.is_ascii_alphabetic() || *c == '/' || *c == '!');
                if tag_like && tail.len() < 48 && !tail.iter().any(|c| *c == '>' || c.is_whitespace()) {
                    return take(0..i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    if let Some(open) = brackets.last().copied() {
        let image = open > 0 && chars[open - 1] == '!';
        return if image { take(0..open - 1) } else { take(0..open) + &take(open + 1..n) };
    }
    line.to_owned()
}

struct Delim {
    ch: char,
    count: usize,
    pos: usize,
}

fn repair_emphasis(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut stack: Vec<Delim> = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '\\' => i += 2,
            '`' => {
                let run = run_len(&chars, i);
                let mut j = i + run;
                while j < n && !(chars[j] == '`' && run_len(&chars, j) == run) {
                    j += 1;
                }
                i = (j + run).min(n);
            }
            'h' if chars[i..].starts_with(&['h', 't', 't', 'p']) && (i == 0 || !chars[i - 1].is_alphanumeric()) => {
                // Skip URLs: their underscores and tildes are not emphasis.
                while i < n && !chars[i].is_whitespace() && chars[i] != ')' {
                    i += 1;
                }
            }
            ']' if chars.get(i + 1) == Some(&'(') => {
                while i < n && chars[i] != ')' {
                    i += 1;
                }
            }
            '*' | '_' | '~' => {
                let run = run_len(&chars, i);
                let prev = i.checked_sub(1).map(|p| chars[p]);
                let next = chars.get(i + run).copied();
                let intraword = prev.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric);
                if (c == '_' && intraword) || (c == '~' && run < 2) {
                    i += run;
                    continue;
                }
                let can_close = prev.is_some_and(|p| !p.is_whitespace());
                let can_open = next.is_some_and(|x| !x.is_whitespace());
                let mut remaining = run;
                if can_close {
                    while remaining > 0 {
                        match stack.last_mut() {
                            Some(top) if top.ch == c => {
                                let used = top.count.min(remaining);
                                top.count -= used;
                                remaining -= used;
                                if top.count == 0 {
                                    stack.pop();
                                }
                            }
                            _ => break,
                        }
                    }
                }
                if remaining > 0 && (can_open || next.is_none()) {
                    stack.push(Delim { ch: c, count: remaining, pos: i + run - remaining });
                }
                i += run;
            }
            _ => i += 1,
        }
    }
    let mut end = n;
    let mut closers = String::new();
    while let Some(delim) = stack.pop() {
        let trailing = chars[delim.pos + delim.count..end].iter().all(|c| c.is_whitespace());
        if trailing && closers.is_empty() {
            // An opener with nothing after it yet: hide it instead of showing a stray `**`.
            end = delim.pos;
        } else {
            closers.extend(std::iter::repeat_n(delim.ch, delim.count));
        }
    }
    let mut out: String = chars[..end].iter().collect();
    if !closers.is_empty() {
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
        out.push_str(&closers);
    }
    out
}

// ───────────────────────── block builder ─────────────────────────

#[derive(Default)]
struct Table {
    alignments: Vec<MdAlign>,
    header: Vec<MdCell>,
    rows: Vec<Vec<MdCell>>,
    row: Vec<MdCell>,
    in_head: bool,
}

#[derive(PartialEq)]
enum Inline {
    None,
    Paragraph,
    Heading(u8),
    Cell,
}

#[derive(Default)]
struct Style {
    bold: u32,
    italic: u32,
    strike: u32,
    links: Vec<String>,
}

struct Builder {
    doc: MdDocument,
    lists: Vec<Option<u64>>,
    quote: u8,
    marker: Option<String>,
    inline: Inline,
    runs: Vec<MdRun>,
    style: Style,
    code: Option<(Option<String>, String)>,
    table: Option<Table>,
    image: Option<(String, String)>,
    paragraph_images: Vec<(String, String)>,
    html: Option<String>,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            doc: MdDocument::default(),
            lists: Vec::new(),
            quote: 0,
            marker: None,
            inline: Inline::None,
            runs: Vec::new(),
            style: Style::default(),
            code: None,
            table: None,
            image: None,
            paragraph_images: Vec::new(),
            html: None,
        }
    }
}

impl Builder {
    fn run(mut self, source: &str) -> MdDocument {
        let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        for event in Parser::new_ext(source, options) {
            self.event(event);
        }
        self.flush_inline();
        self.doc
    }

    fn push(&mut self, kind: MdKind) {
        let tag = match kind {
            MdKind::Paragraph { .. } => 'p',
            MdKind::Heading { .. } => 'h',
            MdKind::Code { .. } => 'c',
            MdKind::Table { .. } => 't',
            MdKind::Image { .. } => 'i',
            MdKind::Rule => 'r',
        };
        self.doc.blocks.push(MdBlock {
            id: format!("{}{tag}", self.doc.blocks.len()),
            kind,
            indent: self.lists.len().min(u8::MAX as usize) as u8,
            quote: self.quote,
            marker: self.marker.take(),
        });
    }

    fn note_link(&mut self, url: &str) {
        if (url.starts_with("http://") || url.starts_with("https://"))
            && !self.doc.links.iter().any(|l| l == url)
        {
            self.doc.links.push(url.to_owned());
        }
    }

    fn push_run(&mut self, text: &str, code: bool, link: Option<String>) {
        if text.is_empty() {
            return;
        }
        let run = MdRun {
            text: text.to_owned(),
            bold: self.style.bold > 0,
            italic: self.style.italic > 0,
            strike: self.style.strike > 0,
            code,
            link: link.or_else(|| self.style.links.last().cloned()),
            citation: false,
        };
        match self.runs.last_mut() {
            Some(last)
                if last.bold == run.bold
                    && last.italic == run.italic
                    && last.strike == run.strike
                    && last.code == run.code
                    && last.link == run.link =>
            {
                last.text.push_str(&run.text)
            }
            _ => self.runs.push(run),
        }
    }

    /// Plain text, with bare URLs turned into links.
    fn text(&mut self, text: &str) {
        if let Some((_, alt)) = self.image.as_mut() {
            alt.push_str(text);
            return;
        }
        if self.inline == Inline::None {
            self.inline = Inline::Paragraph;
        }
        if !self.style.links.is_empty() || !text.contains("http") {
            self.push_run(text, false, None);
            return;
        }
        let mut rest = text;
        while let Some(start) = find_url(rest) {
            let candidate = &rest[start..];
            let mut end = candidate.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`')).unwrap_or(candidate.len());
            while end > 0 {
                let last = candidate[..end].chars().next_back().unwrap_or(' ');
                let unbalanced_paren = last == ')' && !candidate[..end].contains('(');
                if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '_' | ']') || unbalanced_paren {
                    end -= last.len_utf8();
                } else {
                    break;
                }
            }
            let url = &candidate[..end];
            if url.len() <= "https://".len() {
                self.push_run(&rest[..start + end.max(1)], false, None);
                rest = &rest[start + end.max(1)..];
                continue;
            }
            self.push_run(&rest[..start], false, None);
            self.note_link(url);
            self.push_run(url, false, Some(url.to_owned()));
            rest = &candidate[end..];
        }
        self.push_run(rest, false, None);
    }

    fn flush_inline(&mut self) {
        let runs = std::mem::take(&mut self.runs);
        let images = std::mem::take(&mut self.paragraph_images);
        match std::mem::replace(&mut self.inline, Inline::None) {
            Inline::None | Inline::Cell => {}
            Inline::Heading(level) => self.push(MdKind::Heading { level, runs: trim_runs(runs) }),
            Inline::Paragraph => {
                let only_image = images.len() == 1
                    && runs.iter().all(|r| r.link.as_deref() == Some(images[0].0.as_str()) || r.text.trim().is_empty());
                if only_image {
                    let (url, alt) = images.into_iter().next().expect("one image");
                    self.push(MdKind::Image { url, alt });
                    return;
                }
                let runs = trim_runs(runs);
                if !runs.is_empty() {
                    self.push(MdKind::Paragraph { runs });
                }
            }
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some((_, code)) = self.code.as_mut() {
                    code.push_str(&text);
                } else {
                    self.text(&text);
                }
            }
            Event::Code(code) => {
                if self.inline == Inline::None {
                    self.inline = Inline::Paragraph;
                }
                self.push_run(&code, true, None);
            }
            Event::SoftBreak | Event::HardBreak => {
                if self.inline != Inline::None {
                    self.push_run(if self.inline == Inline::Cell { " " } else { "\n" }, false, None);
                }
            }
            Event::Rule => {
                self.flush_inline();
                self.push(MdKind::Rule);
            }
            Event::TaskListMarker(checked) => {
                self.marker = Some(if checked { "[x]" } else { "[ ]" }.to_owned());
            }
            Event::Html(html) => match self.html.as_mut() {
                Some(buffer) => buffer.push_str(&html),
                None => self.inline_html(&html),
            },
            Event::InlineHtml(html) => self.inline_html(&html),
            Event::InlineMath(math) | Event::DisplayMath(math) => self.text(&math),
            Event::FootnoteReference(name) => self.text(&format!("[{name}]")),
        }
    }

    fn inline_html(&mut self, html: &str) {
        let lower = html.trim().to_ascii_lowercase();
        if lower.starts_with("<br") && self.inline != Inline::None {
            self.push_run("\n", false, None);
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.flush_inline();
                self.inline = Inline::Paragraph;
            }
            Tag::Heading { level, .. } => {
                self.flush_inline();
                self.inline = Inline::Heading(level as u8);
            }
            Tag::BlockQuote(_) => {
                self.flush_inline();
                self.quote = self.quote.saturating_add(1);
            }
            Tag::CodeBlock(kind) => {
                self.flush_inline();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split([' ', ',', '{'])
                        .next()
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty()),
                    CodeBlockKind::Indented => None,
                };
                self.code = Some((language, String::new()));
            }
            Tag::HtmlBlock => {
                self.flush_inline();
                self.html = Some(String::new());
            }
            Tag::List(start) => {
                self.flush_inline();
                if self.marker.is_some() {
                    // An item whose first child is a nested list still shows its own marker.
                    self.push(MdKind::Paragraph { runs: Vec::new() });
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush_inline();
                let depth = self.lists.len();
                self.marker = Some(match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let label = format!("{number}.");
                        *number += 1;
                        label
                    }
                    _ => ["•", "◦", "▪"][depth.saturating_sub(1).min(2)].to_owned(),
                });
            }
            Tag::Table(alignments) => {
                self.flush_inline();
                self.table = Some(Table {
                    alignments: alignments
                        .iter()
                        .map(|a| match a {
                            Alignment::Center => MdAlign::Center,
                            Alignment::Right => MdAlign::Trailing,
                            _ => MdAlign::Leading,
                        })
                        .collect(),
                    ..Table::default()
                });
            }
            Tag::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.in_head = true;
                }
            }
            Tag::TableRow => {}
            Tag::TableCell => {
                self.runs.clear();
                self.inline = Inline::Cell;
            }
            Tag::Emphasis => self.style.italic += 1,
            Tag::Strong => self.style.bold += 1,
            Tag::Strikethrough => self.style.strike += 1,
            Tag::Link { dest_url, .. } => {
                if self.inline == Inline::None {
                    self.inline = Inline::Paragraph;
                }
                self.note_link(&dest_url);
                self.style.links.push(dest_url.to_string());
            }
            Tag::Image { dest_url, .. } => {
                if self.inline == Inline::None {
                    self.inline = Inline::Paragraph;
                }
                self.image = Some((dest_url.to_string(), String::new()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.flush_inline(),
            TagEnd::BlockQuote(_) => {
                self.flush_inline();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some((language, mut code)) = self.code.take() {
                    if code.ends_with('\n') {
                        code.pop();
                    }
                    let spans = highlight::highlight(language.as_deref(), &code);
                    self.push(MdKind::Code { language, code, spans, closed: true });
                }
            }
            TagEnd::HtmlBlock => {
                if let Some(html) = self.html.take() {
                    let text = strip_tags(&html);
                    if !text.trim().is_empty() {
                        self.inline = Inline::Paragraph;
                        self.push_run(text.trim(), false, None);
                        self.flush_inline();
                    }
                }
            }
            TagEnd::List(_) => {
                self.flush_inline();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.flush_inline();
                if self.marker.is_some() {
                    self.push(MdKind::Paragraph { runs: Vec::new() });
                }
            }
            TagEnd::TableCell => {
                let runs = trim_runs(std::mem::take(&mut self.runs));
                self.inline = Inline::None;
                if let Some(table) = self.table.as_mut() {
                    table.row.push(MdCell { runs });
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.header = std::mem::take(&mut table.row);
                    table.in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.push(MdKind::Table { alignments: table.alignments, header: table.header, rows: table.rows });
                }
            }
            TagEnd::Emphasis => self.style.italic = self.style.italic.saturating_sub(1),
            TagEnd::Strong => self.style.bold = self.style.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.style.strike = self.style.strike.saturating_sub(1),
            TagEnd::Link => {
                self.style.links.pop();
            }
            TagEnd::Image => {
                if let Some((url, alt)) = self.image.take() {
                    let label = if alt.trim().is_empty() { "image".to_owned() } else { alt.clone() };
                    self.push_run(&label, false, Some(url.clone()));
                    self.paragraph_images.push((url, alt));
                }
            }
            _ => {}
        }
    }
}

fn find_url(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = text[from..].find("http") {
        let at = from + i;
        let rest = &text[at..];
        let boundary = at == 0 || !text[..at].chars().next_back().is_some_and(char::is_alphanumeric);
        if boundary && (rest.starts_with("https://") || rest.starts_with("http://")) {
            return Some(at);
        }
        from = at + 4;
    }
    None
}

/// Drop leading/trailing whitespace runs so blocks don't start or end with a blank line.
fn trim_runs(mut runs: Vec<MdRun>) -> Vec<MdRun> {
    while runs.first().is_some_and(|r| r.text.trim().is_empty()) {
        runs.remove(0);
    }
    while runs.last().is_some_and(|r| r.text.trim().is_empty()) {
        runs.pop();
    }
    if let Some(first) = runs.first_mut() {
        first.text = first.text.trim_start_matches('\n').to_owned();
    }
    if let Some(last) = runs.last_mut() {
        last.text = last.text.trim_end_matches('\n').to_owned();
    }
    runs
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(text: &str) -> String {
        repair_streaming(text).0
    }

    fn plain(doc: &MdDocument) -> Vec<String> {
        doc.blocks
            .iter()
            .map(|b| match &b.kind {
                MdKind::Paragraph { runs } | MdKind::Heading { runs, .. } => {
                    runs.iter().map(|r| r.text.as_str()).collect::<String>()
                }
                MdKind::Code { code, .. } => format!("code:{code}"),
                MdKind::Table { header, rows, .. } => format!("table:{}x{}", header.len(), rows.len()),
                MdKind::Image { url, .. } => format!("image:{url}"),
                MdKind::Rule => "rule".into(),
            })
            .collect()
    }

    #[test]
    fn closes_unfinished_emphasis() {
        assert_eq!(fix("some **bold te"), "some **bold te**");
        assert_eq!(fix("an *ital"), "an *ital*");
        assert_eq!(fix("mix **bold and *it"), "mix **bold and *it***");
        assert_eq!(fix("done **bold** then"), "done **bold** then");
        assert_eq!(fix("~~gone"), "~~gone~~");
    }

    #[test]
    fn hides_openers_with_nothing_after_them() {
        assert_eq!(fix("word **"), "word ");
        assert_eq!(fix("word `"), "word ");
        assert_eq!(fix("trailing \\"), "trailing ");
    }

    #[test]
    fn leaves_snake_case_and_urls_alone() {
        assert_eq!(fix("call some_function_name now"), "call some_function_name now");
        assert_eq!(fix("see https://x.com/a_b/_c"), "see https://x.com/a_b/_c");
        assert_eq!(fix("2 * 3 = 6"), "2 * 3 = 6");
    }

    #[test]
    fn closes_inline_code() {
        assert_eq!(fix("run `cargo bui"), "run `cargo bui`");
        assert_eq!(fix("ok `a` and `b"), "ok `a` and `b`");
    }

    #[test]
    fn shows_link_text_until_the_url_lands() {
        assert_eq!(fix("see [the docs"), "see the docs");
        assert_eq!(fix("see [the docs]"), "see the docs");
        assert_eq!(fix("see [the docs](https://exa"), "see the docs");
        assert_eq!(fix("see [the docs](https://example.com) now"), "see [the docs](https://example.com) now");
        assert_eq!(fix("pic ![alt](https://exa"), "pic ");
        assert_eq!(fix("pic ![al"), "pic ");
    }

    #[test]
    fn holds_back_half_typed_block_markers() {
        assert_eq!(fix("para\n\n#"), "para\n\n");
        assert_eq!(fix("para\n\n- "), "para\n\n");
        assert_eq!(fix("para\n\n2."), "para\n\n");
        assert_eq!(fix("para\n\n- [ ]"), "para\n\n");
        assert_eq!(fix("para\n``"), "para\n");
        // Would otherwise flash the paragraph as a big setext heading.
        assert_eq!(fix("Title line\n--"), "Title line\n");
        assert_eq!(fix("Title line\n==="), "Title line\n");
        assert_eq!(fix("para\n\n---"), "para\n\n---");
    }

    #[test]
    fn list_items_repair_after_their_marker() {
        assert_eq!(fix("- **Rock Hall"), "- **Rock Hall**");
        assert_eq!(fix("3. *Cin"), "3. *Cin*");
        assert_eq!(fix("> ## Head **x"), "> ## Head **x**");
    }

    #[test]
    fn tables_wait_for_their_separator_row() {
        assert_eq!(fix("intro\n\n| a | b |"), "intro\n\n");
        assert_eq!(fix("intro\n\n| a | b |\n|--"), "intro\n\n");
        let confirmed = "intro\n\n| a | b |\n|---|---|\n| 1 |";
        assert_eq!(fix(confirmed), confirmed);
        assert_eq!(plain(&parse(confirmed, true)), ["intro", "table:2x1"]);
    }

    #[test]
    fn open_fences_are_left_to_the_parser() {
        let (text, open) = repair_streaming("```rust\nlet x = **1");
        assert_eq!(text, "```rust\nlet x = **1");
        assert!(open);
        let doc = parse("```rust\nlet x = 1;\n``", true);
        match &doc.blocks[0].kind {
            MdKind::Code { language, code, closed, .. } => {
                assert_eq!(language.as_deref(), Some("rust"));
                assert_eq!(code, "let x = 1;");
                assert!(!closed);
            }
            other => panic!("expected code, got {other:?}"),
        }
        assert!(matches!(&parse("```\na\n```", true).blocks[0].kind, MdKind::Code { closed: true, .. }));
    }

    #[test]
    fn flattens_nested_lists_with_markers() {
        let doc = parse("1. One\n   - **Rock** hall\n   - Art\n2. Two\n\n- [x] done\n- [ ] todo", false);
        let rows: Vec<(u8, Option<&str>)> =
            doc.blocks.iter().map(|b| (b.indent, b.marker.as_deref())).collect();
        assert_eq!(
            rows,
            [(1, Some("1.")), (2, Some("◦")), (2, Some("◦")), (1, Some("2.")), (1, Some("[x]")), (1, Some("[ ]"))]
        );
        assert_eq!(plain(&doc)[1], "Rock hall");
    }

    #[test]
    fn collects_links_and_linkifies_bare_urls() {
        let doc = parse("Read [docs](https://a.dev/x) or https://b.dev/y. Also `https://no.dev`.", false);
        assert_eq!(doc.links, ["https://a.dev/x", "https://b.dev/y"]);
        let MdKind::Paragraph { runs } = &doc.blocks[0].kind else { panic!() };
        assert!(runs.iter().any(|r| r.text == "https://b.dev/y" && r.link.is_some()));
        assert!(runs.iter().any(|r| r.text == "." && r.link.is_none()));
    }

    #[test]
    fn citation_markers_link_to_their_sources() {
        let doc = parse(
            "The film was restored in 4K.[1] Reviews were warm [2, 3] but `x[1]` stays code and [9] is unknown.\n\n\
             ## Sources\n\
             [1] [QueenOnline: release](https://queen.example/release)\n\
             [2] [Forbes review](https://forbes.example/review)\n\
             [3]: https://cine.example/listings",
            false,
        );
        let MdKind::Paragraph { runs } = &doc.blocks[0].kind else { panic!() };
        let cited: Vec<(&str, &str)> =
            runs.iter().filter(|r| r.citation).map(|r| (r.text.as_str(), r.link.as_deref().unwrap())).collect();
        assert_eq!(
            cited,
            [("1", "https://queen.example/release"), ("2", "https://forbes.example/review"), ("3", "https://cine.example/listings")]
        );
        let text: String = runs.iter().filter(|r| !r.citation).map(|r| r.text.as_str()).collect();
        assert_eq!(text, "The film was restored in 4K. Reviews were warm  but x[1] stays code and [9] is unknown.");
        // The list itself gets the same markers in front of each source.
        let MdKind::Paragraph { runs } = &doc.blocks[2].kind else { panic!() };
        assert_eq!(runs.iter().filter(|r| r.citation).count(), 3);
        assert!(runs.iter().any(|r| r.text == "Forbes review" && !r.citation));
    }

    #[test]
    fn numbered_source_lists_define_citations_too() {
        let doc = parse("Fast.[2]\n\n### References\n\n1. [One](https://a.example)\n2. [Two](https://b.example)", false);
        let MdKind::Paragraph { runs } = &doc.blocks[0].kind else { panic!() };
        assert!(runs.iter().any(|r| r.citation && r.link.as_deref() == Some("https://b.example")));
        // Markdown reference definitions resolve in the parser; the result is still a citation.
        let defined = parse("Proven.[1]\n\n[1]: https://proof.example", false);
        let MdKind::Paragraph { runs } = &defined.blocks[0].kind else { panic!() };
        assert!(runs.iter().any(|r| r.citation && r.text == "1" && r.link.as_deref() == Some("https://proof.example")));
        // Without a list of sources, brackets are just text.
        let plain = parse("See item [2] in the array.", false);
        let MdKind::Paragraph { runs } = &plain.blocks[0].kind else { panic!() };
        assert!(runs.iter().all(|r| !r.citation));
    }

    #[test]
    fn lone_images_become_image_blocks() {
        let doc = parse("![cat](https://x.dev/cat.png)", false);
        assert_eq!(plain(&doc), ["image:https://x.dev/cat.png"]);
    }

    #[test]
    fn normalises_model_habits() {
        assert_eq!(plain(&parse("##Title", false)), ["Title"]);
        assert!(matches!(parse("##Title", false).blocks[0].kind, MdKind::Heading { level: 2, .. }));
        assert_eq!(parse("• one\n• two", false).blocks.len(), 2);
        assert_eq!(plain(&parse("Scores:\n| a | b |\n|---|---|\n| 1 | 2 |", false)), ["Scores:", "table:2x1"]);
        assert_eq!(plain(&parse("\u{1b}[1mBold\u{1b}[0m text", false)), ["Bold text"]);
        assert_eq!(plain(&parse("<think>hmm</think>\nAnswer", false)), ["Answer"]);
        assert_eq!(plain(&parse("Answer <think>still thinking", true)), ["Answer"]);
        assert_eq!(plain(&parse("line one\nline two", false)), ["line one\nline two"]);
    }

    #[test]
    fn block_ids_are_stable_while_a_block_grows() {
        let a = parse("First para\n\nSecond gro", true);
        let b = parse("First para\n\nSecond growing longer", true);
        assert_eq!(a.blocks[1].id, b.blocks[1].id);
    }

    #[test]
    fn every_prefix_of_a_document_parses() {
        let source = "# Title\n\nSome **bold** and *it* with `code` and [link](https://x.dev).\n\n\
            1. One\n   - nested ~~x~~\n2. Two\n\n| a | b |\n|:--|--:|\n| 1 | 2 |\n\n```py\nprint('hi')\n```\n\n> quote\n\n---\n![i](https://x.dev/i.png)";
        for (i, _) in source.char_indices() {
            let doc = parse(&source[..i], true);
            for block in &doc.blocks {
                if let MdKind::Paragraph { runs } = &block.kind {
                    let text: String = runs.iter().map(|r| r.text.as_str()).collect();
                    assert!(!text.contains("**"), "raw ** leaked at {i}: {text:?}");
                    assert!(!text.contains("]("), "raw link leaked at {i}: {text:?}");
                    assert!(!text.starts_with('|'), "raw table row leaked at {i}: {text:?}");
                }
            }
        }
    }
}
