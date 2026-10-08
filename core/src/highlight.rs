//! A small, dependency-free syntax highlighter for code blocks.
//!
//! One generic lexer covers the languages agents usually emit. Each language only declares how
//! its comments and strings look and which words are keywords; that is enough to make code
//! readable without shipping grammar files.

use crate::markdown::{MdCodeSpan, MdTokenKind};

/// Blocks larger than this are shown unhighlighted.
const MAX_HIGHLIGHT_BYTES: usize = 60_000;

struct Lang {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    keywords: &'static [&'static str],
    /// `Capitalized` identifiers are types.
    types: bool,
}

const C_LIKE: &[&str] = &[
    "abstract", "as", "async", "await", "break", "case", "catch", "class", "const", "continue",
    "default", "defer", "delete", "do", "else", "enum", "export", "extends", "extension", "false",
    "final", "finally", "fn", "for", "from", "func", "function", "go", "guard", "if", "impl",
    "implements", "import", "in", "interface", "let", "loop", "match", "mod", "mut", "namespace",
    "new", "nil", "null", "of", "package", "private", "protected", "protocol", "pub", "public",
    "return", "self", "static", "struct", "super", "switch", "this", "throw", "throws", "trait",
    "true", "try", "type", "typeof", "undefined", "use", "var", "void", "where", "while", "yield",
    "int", "float", "double", "bool", "string", "char", "long", "unsigned", "auto", "using",
    "override", "virtual", "template", "typename", "include", "define", "val", "fun", "when",
    "object", "companion", "data", "sealed", "chan", "select", "range", "map", "dyn", "ref",
    "unsafe", "crate", "move", "actor", "nonisolated", "some", "any", "inout", "is", "init",
];
const PYTHON: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield", "match", "case", "self",
];
const SHELL: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until", "case",
    "esac", "function", "return", "export", "local", "readonly", "set", "unset", "source", "echo",
    "cd", "exit", "sudo", "true", "false",
];
const RUBY: &[&str] = &[
    "def", "end", "class", "module", "if", "elsif", "else", "unless", "while", "until", "do",
    "return", "yield", "begin", "rescue", "ensure", "require", "include", "nil", "true", "false",
    "self", "then", "case", "when", "in", "and", "or", "not", "puts", "attr_accessor",
];
const SQL: &[&str] = &[
    "select", "from", "where", "and", "or", "not", "insert", "into", "values", "update", "set",
    "delete", "create", "table", "alter", "drop", "index", "view", "join", "left", "right",
    "inner", "outer", "on", "as", "group", "by", "order", "having", "limit", "offset", "union",
    "all", "distinct", "null", "is", "in", "like", "between", "primary", "key", "foreign",
    "references", "default", "case", "when", "then", "else", "end", "with", "returning", "exists",
    "count", "sum", "avg", "min", "max", "asc", "desc", "begin", "commit", "rollback",
];
const DATA: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];
const LUA: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

