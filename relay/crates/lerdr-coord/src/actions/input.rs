//! Pane input and lifecycle actions — the `input.go`/`dispatch.go` port.
//!
//! Every handler answers a `command_result` + `action_receipt` pair through
//! [`Outcome::frames`]. Method/params match the Herdr socket schema the
//! oracle's CLI commands map onto.

use lerdr_core::protocol::Inbound;
use lerdr_herdr::Client;
use serde::Serialize;

use super::{dispatch_failure, ActionContext, Outcome, COMMAND_DEADLINE, PROMPT_MAX_CHARS};

/// `secretMaxRunes`.
const SECRET_MAX_RUNES: usize = 256;
/// `MaxPaneInputTextBytes` — `pane.send_input` text budget.
const MAX_INPUT_TEXT_BYTES: usize = 64 * 1024;
/// `shiftTabSequence` — Herdr maps a bare `shift+tab` keys request to the
/// backtab escape, which raw key delivery does not produce.
const SHIFT_TAB_SEQUENCE: &str = "\x1b[Z";

#[derive(Serialize)]
struct PaneText<'a> {
    pane_id: &'a str,
    text: &'a str,
}

#[derive(Serialize)]
struct PaneKeys<'a> {
    pane_id: &'a str,
    keys: &'a [String],
}

#[derive(Serialize)]
struct PaneInput<'a> {
    pane_id: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    text: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    keys: Vec<String>,
}

#[derive(Serialize)]
struct PaneTarget<'a> {
    pane_id: &'a str,
}

#[derive(Serialize)]
struct AgentPrompt<'a> {
    target: &'a str,
    text: &'a str,
}

/// `send_text` → `pane.send_text{pane_id, text}`.
pub(crate) async fn send_text(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let outcome = send_text_inner(&ctx.client, &message.pane_id, &message.text).await;
    outcome.frames(request_id, "send_text", action_id)
}

async fn send_text_inner(client: &Client, pane_id: &str, text: &str) -> Outcome {
    if pane_id.is_empty() || text.is_empty() {
        return Outcome::failed(pane_id, "Text and agent are required");
    }
    match client
        .call_with_timeout(
            "pane.send_text",
            &PaneText { pane_id, text },
            Some(COMMAND_DEADLINE),
        )
        .await
    {
        Ok(_) => Outcome::completed(pane_id, None),
        Err(err) => dispatch_failure(pane_id, &err),
    }
}

/// `send_keys` → `pane.send_keys{pane_id, keys}` — keys pass through
/// `normalizeTerminalKey` (a `ctrl+X` uppercase letter folds to lowercase);
/// a lone `shift+tab` becomes the backtab escape through `pane.send_text`.
pub(crate) async fn send_keys(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let pane_id = message.pane_id.as_str();
    if pane_id.is_empty() || message.keys.is_empty() {
        return Outcome::failed(pane_id, "Keys and agent are required").frames(
            request_id,
            "send_keys",
            action_id,
        );
    }
    let keys: Vec<String> = message
        .keys
        .iter()
        .map(|key| normalize_terminal_key(key))
        .collect();
    let outcome = if keys.len() == 1 && keys[0].eq_ignore_ascii_case("shift+tab") {
        match ctx
            .client
            .call_with_timeout(
                "pane.send_text",
                &PaneText {
                    pane_id,
                    text: SHIFT_TAB_SEQUENCE,
                },
                Some(COMMAND_DEADLINE),
            )
            .await
        {
            Ok(_) => Outcome::completed(pane_id, None),
            Err(err) => dispatch_failure(pane_id, &err),
        }
    } else {
        match ctx
            .client
            .call_with_timeout(
                "pane.send_keys",
                &PaneKeys {
                    pane_id,
                    keys: &keys,
                },
                Some(COMMAND_DEADLINE),
            )
            .await
        {
            Ok(_) => Outcome::completed(pane_id, None),
            Err(err) => dispatch_failure(pane_id, &err),
        }
    };
    outcome.frames(request_id, "send_keys", action_id)
}

/// `send_input` → `pane.send_input{pane_id, text?, keys?}` — keys are
/// semantic names validated exactly like `ValidatePaneInput`.
pub(crate) async fn send_input(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let pane_id = message.pane_id.as_str();
    let input = match normalize_pane_input(&message.text, &message.keys) {
        Ok(input) => input,
        Err(()) => {
            return Outcome::failed(pane_id, "Valid text or keys and an agent are required")
                .frames(request_id, "send_input", action_id);
        }
    };
    if pane_id.is_empty() {
        return Outcome::failed(pane_id, "Valid text or keys and an agent are required").frames(
            request_id,
            "send_input",
            action_id,
        );
    }
    let outcome = match ctx
        .client
        .call_with_timeout(
            "pane.send_input",
            &PaneInput {
                pane_id,
                text: &input.text,
                keys: input.keys,
            },
            Some(COMMAND_DEADLINE),
        )
        .await
    {
        Ok(_) => Outcome::completed(pane_id, None),
        Err(err) => dispatch_failure(pane_id, &err),
    };
    outcome.frames(request_id, "send_input", action_id)
}

