//! Tier-2 marginal surface against the fake Herdr server: exact params
//! serialization for every new method, typed response decoding, and the
//! capability ledger's refusal-driven degradation.

mod support;

use serde_json::json;
use support::{Action, FakeHerdr};

use lerdr_herdr::{
    capabilities::features, Client, ClientWindowTitleReason, ConfigReloadStatus, FeatureState,
    IntegrationTarget, PaneReportMetadataParams, PluginLogListParams, PluginPaneOpenParams,
    PluginPanePlacement, PopupSize, SplitDirection, WorkspaceReportMetadataParams,
};

fn client_for(server: &FakeHerdr) -> Client {
    Client::unix(server.sock_path.clone())
}

// -- serialization ----------------------------------------------------------

#[tokio::test]
async fn pane_report_metadata_serializes_full_shape() {
    let server = FakeHerdr::start(Action::Reply(json!({"type": "ok"}))).await;
    let client = client_for(&server);
    let mut tokens = std::collections::BTreeMap::new();
    tokens.insert("lerdr_watching".to_owned(), Some("1".to_owned()));
    tokens.insert("stale".to_owned(), None); // clears the key
    let mut state_labels = std::collections::BTreeMap::new();
    state_labels.insert("watch".to_owned(), "phone".to_owned());
    client
        .pane_report_metadata(&PaneReportMetadataParams {
            pane_id: "wE:pE".to_owned(),
            source: "lerdr-relay".to_owned(),
            agent: Some("claude".to_owned()),
            title: Some("working".to_owned()),
            display_agent: Some("claude".to_owned()),
            state_labels,
            tokens,
            ttl_ms: Some(300_000),
            seq: Some(7),
            applies_to_source: Some("mobile".to_owned()),
            clear_title: true,
            clear_display_agent: true,
            clear_state_labels: true,
        })
        .await
        .unwrap();

    let req = &server.requests()[0];
    assert_eq!(req.method, "pane.report_metadata");
    let p = &req.params;
    assert_eq!(p["pane_id"], "wE:pE");
    assert_eq!(p["source"], "lerdr-relay");
    assert_eq!(p["agent"], "claude");
    assert_eq!(p["title"], "working");
    assert_eq!(p["display_agent"], "claude");
    assert_eq!(p["state_labels"]["watch"], "phone");
    assert_eq!(p["tokens"]["lerdr_watching"], "1");
    assert_eq!(p["tokens"]["stale"], serde_json::Value::Null);
    assert_eq!(p["ttl_ms"], 300_000);
    assert_eq!(p["seq"], 7);
    assert_eq!(p["applies_to_source"], "mobile");
    assert_eq!(p["clear_title"], true);
    assert_eq!(p["clear_display_agent"], true);
    assert_eq!(p["clear_state_labels"], true);
}

#[tokio::test]
async fn pane_report_metadata_omits_absent_optionals() {
    let server = FakeHerdr::start(Action::Reply(json!({"type": "ok"}))).await;
    let client = client_for(&server);
    let mut tokens = std::collections::BTreeMap::new();
    tokens.insert("lerdr_watching".to_owned(), Some("1".to_owned()));
    client
        .pane_report_metadata(&PaneReportMetadataParams {
            pane_id: "wE:pE".to_owned(),
            source: "lerdr-relay".to_owned(),
            tokens,
            ttl_ms: Some(300_000),
            seq: Some(1),
            ..PaneReportMetadataParams::default()
        })
        .await
        .unwrap();

    let p = &server.requests()[0].params;
    // Optional/absent fields stay off the wire.
    for key in [
        "agent",
        "title",
        "display_agent",
        "state_labels",
        "applies_to_source",
    ] {
        assert!(p.get(key).is_none(), "{key} must be omitted");
    }
    // `clear_*` are plain bools — always present per the schema's shape.
    assert_eq!(p["clear_title"], false);
}