fn lang_for(name: &str) -> Option<Lang> {
    let lang = |line_comments, block_comment, quotes, keywords, types| {
        Some(Lang { line_comments, block_comment, quotes, keywords, types })
    };
    match name {
        "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" | "mjs" | "cjs" | "java"
        | "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "cs" | "csharp" | "go" | "golang" | "rust"
        | "rs" | "swift" | "kotlin" | "kt" | "php" | "dart" | "scala" | "objc" | "objective-c"
        | "zig" | "groovy" | "proto" | "glsl" => {
            lang(&["//"], Some(("/*", "*/")), &['"', '\'', '`'], C_LIKE, true)
        }
        "py" | "python" | "python3" => lang(&["#"], None, &['"', '\''], PYTHON, true),
        "sh" | "bash" | "zsh" | "shell" | "console" | "fish" | "dockerfile" | "docker" | "make"
        | "makefile" | "powershell" | "ps1" => lang(&["#"], None, &['"', '\''], SHELL, false),
        "rb" | "ruby" | "elixir" | "ex" | "perl" | "pl" | "r" | "nix" => {
            lang(&["#"], None, &['"', '\''], RUBY, true)
        }
        "sql" | "sqlite" | "postgres" | "postgresql" | "mysql" | "plsql" => {
            lang(&["--"], Some(("/*", "*/")), &['\''], SQL, false)
        }
        "json" | "jsonc" | "json5" => lang(&["//"], Some(("/*", "*/")), &['"'], DATA, false),
        "yaml" | "yml" | "toml" | "ini" | "conf" | "env" | "properties" => {
            lang(&["#"], None, &['"', '\''], DATA, false)
        }
        "lua" | "haskell" | "hs" | "elm" | "ada" => lang(&["--"], None, &['"', '\''], LUA, true),
        "css" | "scss" | "less" => lang(&["//"], Some(("/*", "*/")), &['"', '\''], &[], false),
        "html" | "xml" | "svg" | "vue" | "svelte" | "plist" => {
            lang(&[], Some(("<!--", "-->")), &['"', '\''], &[], false)
        }
        _ => None,
    }
}

