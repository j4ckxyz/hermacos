//! Slash commands: the catalog for the command menu, and running one.
//!
//! The gateway has two entry points. `slash.exec` runs a command and returns its printed
//! output. Commands that are really prompts (skills, `/goal`) or that hand text back to the
//! composer (`/undo`) are refused there and answered by `command.dispatch` with a typed
//! directive instead. A client tries the first and falls back to the second.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{HermesError, Result};
use crate::gateway::Gateway;
use crate::markdown::strip_ansi;
use crate::types::{SlashCommand, SlashOutcome};
use crate::util::{opt_str, str_of};

/// Some commands think for a while (`/compress`, `/insights`).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);
/// An alias may point at another alias; this many hops is already a loop.
const MAX_ALIAS_HOPS: usize = 4;

fn with_slash(name: &str) -> String {
    format!("/{}", name.trim().trim_start_matches('/'))
}

/// Flatten the gateway's catalog into one list, categories in the server's order.
pub(crate) fn commands_from_catalog(catalog: &Value) -> Vec<SlashCommand> {
    let pair = |row: &Value| -> Option<(String, String)> {
        let row = row.as_array()?;
        let name = row.first()?.as_str()?.trim();
        (!name.is_empty()).then(|| (with_slash(name), row.get(1).and_then(Value::as_str).unwrap_or_default().trim().to_owned()))
    };
    // name -> other names for it
    let mut aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (alias, canonical) in catalog.get("canon").and_then(Value::as_object).into_iter().flatten() {
        let (alias, canonical) = (with_slash(alias), with_slash(canonical.as_str().unwrap_or_default()));
        if alias != canonical {
            aliases.entry(canonical).or_default().push(alias);
        }
    }
    let subcommands = |name: &str| -> Vec<String> {
        let table = catalog.get("sub").and_then(Value::as_object);
        let bare = name.trim_start_matches('/');
        table
            .and_then(|t| t.get(name).or_else(|| t.get(bare)))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    };
    let mut out: Vec<SlashCommand> = Vec::new();
    let mut add = |name: String, description: String, category: &str| {
        if out.iter().any(|c| c.name == name) {
            return;
        }
        out.push(SlashCommand {
            aliases: aliases.get(&name).cloned().unwrap_or_default(),
            subcommands: subcommands(&name),
            category: category.to_owned(),
            name,
            description,
        });
    };
    for category in catalog.get("categories").and_then(Value::as_array).into_iter().flatten() {
        let title = str_of(category, "name");
        for row in category.get("pairs").and_then(Value::as_array).into_iter().flatten() {
            if let Some((name, description)) = pair(row) {
                add(name, description, &title);
            }
        }
    }
    // Older gateways list everything in one flat table.
    for row in catalog.get("pairs").and_then(Value::as_array).into_iter().flatten() {
        if let Some((name, description)) = pair(row) {
            add(name, description, "");
        }
    }
    out
}

/// Printed output, cleaned of terminal colour codes.
fn printed(result: &Value) -> String {
    let body = opt_str(result, "output").unwrap_or_else(|| "(no output)".into());
    let text = match opt_str(result, "warning") {
        Some(warning) => format!("warning: {warning}\n{body}"),
        None => body,
    };
    strip_ansi(&text).trim_end().to_owned()
}

enum Directive {
    Done(SlashOutcome),
    /// Run this other command instead.
    Alias(String),
}

fn directive(result: &Value, arg: &str) -> Option<Directive> {
    let notice = opt_str(result, "notice").map(|n| strip_ansi(&n));
    let message = opt_str(result, "message").unwrap_or_default();
    Some(match result.get("type").and_then(Value::as_str)? {
        "exec" | "plugin" => Directive::Done(SlashOutcome::Output { text: printed(result) }),
        "alias" => {
            let target = opt_str(result, "target")?;
            Directive::Alias(format!("{} {arg}", with_slash(&target)).trim_end().to_owned())
        }
        "prefill" => Directive::Done(SlashOutcome::Prefill { message, notice }),
        "send" | "skill" if !message.trim().is_empty() => {
            Directive::Done(SlashOutcome::Send { message, display: opt_str(result, "display"), notice })
        }
        "send" | "skill" => Directive::Done(SlashOutcome::Output {
            text: notice.unwrap_or_else(|| "The command had nothing to send.".into()),
        }),
        _ => return None,
    })
}