#[tokio::test]
async fn workspace_report_metadata_serializes() {
    let server = FakeHerdr::start(Action::Reply(json!({"type": "ok"}))).await;
    let client = client_for(&server);
    let mut tokens = std::collections::BTreeMap::new();
    tokens.insert("lerdr_devices".to_owned(), Some("3".to_owned()));
    client
        .workspace_report_metadata(&WorkspaceReportMetadataParams {
            workspace_id: "wE".to_owned(),
            source: "lerdr-relay".to_owned(),
            tokens,
            ttl_ms: Some(300_000),
            seq: Some(2),
        })
        .await
        .unwrap();

    let p = &server.requests()[0].params;
    assert_eq!(server.requests()[0].method, "workspace.report_metadata");
    assert_eq!(p["workspace_id"], "wE");
    assert_eq!(p["tokens"]["lerdr_devices"], "3");
    assert_eq!(p["seq"], 2);
}

#[tokio::test]
async fn window_title_set_and_clear_wire_shape() {
    let server = FakeHerdr::start(Action::Reply(
        json!({"type": "client_window_title", "changed": true, "reason": "set"}),
    ))
    .await;
    let client = client_for(&server);
    let outcome = client
        .client_window_title_set("lerdr: 2 devices")
        .await
        .unwrap();
    assert!(outcome.changed);
    assert_eq!(outcome.reason, ClientWindowTitleReason::Set);
    assert_eq!(server.requests()[0].params["title"], "lerdr: 2 devices");

    let outcome = client.client_window_title_clear().await.unwrap();
    assert!(outcome.changed);
    let clear = &server.requests()[1];
    assert_eq!(clear.method, "client.window_title.clear");
    assert_eq!(clear.params, json!({}), "clear takes empty params");
}

#[tokio::test]
async fn window_title_no_foreground_reason_decodes() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "client_window_title", "changed": false,
        "reason": "no_foreground_client"
    })))
    .await;
    let outcome = client_for(&server)
        .client_window_title_clear()
        .await
        .unwrap();
    assert!(!outcome.changed);
    assert_eq!(outcome.reason, ClientWindowTitleReason::NoForegroundClient);
}

#[tokio::test]
async fn server_admin_methods_send_empty_params() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "config_reload", "status": "applied", "diagnostics": []
    })))
    .await;
    let client = client_for(&server);
    client.server_reload_config().await.unwrap();
    server.set_default(Action::Reply(json!({
        "type": "agent_manifest_status", "manifests": []
    })));
    client.server_agent_manifests().await.unwrap();
    server.set_default(Action::Reply(json!({
        "type": "agent_manifest_reload", "manifests": []
    })));
    client.server_reload_agent_manifests().await.unwrap();
    let requests = server.requests();
    assert_eq!(requests[0].method, "server.reload_config");
    assert_eq!(requests[1].method, "server.agent_manifests");
    assert_eq!(requests[2].method, "server.reload_agent_manifests");
    for req in &requests {
        assert_eq!(req.params, json!({}), "{} takes empty params", req.method);
    }
}

#[tokio::test]
async fn config_reload_decodes_status_and_diagnostics() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "config_reload", "status": "partial",
        "diagnostics": ["plugin x: bad key"]
    })))
    .await;
    let outcome = client_for(&server).server_reload_config().await.unwrap();
    assert_eq!(outcome.status, ConfigReloadStatus::Partial);
    assert_eq!(outcome.diagnostics, vec!["plugin x: bad key".to_owned()]);
}

#[tokio::test]
async fn agent_manifest_status_decodes() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "agent_manifest_status",
        "last_check_unix": 1700000000,
        "last_result": "ok",
        "manifests": [{
            "agent": "claude", "source": "remote", "source_kind": "builtin",
            "local_override_shadowing_remote": false,
            "active_version": "v1"
        }]
    })))
    .await;
    let status = client_for(&server).server_agent_manifests().await.unwrap();
    assert_eq!(status.last_check_unix, Some(1_700_000_000));
    assert_eq!(status.manifests.len(), 1);
    assert_eq!(status.manifests[0].agent, "claude");
    assert_eq!(status.manifests[0].active_version.as_deref(), Some("v1"));
}