pub(crate) fn highlight(language: Option<&str>, code: &str) -> Vec<MdCodeSpan> {
    let plain = || vec![MdCodeSpan { text: code.to_owned(), kind: MdTokenKind::Plain }];
    let Some(name) = language else { return plain() };
    if code.len() > MAX_HIGHLIGHT_BYTES {
        return plain();
    }
    let Some(lang) = lang_for(name) else { return plain() };
    let case_insensitive = matches!(name, "sql" | "sqlite" | "postgres" | "postgresql" | "mysql" | "plsql");
    let keyed = matches!(name, "json" | "jsonc" | "json5" | "yaml" | "yml" | "toml" | "ini" | "conf" | "env" | "properties");
    let markup = matches!(name, "html" | "xml" | "svg" | "vue" | "svelte" | "plist");

    let mut spans: Vec<MdCodeSpan> = Vec::new();
    let mut push = |text: &str, kind: MdTokenKind| {
        if text.is_empty() {
            return;
        }
        match spans.last_mut() {
            Some(last) if last.kind == kind => last.text.push_str(text),
            _ => spans.push(MdCodeSpan { text: text.to_owned(), kind }),
        }
    };

    let mut i = 0;
    while i < code.len() {
        let rest = &code[i..];
        let c = rest.chars().next().expect("non-empty rest");

        if let Some((open, close)) = lang.block_comment {
            if rest.starts_with(open) {
                let end = rest[open.len()..].find(close).map(|e| e + open.len() + close.len()).unwrap_or(rest.len());
                push(&rest[..end], MdTokenKind::Comment);
                i += end;
                continue;
            }
        }
        if lang.line_comments.iter().any(|lc| rest.starts_with(lc)) {
            let end = rest.find('\n').unwrap_or(rest.len());
            push(&rest[..end], MdTokenKind::Comment);
            i += end;
            continue;
        }
        if lang.quotes.contains(&c) {
            let triple: String = std::iter::repeat_n(c, 3).collect();
            let (delim, multiline) = if rest.starts_with(&triple) { (triple.as_str(), true) } else { (&rest[..1], c == '`') };
            let mut end = delim.len();
            let mut closed = false;
            while end < rest.len() {
                let tail = &rest[end..];
                if tail.starts_with('\\') {
                    end += 1 + tail[1..].chars().next().map_or(0, char::len_utf8);
                } else if tail.starts_with(delim) {
                    end += delim.len();
                    closed = true;
                    break;
                } else if tail.starts_with('\n') && !multiline {
                    break;
                } else {
                    end += tail.chars().next().map_or(1, char::len_utf8);
                }
            }
            // A lone apostrophe (Rust lifetimes, prose in comments) is not a string.
            let end = end.min(rest.len());
            if !closed && c == '\'' {
                push(&rest[..1], MdTokenKind::Plain);
                i += 1;
                continue;
            }
            let key = keyed && rest[end..].trim_start_matches(' ').starts_with(':');
            push(&rest[..end], if key { MdTokenKind::Property } else { MdTokenKind::String });
            i += end;
            continue;
        }
        if c.is_ascii_digit() {
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
                .unwrap_or(rest.len());
            push(&rest[..end], MdTokenKind::Number);
            i += end;
            continue;
        }
        if c.is_alphabetic() || c == '_' || c == '$' || c == '@' {
            let end = rest[c.len_utf8()..]
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$' || (markup || keyed) && ch == '-'))
                .map(|e| e + c.len_utf8())
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let after = rest[end..].trim_start_matches(' ');
            let is_keyword = if case_insensitive {
                lang.keywords.contains(&word.to_ascii_lowercase().as_str())
            } else {
                lang.keywords.contains(&word)
            };
            let line_start = code[..i].rsplit('\n').next().is_some_and(|l| l.trim().is_empty() || l.trim() == "-");
            let kind = if markup {
                let prev = code[..i].chars().next_back();
                if matches!(prev, Some('<') | Some('/')) { MdTokenKind::Keyword }
                else if after.starts_with('=') { MdTokenKind::Property }
                else { MdTokenKind::Plain }
            } else if is_keyword {
                MdTokenKind::Keyword
            } else if keyed && line_start && (after.starts_with(':') || after.starts_with('=')) {
                MdTokenKind::Property
            } else if after.starts_with('(') {
                MdTokenKind::Function
            } else if lang.types && c.is_uppercase() {
                MdTokenKind::Type
            } else {
                MdTokenKind::Plain
            };
            push(word, kind);
            i += end;
            continue;
        }
        push(&rest[..c.len_utf8()], MdTokenKind::Plain);
        i += c.len_utf8();
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: &str, code: &str) -> Vec<(String, MdTokenKind)> {
        highlight(Some(lang), code)
            .into_iter()
            .filter(|s| !s.text.trim().is_empty() && s.kind != MdTokenKind::Plain)
            .map(|s| (s.text, s.kind))
            .collect()
    }

    #[test]
    fn rust_tokens() {
        let got = kinds("rust", "fn main() { let x: Vec<u8> = vec![1]; } // hi");
        assert!(got.contains(&("fn".into(), MdTokenKind::Keyword)));
        assert!(got.contains(&("main".into(), MdTokenKind::Function)));
        assert!(got.contains(&("Vec".into(), MdTokenKind::Type)));
        assert!(got.contains(&("1".into(), MdTokenKind::Number)));
        assert!(got.contains(&("// hi".into(), MdTokenKind::Comment)));
    }

    #[test]
    fn python_strings_and_comments() {
        let got = kinds("python", "def f(a):\n    \"\"\"doc\nstring\"\"\"\n    return 'x'  # note");
        assert!(got.contains(&("\"\"\"doc\nstring\"\"\"".into(), MdTokenKind::String)));
        assert!(got.contains(&("# note".into(), MdTokenKind::Comment)));
    }

    #[test]
    fn json_keys_are_properties() {
        let got = kinds("json", "{\"name\": \"hermes\", \"n\": 3, \"ok\": true}");
        assert!(got.contains(&("\"name\"".into(), MdTokenKind::Property)));
        assert!(got.contains(&("\"hermes\"".into(), MdTokenKind::String)));
        assert!(got.contains(&("true".into(), MdTokenKind::Keyword)));
    }

    #[test]
    fn round_trips_the_source() {
        let code = "let s = 'it''s'; /* open\nif x { \"é\" }";
        for lang in ["rust", "sql", "python", "html", "yaml", "nope"] {
            let joined: String = highlight(Some(lang), code).into_iter().map(|s| s.text).collect();
            assert_eq!(joined, code, "{lang}");
        }
    }
}