pub(crate) async fn run(gateway: &Arc<Gateway>, session_id: &str, command: &str) -> Result<SlashOutcome> {
    let mut command = command.trim().to_owned();
    for _ in 0..MAX_ALIAS_HOPS {
        let bare = command.trim_start_matches('/');
        let (name, arg) = bare.split_once(char::is_whitespace).map_or((bare, ""), |(n, a)| (n, a.trim()));
        if name.is_empty() {
            return Err(HermesError::protocol("Type a command after the slash."));
        }
        let exec = gateway
            .request("slash.exec", json!({ "session_id": session_id, "command": bare }), COMMAND_TIMEOUT)
            .await;
        let next = match exec {
            Ok(result) => directive(&result, arg).unwrap_or(Directive::Done(SlashOutcome::Output { text: printed(&result) })),
            Err(exec_error) => {
                let params = json!({ "session_id": session_id, "name": name, "arg": arg });
                match gateway.request("command.dispatch", params, COMMAND_TIMEOUT).await {
                    Ok(result) => directive(&result, arg)
                        .ok_or_else(|| HermesError::protocol(format!("/{name} returned something this app doesn't understand.")))?,
                    // "not a … command" only says the fallback had nothing to add; the first
                    // failure is the real one, unless that was merely the redirect itself.
                    Err(HermesError::Rpc { message, .. }) if message.contains("not a quick") => {
                        return Err(match exec_error {
                            HermesError::Rpc { code: 4018, .. } => {
                                HermesError::Rpc { code: 4018, message: format!("/{name} isn't a command this server knows.") }
                            }
                            other => other,
                        });
                    }
                    Err(dispatch_error) => return Err(dispatch_error),
                }
            }
        };
        match next {
            Directive::Done(outcome) => return Ok(outcome),
            Directive::Alias(target) => command = target,
        }
    }
    Err(HermesError::protocol("That command keeps redirecting to another command."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_flattens_with_aliases_and_subcommands() {
        let catalog = json!({
            "categories": [
                { "name": "Session", "pairs": [["/new", "Start a new session"], ["/usage", "Show token usage"]] },
                { "name": "Configuration", "pairs": [["reasoning", "Set reasoning effort"]] },
            ],
            "pairs": [["/usage", "Show token usage"], ["/extra", "Only in the flat list"]],
            "canon": { "/reset": "/new", "new": "new", "u": "usage" },
            "sub": { "/reasoning": ["low", "high"] },
        });
        let commands = commands_from_catalog(&catalog);
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["/new", "/usage", "/reasoning", "/extra"]);
        assert_eq!(commands[0].aliases, ["/reset"]);
        assert_eq!(commands[1].aliases, ["/u"]);
        assert_eq!(commands[2].subcommands, ["low", "high"]);
        assert_eq!(commands[2].category, "Configuration");
        assert_eq!(commands[3].category, "");
    }

    #[test]
    fn directives_map_to_outcomes() {
        let skill = json!({ "type": "skill", "message": "Plan a trip", "display": "/plan-trip" });
        assert!(matches!(
            directive(&skill, ""),
            Some(Directive::Done(SlashOutcome::Send { message, display: Some(d), .. })) if message == "Plan a trip" && d == "/plan-trip"
        ));
        let alias = json!({ "type": "alias", "target": "help" });
        assert!(matches!(directive(&alias, "me"), Some(Directive::Alias(t)) if t == "/help me"));
        let exec = json!({ "type": "exec", "output": "\u{1b}[1mTokens\u{1b}[0m 12\n" });
        assert!(matches!(directive(&exec, ""), Some(Directive::Done(SlashOutcome::Output { text })) if text == "Tokens 12"));
        assert!(directive(&json!({ "output": "plain" }), "").is_none());
    }
}