#[tokio::test]
async fn agent_manifest_reload_decodes_list() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "agent_manifest_reload",
        "manifests": [{
            "agent": "codex", "source": "local", "source_kind": "override",
            "local_override_shadowing_remote": true
        }]
    })))
    .await;
    let manifests = client_for(&server)
        .server_reload_agent_manifests()
        .await
        .unwrap();
    assert_eq!(manifests.len(), 1);
    assert!(manifests[0].local_override_shadowing_remote);
}

#[tokio::test]
async fn integration_targets_serialize_and_decode() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "integration_install",
        "target": "claude",
        "details": {"messages": ["hook installed"]}
    })))
    .await;
    let client = client_for(&server);
    let outcome = client
        .integration_install(IntegrationTarget::Claude)
        .await
        .unwrap();
    assert_eq!(outcome.target, IntegrationTarget::Claude);
    assert_eq!(outcome.details.messages, vec!["hook installed".to_owned()]);
    assert_eq!(server.requests()[0].params["target"], "claude");

    // The antigravity_cli spelling is snake_case on the wire.
    let outcome = client
        .integration_install(IntegrationTarget::AntigravityCli)
        .await
        .unwrap();
    assert_eq!(outcome.target, IntegrationTarget::Claude); // server echo
    assert_eq!(server.requests()[1].params["target"], "antigravity_cli");
}

#[tokio::test]
async fn integration_uninstall_decodes() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "integration_uninstall",
        "target": "codex",
        "details": {"messages": []}
    })))
    .await;
    let outcome = client_for(&server)
        .integration_uninstall(IntegrationTarget::Codex)
        .await
        .unwrap();
    assert_eq!(outcome.target, IntegrationTarget::Codex);
    assert_eq!(server.requests()[0].method, "integration.uninstall");
}

#[tokio::test]
async fn plugin_enable_disable_wire_shape() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "plugin_enabled",
        "plugin": {"plugin_id": "lerdr.events", "name": "Lerdr",
                   "version": "1", "manifest_path": "/m", "plugin_root": "/r",
                   "enabled": true}
    })))
    .await;
    let client = client_for(&server);
    let plugin = client.plugin_enable("lerdr.events").await.unwrap();
    assert_eq!(plugin.plugin_id, "lerdr.events");
    assert!(plugin.enabled);
    assert_eq!(server.requests()[0].method, "plugin.enable");
    assert_eq!(server.requests()[0].params["plugin_id"], "lerdr.events");

    server.set_default(Action::Reply(json!({
        "type": "plugin_disabled",
        "plugin": {"plugin_id": "lerdr.events", "name": "Lerdr",
                   "version": "1", "manifest_path": "/m", "plugin_root": "/r",
                   "enabled": false}
    })));
    let plugin = client.plugin_disable("lerdr.events").await.unwrap();
    assert!(!plugin.enabled);
    assert_eq!(server.requests()[1].method, "plugin.disable");
}

#[tokio::test]
async fn plugin_log_list_params_and_decode() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "plugin_log_list",
        "logs": [{"log_id": "l1", "plugin_id": "lerdr.events",
                  "command": ["echo", "hi"], "status": "succeeded",
                  "started_unix_ms": 1700000000}]
    })))
    .await;
    let client = client_for(&server);
    let logs = client
        .plugin_log_list(Some("lerdr.events"), Some(10))
        .await
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].log_id, "l1");
    let p = &server.requests()[0].params;
    assert_eq!(p["plugin_id"], "lerdr.events");
    assert_eq!(p["limit"], 10);

    // No filter — both keys stay off the wire.
    client.plugin_log_list(None, None).await.unwrap();
    let p = &server.requests()[1].params;
    assert!(p.get("plugin_id").is_none());
    assert!(p.get("limit").is_none());
}