/// `send_secret` — one key per rune plus Enter through `pane.send_keys`:
/// `pane.send_text` wraps text in a bracketed paste that a noecho reader
/// would swallow as part of the secret. Nothing about the secret reaches
/// the receipt or logs.
pub(crate) async fn send_secret(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let pane_id = message.pane_id.as_str();
    let text = message.text.as_str();
    if pane_id.is_empty() || text.is_empty() {
        return Outcome::failed(pane_id, "Secret and agent are required").frames(
            request_id,
            "send_secret",
            action_id,
        );
    }
    let runes: Vec<char> = text.chars().collect();
    if runes.len() > SECRET_MAX_RUNES {
        return Outcome::failed(pane_id, "Secret is too long").frames(
            request_id,
            "send_secret",
            action_id,
        );
    }
    let mut keys: Vec<String> = Vec::with_capacity(runes.len() + 1);
    for value in runes {
        if (value as u32) < 0x20 || value == '\u{7f}' {
            return Outcome::failed(pane_id, "Secret must not contain control characters").frames(
                request_id,
                "send_secret",
                action_id,
            );
        }
        keys.push(value.to_string());
    }
    keys.push("Enter".to_owned());
    let outcome = match ctx
        .client
        .call_with_timeout(
            "pane.send_keys",
            &PaneKeys {
                pane_id,
                keys: &keys,
            },
            Some(COMMAND_DEADLINE),
        )
        .await
    {
        Ok(_) => Outcome::completed(pane_id, None),
        Err(err) => dispatch_failure(pane_id, &err),
    };
    outcome.frames(request_id, "send_secret", action_id)
}

/// `submit_prompt` → `agent.prompt{target: pane_id, text}`. Qoder panes get
/// `pane.send_text` + `Enter` instead — its TUI does not treat the prompt
/// RPC's submit as a submission. A failed second leg is `partially_applied`:
/// the text already landed.
pub(crate) async fn submit_prompt(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let pane_id = message.pane_id.as_str();
    let text = if message.text.is_empty() {
        message.prompt.as_str()
    } else {
        message.text.as_str()
    };
    let outcome = prompt_inner(&ctx, pane_id, text).await;
    outcome.frames(request_id, "submit_prompt", action_id)
}

/// `handlePrompt`'s effect — shared by the routed `submit_prompt` and the
/// `agent_start` initial prompt (the oracle calls `handlePrompt` inline for
/// the latter).
pub(crate) async fn prompt_inner(ctx: &ActionContext, pane_id: &str, text: &str) -> Outcome {
    if pane_id.is_empty() || text.is_empty() {
        return Outcome::failed(pane_id, "Text and agent are required");
    }
    if text.chars().count() > PROMPT_MAX_CHARS {
        return Outcome::failed(pane_id, "Prompt is longer than 100,000 characters");
    }
    let requires_enter = ctx
        .topology
        .pane_of(pane_id)
        .and_then(|agent| agent.agent.as_deref())
        .is_some_and(is_qoder_agent);
    if requires_enter {
        match ctx
            .client
            .call_with_timeout(
                "pane.send_text",
                &PaneText { pane_id, text },
                Some(COMMAND_DEADLINE),
            )
            .await
        {
            Err(err) => dispatch_failure(pane_id, &err),
            Ok(_) => match ctx
                .client
                .call_with_timeout(
                    "pane.send_keys",
                    &PaneKeys {
                        pane_id,
                        keys: &["Enter".to_owned()],
                    },
                    Some(COMMAND_DEADLINE),
                )
                .await
            {
                Ok(_) => Outcome::completed(pane_id, None),
                Err(_) => Outcome::partially_applied(pane_id, "prompt text was already delivered"),
            },
        }
    } else {
        match ctx
            .client
            .call_with_timeout(
                "agent.prompt",
                &AgentPrompt {
                    target: pane_id,
                    text,
                },
                Some(COMMAND_DEADLINE),
            )
            .await
        {
            Ok(_) => Outcome::completed(pane_id, None),
            Err(err) => dispatch_failure(pane_id, &err),
        }
    }
}

