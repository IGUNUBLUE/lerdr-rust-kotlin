//! Pane input and lifecycle actions — the `input.go`/`dispatch.go` port.
//!
//! Every handler answers a `command_result` + `action_receipt` pair through
//! [`Outcome::frames`]. Method/params match the Herdr socket schema the
//! oracle's CLI commands map onto.

use lerdr_core::protocol::Inbound;
use lerdr_herdr::Client;
use serde::Serialize;

use super::{
    dispatch_failure, record_activity, record_activity_extract, uploads, ActionContext, Outcome,
    COMMAND_DEADLINE, PROMPT_MAX_CHARS,
};

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
    // `expandPromptAttachmentReferences` — `Attachment: <ref>` lines resolve
    // through the upload index before dispatch; unresolvable refs fail with
    // the oracle's exact message.
    let mut text = message.text.clone();
    if let Err(error) =
        uploads::expand_attachment_references(&ctx, message.target.as_ref(), &mut text).await
    {
        return Outcome::failed(&message.pane_id, error).frames(request_id, "send_text", action_id);
    }
    let outcome = send_text_inner(&ctx.client, &message.pane_id, &text).await;
    if outcome.ok {
        record_activity_extract(
            &ctx,
            "send_text",
            "sent",
            "Text inserted",
            &text,
            &message.pane_id,
            request_id,
        );
    }
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
    if outcome.ok {
        let label = message.raw_str("activity_label").unwrap_or("keys");
        record_activity(&ctx, "send_keys", "sent", label, pane_id, request_id);
    }
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
    if outcome.ok {
        let label = message
            .raw_str("activity_label")
            .unwrap_or("Terminal input sent");
        // The oracle records this family under the `input` kind.
        record_activity_extract(
            &ctx,
            "input",
            "sent",
            label,
            &input.text,
            pane_id,
            request_id,
        );
    }
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
    if outcome.ok {
        record_activity(
            &ctx,
            "send_secret",
            "sent",
            "Password entered",
            pane_id,
            request_id,
        );
    }
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
    let mut text = if message.text.is_empty() {
        message.prompt.clone()
    } else {
        message.text.clone()
    };
    // `expandPromptAttachmentReferences` — same as send_text.
    if let Err(error) =
        uploads::expand_attachment_references(&ctx, message.target.as_ref(), &mut text).await
    {
        return Outcome::failed(pane_id, error).frames(request_id, "submit_prompt", action_id);
    }
    let outcome = prompt_inner(&ctx, pane_id, &text, request_id).await;
    outcome.frames(request_id, "submit_prompt", action_id)
}