#[tokio::test]
async fn plugin_pane_open_serializes_all_flags() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "plugin_pane_opened",
        "plugin_pane": {
            "plugin_id": "lerdr.events", "entrypoint": "status",
            "pane": {"pane_id": "wE:p9", "terminal_id": "t9",
                     "workspace_id": "wE", "tab_id": "wE:t1",
                     "focused": true, "agent_status": "idle"}
        }
    })))
    .await;
    let client = client_for(&server);
    let mut env = std::collections::BTreeMap::new();
    env.insert("PATH".to_owned(), "/bin".to_owned());
    let pane = client
        .plugin_pane_open(&PluginPaneOpenParams {
            plugin_id: "lerdr.events".to_owned(),
            entrypoint: "status".to_owned(),
            workspace_id: Some("wE".to_owned()),
            target_pane_id: Some("wE:pE".to_owned()),
            cwd: Some("/tmp".to_owned()),
            env,
            direction: Some(SplitDirection::Right),
            placement: Some(PluginPanePlacement::Overlay),
            width: Some(PopupSize::Percent(80)),
            height: Some(PopupSize::Cells(24)),
            focus: Some(true),
        })
        .await
        .unwrap();
    assert_eq!(pane.pane.pane_id, "wE:p9");

    let p = &server.requests()[0].params;
    assert_eq!(p["plugin_id"], "lerdr.events");
    assert_eq!(p["entrypoint"], "status");
    assert_eq!(p["workspace_id"], "wE");
    assert_eq!(p["target_pane_id"], "wE:pE");
    assert_eq!(p["cwd"], "/tmp");
    assert_eq!(p["env"]["PATH"], "/bin");
    assert_eq!(p["direction"], "right");
    assert_eq!(p["placement"], "overlay");
    assert_eq!(p["width"], "80%");
    assert_eq!(p["height"], 24);
    assert_eq!(p["focus"], true);
}

#[tokio::test]
async fn plugin_pane_open_minimal_omits_optionals() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "plugin_pane_opened",
        "plugin_pane": {
            "plugin_id": "p", "entrypoint": "e",
            "pane": {"pane_id": "wE:p9", "terminal_id": "t9",
                     "workspace_id": "wE", "tab_id": "wE:t1",
                     "focused": false, "agent_status": "idle"}
        }
    })))
    .await;
    client_for(&server)
        .plugin_pane_open(&PluginPaneOpenParams {
            plugin_id: "p".to_owned(),
            entrypoint: "e".to_owned(),
            ..PluginPaneOpenParams::default()
        })
        .await
        .unwrap();
    let p = &server.requests()[0].params;
    assert_eq!(p["plugin_id"], "p");
    assert_eq!(p["entrypoint"], "e");
    for key in [
        "workspace_id",
        "target_pane_id",
        "cwd",
        "env",
        "direction",
        "placement",
        "width",
        "height",
        "focus",
    ] {
        assert!(p.get(key).is_none(), "{key} must be omitted");
    }
}

#[tokio::test]
async fn plugin_pane_focus_and_close() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "plugin_pane_focused",
        "plugin_pane": {
            "plugin_id": "p", "entrypoint": "e",
            "pane": {"pane_id": "wE:p9", "terminal_id": "t9",
                     "workspace_id": "wE", "tab_id": "wE:t1",
                     "focused": true, "agent_status": "idle"}
        }
    })))
    .await;
    let client = client_for(&server);
    let pane = client.plugin_pane_focus("wE:p9").await.unwrap();
    assert_eq!(pane.pane.pane_id, "wE:p9");
    assert_eq!(server.requests()[0].params["pane_id"], "wE:p9");

    server.set_default(Action::Reply(
        json!({"type": "plugin_pane_closed", "pane_id": "wE:p9"}),
    ));
    let closed = client.plugin_pane_close("wE:p9").await.unwrap();
    assert_eq!(closed, "wE:p9");
    assert_eq!(server.requests()[1].method, "plugin.pane.close");
}