/// `agent_stop` → `pane.close{pane_id}`.
pub(crate) async fn agent_stop(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<lerdr_core::protocol::Outbound> {
    let pane_id = message.pane_id.as_str();
    if pane_id.is_empty() {
        return Outcome::failed(pane_id, "Agent is required").frames(
            request_id,
            "agent_stop",
            action_id,
        );
    }
    let outcome = match ctx
        .client
        .call_with_timeout(
            "pane.close",
            &PaneTarget { pane_id },
            Some(COMMAND_DEADLINE),
        )
        .await
    {
        Ok(_) => {
            // The oracle drops the remembered profile and publishes the
            // post-close topology (`profiles.Forget` + `MarkTopologyChanged`
            // + `wake`).
            ctx.profiles.forget(pane_id);
            ctx.handle.refresh().await;
            Outcome::completed(pane_id, None)
        }
        Err(err) => dispatch_failure(pane_id, &err),
    };
    outcome.frames(request_id, "agent_stop", action_id)
}

/// `isQoderAgent`.
fn is_qoder_agent(agent: &str) -> bool {
    matches!(agent.trim().to_lowercase().as_str(), "qoder" | "qodercli")
}

/// `normalizeTerminalKey` — `ctrl+X` folds the letter to lowercase; every
/// other key passes through untouched.
fn normalize_terminal_key(key: &str) -> String {
    const PREFIX: &str = "ctrl+";
    if key.len() != PREFIX.len() + 1 || !key[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
        return key.to_owned();
    }
    let letter = key.as_bytes()[PREFIX.len()];
    if letter.is_ascii_uppercase() {
        return format!("ctrl+{}", (letter as char).to_ascii_lowercase());
    }
    key.to_owned()
}

struct NormalizedPaneInput {
    text: String,
    keys: Vec<String>,
}

/// `ValidatePaneInput` — text budget plus semantic key normalization; the
/// `Err()` is collapsed because every variant fails the same way at the
/// dispatch boundary.
fn normalize_pane_input(text: &str, keys: &[String]) -> Result<NormalizedPaneInput, ()> {
    if text.is_empty() && keys.is_empty() {
        return Err(());
    }
    if text.len() > MAX_INPUT_TEXT_BYTES {
        return Err(());
    }
    let mut normalized = Vec::with_capacity(keys.len());
    for key in keys {
        normalized.push(normalize_semantic_input_key(key).ok_or(())?);
    }
    Ok(NormalizedPaneInput {
        text: text.to_owned(),
        keys: normalized,
    })
}

/// `normalizeSemanticInputKey` — `ctrl`/`alt`/`shift` modifiers in that
/// fixed order, base one of the advertised names or `F1`–`F24`.
fn normalize_semantic_input_key(value: &str) -> Option<String> {
    if value.is_empty() || value.trim() != value {
        return None;
    }
    let parts: Vec<&str> = value.split('+').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut modifiers = Vec::new();
    for part in &parts[..parts.len() - 1] {
        let modifier = match part.to_lowercase().as_str() {
            "ctrl" => "ctrl",
            "alt" => "alt",
            "shift" => "shift",
            _ => return None,
        };
        if modifiers.contains(&modifier) {
            return None;
        }
        modifiers.push(modifier);
    }
    let base_name = parts[parts.len() - 1];
    let base = match base_name.to_lowercase().as_str() {
        "enter" => "Enter".to_owned(),
        "esc" => "Esc".to_owned(),
        "tab" => "Tab".to_owned(),
        "backspace" => "Backspace".to_owned(),
        "up" => "Up".to_owned(),
        "down" => "Down".to_owned(),
        "left" => "Left".to_owned(),
        "right" => "Right".to_owned(),
        lower => {
            let digits = lower.strip_prefix('f')?;
            let number: u32 = digits.parse().ok()?;
            if !(1..=24).contains(&number) || lower != format!("f{number}") {
                return None;
            }
            format!("F{number}")
        }
    };
    let mut ordered: Vec<&str> = Vec::with_capacity(modifiers.len() + 1);
    for modifier in ["ctrl", "alt", "shift"] {
        if modifiers.contains(&modifier) {
            ordered.push(modifier);
        }
    }
    ordered.push(&base);
    Some(ordered.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_key_folds_uppercase_ctrl_letter() {
        assert_eq!(normalize_terminal_key("ctrl+C"), "ctrl+c");
        assert_eq!(normalize_terminal_key("Ctrl+Z"), "ctrl+z");
        assert_eq!(normalize_terminal_key("ctrl+c"), "ctrl+c");
        assert_eq!(normalize_terminal_key("Enter"), "Enter");
        assert_eq!(normalize_terminal_key("ctrl+enter"), "ctrl+enter");
    }

    #[test]
    fn semantic_keys_normalize() {
        assert_eq!(
            normalize_semantic_input_key("shift+ctrl+enter"),
            Some("ctrl+shift+Enter".to_owned())
        );
        assert_eq!(normalize_semantic_input_key("f5"), Some("F5".to_owned()));
        assert_eq!(normalize_semantic_input_key("F24"), Some("F24".to_owned()));
        assert_eq!(normalize_semantic_input_key("f25"), None);
        assert_eq!(normalize_semantic_input_key("ctrl+ctrl+x"), None);
        assert_eq!(normalize_semantic_input_key("meta+x"), None);
        assert_eq!(normalize_semantic_input_key(" enter"), None);
        assert_eq!(normalize_semantic_input_key(""), None);
    }
}