/// `handlePrompt`'s effect — shared by the routed `submit_prompt` and the
/// `agent_start` initial prompt (the oracle calls `handlePrompt` inline for
/// the latter, with the `"-initial"` request-id suffix).
pub(crate) async fn prompt_inner(
    ctx: &ActionContext,
    pane_id: &str,
    text: &str,
    request_id: &str,
) -> Outcome {
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
    let outcome = if requires_enter {
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
    };
    if outcome.ok {
        record_activity_extract(
            ctx,
            "submit_prompt",
            "sent",
            "Prompt sent",
            text,
            pane_id,
            request_id,
        );
    }
    outcome
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
            // `d.state.BumpGeneration` (dispatch.go:543) — the close ended
            // the pane's session; stale exact targets must stop
            // validating. Then the oracle drops the remembered profile and
            // publishes the post-close topology (`profiles.Forget` +
            // `MarkTopologyChanged` + `wake`).
            ctx.handle.bump_generation(pane_id.to_owned()).await;
            ctx.profiles.forget(pane_id);
            ctx.handle.refresh().await;
            Outcome::completed(pane_id, None)
        }
        Err(err) => dispatch_failure(pane_id, &err),
    };
    if outcome.ok {
        record_activity(
            &ctx,
            "agent_stop",
            "sent",
            "Stopped agent",
            pane_id,
            request_id,
        );
    }
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

    /// Replies `{"type":"ok"}` (or the scripted `error`) to each request —
    /// enough to drive `agent_stop` to its bump path.
    struct OkTransport {
        error: Option<serde_json::Value>,
    }

    impl lerdr_herdr::Transport for OkTransport {
        fn dial(
            &self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<lerdr_herdr::BoxIo>> + Send>,
        > {
            let error = self.error.clone();
            Box::pin(async move {
                let (client_end, mut server_end) = tokio::io::duplex(8192);
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while let Ok(n) = server_end.read(&mut chunk).await {
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                            let line: Vec<u8> = buf.drain(..=pos).collect();
                            let request: serde_json::Value =
                                serde_json::from_slice(&line).unwrap_or_default();
                            let id = request["id"].clone();
                            let reply = match &error {
                                Some(error) => serde_json::json!({"id": id, "error": error}),
                                None => serde_json::json!({"id": id, "result": {"type": "ok"}}),
                            };
                            if server_end
                                .write_all(reply.to_string().as_bytes())
                                .await
                                .is_err()
                                || server_end.write_all(b"\n").await.is_err()
                            {
                                return;
                            }
                        }
                    }
                });
                Ok(Box::new(client_end) as lerdr_herdr::BoxIo)
            })
        }

        fn describe(&self) -> String {
            "ok".to_owned()
        }
    }

    fn test_context(client: Client) -> ActionContext {
        ActionContext {
            handle: crate::TopologyActor::spawn(
                client.clone(),
                tokio_util::sync::CancellationToken::new(),
            ),
            leases: crate::actions::leases::Leases::new(client.clone()),
            acks: crate::actions::Acks::default(),
            profiles: crate::actions::profiles::Resolver::with_config_home(
                tempfile::tempdir().expect("tempdir").keep(),
            ),
            questions: crate::actions::questions::Questions::default(),
            uploads: crate::actions::uploads::Uploads::new(
                tempfile::tempdir().expect("tempdir").keep(),
            ),
            activities: crate::actions::activity::Journal::default(),
            push: crate::actions::push::Push::default(),
            speech: crate::actions::speech::Speech::default(),
            notices: crate::actions::Notices::default(),
            audit: None,
            device_id: "test-device".to_owned(),
            client,
            topology: std::sync::Arc::new(crate::topology::Topology::default()),
            client_id: "test-client".to_owned(),
        }
    }

    /// Wait until the actor publishes a topology whose pane generation
    /// reaches `want` (the bump runs on the actor's command lane).
    async fn await_generation(handle: &crate::actor::TopologyHandle, pane_id: &str, want: i64) {
        let pane_id = pane_id.to_owned();
        let mut rx = handle.topology.clone();
        tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            while rx.borrow().generation_of(&pane_id) < want {
                rx.changed().await.expect("topology channel closed");
            }
        })
        .await
        .expect("generation bump was not published");
    }

    #[tokio::test]
    async fn agent_stop_bumps_the_pane_generation() {
        let client = Client::new(
            std::sync::Arc::new(OkTransport { error: None }),
            lerdr_herdr::ClientConfig::default(),
        );
        let ctx = test_context(client);
        let mut message = Inbound::default();
        message.pane_id = "wE:p1".into();
        let frames = agent_stop(ctx.clone(), "r1", "a1", &message).await;
        assert!(matches!(
            frames.first(),
            Some(lerdr_core::protocol::Outbound::CommandResult(m)) if m.ok == Some(true)
        ));
        // dispatch.go:543 — a successful close advances the pane epoch so
        // stale exact targets stop validating.
        await_generation(&ctx.handle, "wE:p1", 1).await;

        // A refused close bumps nothing (dispatch.go:537-538 `failErr`).
        let client = Client::new(
            std::sync::Arc::new(OkTransport {
                error: Some(serde_json::json!({"code": "pane_not_found", "message": "gone"})),
            }),
            lerdr_herdr::ClientConfig::default(),
        );
        let ctx = test_context(client);
        message.pane_id = "wE:p2".into();
        let frames = agent_stop(ctx.clone(), "r2", "a2", &message).await;
        assert!(matches!(
            frames.first(),
            Some(lerdr_core::protocol::Outbound::CommandResult(m)) if m.ok == Some(false)
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(ctx.handle.topology.borrow().generation_of("wE:p2"), 0);
    }
}