// -- capability ledger ------------------------------------------------------

/// A definitive `unknown_method` refusal on a NOTED method flips the
/// published feature to `Unsupported` — the relay's callers then stop
/// attempting it.
#[tokio::test]
async fn unknown_method_refusal_marks_feature_unsupported() {
    let server = FakeHerdr::start(Action::Refuse("unknown_method", "no such method")).await;
    let client = client_for(&server);
    let err = client.client_window_title_set("x").await.unwrap_err();
    assert_eq!(err.phase(), lerdr_herdr::DispatchPhase::Refused);
    let feature = client.feature(features::CLIENT_WINDOW_TITLE_SET);
    assert_eq!(feature.state, FeatureState::Unsupported);
    assert_eq!(feature.reason, "method_not_supported");
}

/// Success on a NOTED method records `Supported` evidence.
#[tokio::test]
async fn succeeded_call_marks_feature_supported() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "client_window_title", "changed": true, "reason": "set"
    })))
    .await;
    let client = client_for(&server);
    client.client_window_title_set("x").await.unwrap();
    let feature = client.feature(features::CLIENT_WINDOW_TITLE_SET);
    assert_eq!(feature.state, FeatureState::Supported);
}

/// An untracked failure (dispatch lost) leaves the feature unknown.
#[tokio::test]
async fn dispatched_unknown_records_no_evidence() {
    let server = FakeHerdr::start(Action::HangUp).await;
    let client = client_for(&server);
    let _ = client.client_window_title_set("x").await.unwrap_err();
    let feature = client.feature(features::CLIENT_WINDOW_TITLE_SET);
    assert_eq!(feature.state, FeatureState::Unknown);
}

/// `PluginLogListParams` defaults keep both fields off the wire — the
/// no-filter list call.
#[test]
fn plugin_log_list_params_default_is_empty() {
    let params = PluginLogListParams::default();
    let value = serde_json::to_value(&params).unwrap();
    assert_eq!(value, json!({}));
}

/// `PopupSize` CLI spellings round-trip through `parse`.
#[test]
fn popup_size_parse_accepts_cells_and_percent() {
    assert_eq!(PopupSize::parse("80%"), Some(PopupSize::Percent(80)));
    assert_eq!(PopupSize::parse("100%"), Some(PopupSize::Percent(100)));
    assert_eq!(PopupSize::parse("1%"), Some(PopupSize::Percent(1)));
    assert_eq!(PopupSize::parse("0%"), None, "percent floor is 1");
    assert_eq!(PopupSize::parse("101%"), None, "percent ceiling is 100");
    assert_eq!(PopupSize::parse("24"), Some(PopupSize::Cells(24)));
    assert_eq!(PopupSize::parse("0"), Some(PopupSize::Cells(0)));
    assert_eq!(PopupSize::parse("65536"), None);
    assert_eq!(PopupSize::parse("abc"), None);
}

/// `IntegrationTarget::parse` covers the CLI spellings the installed
/// `herdr integration install` accepts.
#[test]
fn integration_target_parse_covers_cli_spellings() {
    assert_eq!(
        IntegrationTarget::parse("claude"),
        Some(IntegrationTarget::Claude)
    );
    assert_eq!(
        IntegrationTarget::parse("antigravity-cli"),
        Some(IntegrationTarget::AntigravityCli)
    );
    assert_eq!(
        IntegrationTarget::parse("antigravity_cli"),
        Some(IntegrationTarget::AntigravityCli)
    );
    assert_eq!(IntegrationTarget::parse("bogus"), None);
}
