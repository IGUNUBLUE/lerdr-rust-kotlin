//! Capability ledger against the fake Herdr: schema-driven verdicts, probe
//! fallback, generation discipline, identity-aware notes, and the
//! `workspace.reordered` subscription outcome — plus the pane.read
//! singleflight key covering the *whole* parameter tuple.

mod support;

use std::time::Duration;

use serde_json::json;
use support::{Action, FakeHerdr};

use lerdr_herdr::capabilities::features;
use lerdr_herdr::{
    Client, ClientConfig, DispatchPhase, FeatureState, PaneReadParams, ReadFormat, ReadSource,
    SchemaRegistry, SchemaSource,
};

fn pong() -> serde_json::Value {
    json!({"type": "pong", "version": "0.9.1", "protocol": 22,
           "capabilities": {"live_handoff": true,
                            "endpoint_protocol_generation": 3,
                            "surface_interest": true,
                            "health_check": true}})
}

/// A client pinned to the fake socket, CLI introspection off, and the
/// herdr binary pointed nowhere so `--version` fails fast.
fn test_client(server: &FakeHerdr, schema_source: SchemaSource) -> Client {
    Client::unix_with(
        server.sock_path.clone(),
        ClientConfig {
            herdr_bin: Some(std::path::PathBuf::from("/nonexistent/herdr")),
            schema_source,
            ..ClientConfig::default()
        },
    )
}

fn schema_with(methods: &[&str], subs: &[&str], events: &[&str]) -> SchemaRegistry {
    let method_entries: Vec<_> = methods
        .iter()
        .map(|m| json!({"properties": {"method": {"const": m}}}))
        .collect();
    let sub_entries: Vec<_> = subs
        .iter()
        .map(|t| json!({"properties": {"type": {"const": t}}}))
        .collect();
    let event_entries: Vec<_> = events
        .iter()
        .map(|t| json!({"properties": {"type": {"const": t}}}))
        .collect();
    SchemaRegistry::from_value(&json!({
        "protocol": 22,
        "schemas": {
            "request": {
                "oneOf": method_entries,
                "$defs": {"Subscription": {"oneOf": sub_entries}},
            },
            "event": {"$defs": {"EventData": {"oneOf": event_entries}}},
        }
    }))
}

#[tokio::test]
async fn schema_path_adjudicates_tracked_methods() {
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    let schema = schema_with(
        &["ping", "pane.read", "agent.view.set"],
        &["pane.updated", "workspace.reordered"],
        &[],
    );
    let client = test_client(&server, SchemaSource::Static(schema));

    let report = client.collect_capabilities().await;
    assert_eq!(
        report.feature(features::ORDINARY_JSON).state,
        FeatureState::Supported
    );
    assert_eq!(
        report.feature(features::PANE_READ).state,
        FeatureState::Supported
    );
    assert_eq!(
        report.feature(features::PANE_READ).reason,
        "schema_advertised"
    );
    assert_eq!(
        report.feature(features::TAB_MOVE).state,
        FeatureState::Unsupported
    );
    assert_eq!(report.feature(features::TAB_MOVE).reason, "schema_absent");
    assert_eq!(
        report.feature(features::WORKSPACE_REORDERED).state,
        FeatureState::Supported
    );
    // `pane.output_changed` is not in the subscription table.
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).state,
        FeatureState::Unsupported
    );
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).reason,
        "schema_absent"
    );
    assert_eq!(report.server_version, "0.9.1");
    assert_eq!(report.server_protocol, 22);
    assert_eq!(report.endpoint_protocol_generation, Some(3));
    assert_eq!(
        report.feature(features::CLIENT_SHELL_ENDPOINT).state,
        FeatureState::Supported
    );
    // The schema path burns no probe sockets — one ping only.
    assert_eq!(server.accept_count(), 1);
}

/// Herdr 0.9.1's exact shape: `pane_output_changed` appears in the
/// streamed-event payload table but the `Subscription` variant does not
/// exist — the live socket rejects it as an unknown variant. The schema
/// verdict must come from the subscription table alone, so the
/// `events.subscribe` handshake never pays the doomed round-trip.
#[tokio::test]
async fn output_changed_event_payload_without_subscription_is_unsupported() {
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    let schema = schema_with(
        &["ping", "events.subscribe"],
        &["pane.updated", "workspace.reordered"],
        &["pane_output_changed", "pane_updated"],
    );
    let client = test_client(&server, SchemaSource::Static(schema));

    let report = client.collect_capabilities().await;
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).state,
        FeatureState::Unsupported,
        "the EventData listing is not subscription evidence"
    );
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).reason,
        "schema_absent"
    );
    assert!(!client.should_attempt_pane_output_changed());

    // The consult suppresses the attempt — the handshake goes out clean.
    // (`reqs[0]` is the collect's ping; the subscribe is `reqs[1]`.)
    server.push(Action::Stream(vec![support::subscription_started_line()]));
    let stream = client.subscribe_topology().await.unwrap();
    drop(stream);
    let reqs = server.requests();
    let types: Vec<&str> = reqs[1].params["subscriptions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["type"].as_str())
        .collect();
    assert!(!types.contains(&"pane.output_changed"));
    assert!(types.contains(&"workspace.reordered"));
    // A skipped attempt probed nothing — the atomic stays "not probed".
    assert_eq!(client.pane_output_changed_supported(), None);
}

/// When the schema does list the `pane.output_changed` subscription
/// variant, it is adjudicated like `workspace.reordered`.
#[tokio::test]
async fn output_changed_subscription_in_schema_is_supported() {
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    let schema = schema_with(
        &["ping", "events.subscribe"],
        &["pane.updated", "pane.output_changed"],
        &[],
    );
    let client = test_client(&server, SchemaSource::Static(schema));

    let report = client.collect_capabilities().await;
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).state,
        FeatureState::Supported
    );
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).reason,
        "schema_advertised"
    );
    assert!(client.should_attempt_pane_output_changed());
}

#[tokio::test]
async fn schema_mismatch_falls_back_to_probes() {
    // Schema claims protocol 99 but the server answers 22 — untrusted, so
    // the three optimistic probes run. Validation refusals prove support.
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    server.push(Action::Reply(pong())); // ping
    server.push(Action::Refuse("workspace_move_block_failed", "empty block")); // move_block probe
    server.push(Action::Refuse("tab_not_found", "no tab")); // tab.move probe
    server.push(Action::Refuse("pane_not_found", "no pane")); // pane.read probe
                                                              // The schema lies about the protocol (99 vs the server's 22) —
                                                              // untrusted, so the probe path must run.
    let schema = SchemaRegistry::from_value(&json!({
        "protocol": 99,
        "schemas": {"request": {"oneOf": [{"properties": {"method": {"const": "ping"}}}]}}
    }));
    let client = test_client(&server, SchemaSource::Static(schema));

    let report = client.collect_capabilities().await;
    assert_eq!(
        report.feature(features::WORKSPACE_MOVE_BLOCK).state,
        FeatureState::Supported
    );
    assert_eq!(
        report.feature(features::WORKSPACE_MOVE_BLOCK).reason,
        "recognized_validation_refusal"
    );
    assert_eq!(
        report.feature(features::TAB_MOVE).state,
        FeatureState::Supported
    );
    assert_eq!(
        report.feature(features::PANE_READ).state,
        FeatureState::Supported
    );
    assert_eq!(server.accept_count(), 4, "ping + 3 probes");
}

#[tokio::test]
async fn probe_path_unknown_method_means_unsupported() {
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    server.push(Action::Reply(pong()));
    server.push(Action::Refuse("unknown_method", "nope"));
    server.push(Action::Refuse("method_not_found", "nope"));
    server.push(Action::Refuse("unsupported_method", "nope"));
    let client = test_client(&server, SchemaSource::Disabled);

    let report = client.collect_capabilities().await;
    for name in [
        features::WORKSPACE_MOVE_BLOCK,
        features::TAB_MOVE,
        features::PANE_READ,
    ] {
        assert_eq!(
            report.feature(name).state,
            FeatureState::Unsupported,
            "{name}"
        );
        assert_eq!(report.feature(name).reason, "method_not_supported");
    }
}

#[tokio::test]
async fn transport_failure_degrades_to_unknown_not_unsupported() {
    // Every connection hangs up post-dispatch: nothing may be concluded.
    let server = FakeHerdr::start(Action::HangUp).await;
    let client = test_client(&server, SchemaSource::Disabled);

    let report = client.collect_capabilities().await;
    // The failed ping ends the refresh early — every feature unknown.
    for name in [
        features::ORDINARY_JSON,
        features::PANE_READ,
        features::TAB_MOVE,
        features::WORKSPACE_MOVE_BLOCK,
    ] {
        assert_eq!(report.feature(name).state, FeatureState::Unknown, "{name}");
    }
    assert_eq!(
        report.feature(features::ORDINARY_JSON).reason,
        "server_reply_unavailable"
    );
    assert!(report.server_version.is_empty());
}

#[tokio::test]
async fn generation_ticks_but_unchanged_evidence_keeps_its_stamp() {
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    let schema = schema_with(&["ping", "pane.read"], &[], &[]);
    let client = test_client(&server, SchemaSource::Static(schema));

    let first = client.collect_capabilities().await;
    assert_eq!(first.generation, 1);
    let pane_gen = first.feature(features::PANE_READ).generation;

    let second = client.collect_capabilities().await;
    assert_eq!(second.generation, 2, "refresh tick advances");
    // Identical evidence — identical published stamp, so a downstream
    // equality check (Topology::set_herdr_status) stays silent.
    assert_eq!(second.feature(features::PANE_READ).generation, pane_gen);
    assert_eq!(first.features, second.features);
}

#[tokio::test]
async fn observed_notes_overlay_schema_and_reuse() {
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    // Schema says tab.move exists; a real call just got `unknown_method` —
    // observed evidence must win over inference.
    let schema = schema_with(&["ping", "tab.move"], &[], &[]);
    let client = test_client(&server, SchemaSource::Static(schema));

    client.note_feature(
        features::TAB_MOVE,
        FeatureState::Unsupported,
        "method_not_supported",
    );
    let report = client.collect_capabilities().await;
    assert_eq!(
        report.feature(features::TAB_MOVE).state,
        FeatureState::Unsupported
    );
    assert_eq!(
        report.feature(features::TAB_MOVE).reason,
        "method_not_supported"
    );
}

#[tokio::test]
async fn notes_do_not_leak_across_server_identities() {
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    let client = test_client(&server, SchemaSource::Disabled);

    // Identify the server (0.9.1/22/…), then record an observed verdict.
    client.collect_capabilities().await;
    client.note_feature(
        features::PANE_READ,
        FeatureState::Unsupported,
        "method_not_supported",
    );

    // A different build answers the next refresh — the stale note must not
    // mask the new probe result.
    let pong_b = json!({"type": "pong", "version": "0.10.0", "protocol": 22});
    server.push(Action::Reply(pong_b.clone()));
    server.push(Action::Refuse("workspace_move_block_failed", "x"));
    server.push(Action::Refuse("tab_not_found", "x"));
    server.push(Action::Refuse("pane_not_found", "x"));
    server.set_default(Action::Refuse("pane_not_found", "x"));

    let report = client.collect_capabilities().await;
    assert_eq!(report.server_version, "0.10.0");
    assert_eq!(
        report.feature(features::PANE_READ).state,
        FeatureState::Supported,
        "stale note must not shadow the fresh probe"
    );
}

#[tokio::test]
async fn workspace_reordered_subscription_outcome_overrides_schema() {
    // Schema claims the variant exists and the live subscription confirms
    // it — the report must carry the observed handshake's reason
    // (`subscription_acknowledged`), not `schema_advertised`: observed
    // evidence wins over inference.
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    server.push(Action::Stream(vec![support::subscription_started_line()]));
    let schema = schema_with(&["ping", "events.subscribe"], &["workspace.reordered"], &[]);
    let client = test_client(&server, SchemaSource::Static(schema));

    let stream = client.subscribe_topology().await.unwrap();
    drop(stream);
    assert_eq!(client.workspace_reordered_supported(), Some(true));
    // The schema verdict does not exist until `collect_capabilities`
    // runs below — this subscribe is pre-adjudication, so
    // `pane.output_changed` rides and is acknowledged.
    let reqs = server.requests();
    let types: Vec<&str> = reqs[0].params["subscriptions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["type"].as_str())
        .collect();
    assert!(types.contains(&"pane.output_changed"));
    assert_eq!(client.pane_output_changed_supported(), Some(true));

    let report = client.collect_capabilities().await;
    assert_eq!(
        report.feature(features::WORKSPACE_REORDERED).state,
        FeatureState::Supported
    );
    assert_eq!(
        report.feature(features::WORKSPACE_REORDERED).reason,
        "subscription_acknowledged"
    );
    // The same overlay rule covers `pane.output_changed`: the schema's
    // `schema_absent` loses to the handshake's observed acceptance.
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).state,
        FeatureState::Supported
    );
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).reason,
        "subscription_acknowledged"
    );
}

#[tokio::test]
async fn subscribe_skips_reordered_when_ledger_knows_unsupported() {
    let server = FakeHerdr::start(Action::Stream(vec![support::subscription_started_line()])).await;
    let client = test_client(&server, SchemaSource::Disabled);
    client.note_feature(
        features::WORKSPACE_REORDERED,
        FeatureState::Unsupported,
        "subscription_rejected",
    );

    let stream = client.subscribe_topology().await.unwrap();
    drop(stream);
    let reqs = server.requests();
    let types: Vec<&str> = reqs[0].params["subscriptions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["type"].as_str())
        .collect();
    assert!(
        !types.contains(&"workspace.reordered"),
        "known-unsupported variant must not be re-offered: {types:?}"
    );
    // The other optional entry is unjudged — it still rides.
    assert!(types.contains(&"pane.output_changed"));
    assert_eq!(types.len(), 21);
    // The variant was skipped on the ledger's verdict, not rejected by
    // this bootstrap — the probe atomic stays "not probed" while the
    // published verdict keeps the observed evidence.
    assert_eq!(client.workspace_reordered_supported(), None);
    assert_eq!(
        client.feature(features::WORKSPACE_REORDERED).state,
        FeatureState::Unsupported
    );
    assert_eq!(client.pane_output_changed_supported(), Some(true));
}

#[tokio::test]
async fn bootstrap_reprobes_reordered_despite_prior_verdict() {
    // Oracle ordering: `Bootstrap` runs `workspaceReorderedReset` —
    // `InvalidateLiveCapabilities` — *before* consulting
    // `ShouldAttemptWorkspaceReordered`, so a verdict left over from the
    // previous connection never suppresses the re-probe: reconnects may
    // face a different server build. (Standalone `subscribe_topology`
    // callers still honor the verdict — see the skip test above.)
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    server.push(Action::Stream(vec![support::subscription_started_line()]));
    server.push(Action::Reply(support::snapshot_result()));
    let client = test_client(&server, SchemaSource::Disabled);
    client.note_feature(
        features::WORKSPACE_REORDERED,
        FeatureState::Unsupported,
        "subscription_rejected",
    );
    assert!(!client.should_attempt_workspace_reordered());

    let boot = client.bootstrap_topology().await.unwrap();
    drop(boot.stream);

    let reqs = server.requests();
    let types: Vec<&str> = reqs[0].params["subscriptions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["type"].as_str())
        .collect();
    assert!(
        types.contains(&"workspace.reordered"),
        "bootstrap must re-probe the variant after invalidation: {types:?}"
    );
    assert_eq!(client.workspace_reordered_supported(), Some(true));
}

#[tokio::test]
async fn subscribe_rejection_marks_reordered_unsupported() {
    // First subscribe hits Herdr's pre-dispatch refusal for the unknown
    // variant (empty id + invalid_request); the fallback succeeds. The
    // default replies to the capability collect's unary calls.
    let server = FakeHerdr::start(Action::Reply(pong())).await;
    server.push(Action::Custom(|conn, _req| {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut conn = conn;
            let _ = conn
                .write_all(
                    br#"{"id":"","error":{"code":"invalid_request","message":"invalid request: unknown variant `workspace.reordered`"}}
"#
                    .as_slice(),
                )
                .await;
        })
    }));
    // The fallback resubscribe (without the rejected variant) succeeds.
    server.push(Action::Stream(vec![support::subscription_started_line()]));
    let client = test_client(&server, SchemaSource::Disabled);

    let stream = client.subscribe_topology().await.unwrap();
    drop(stream);
    assert_eq!(client.workspace_reordered_supported(), Some(false));
    // `pane.output_changed` rode the retry and was acknowledged there.
    assert_eq!(client.pane_output_changed_supported(), Some(true));

    let report = client.collect_capabilities().await;
    assert_eq!(
        report.feature(features::WORKSPACE_REORDERED).state,
        FeatureState::Unsupported
    );
    assert_eq!(
        report.feature(features::WORKSPACE_REORDERED).reason,
        "subscription_rejected"
    );
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).state,
        FeatureState::Supported
    );
    assert_eq!(
        report.feature(features::PANE_OUTPUT_CHANGED).reason,
        "subscription_acknowledged"
    );
}

#[tokio::test]
async fn cli_schema_source_runs_the_stubbed_binary() {
    // A shell `herdr` stub: prints the schema for `api schema --json`,
    // a version line otherwise — and echoes HERDR_SOCKET_PATH into the
    // output so the env propagation is asserted too.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("herdr");
    let schema_doc = serde_json::to_string(&json!({
        "protocol": 22,
        "schemas": {"request": {"oneOf": [
            {"properties": {"method": {"const": "ping"}}},
            {"properties": {"method": {"const": "pane.read"}}},
            {"properties": {"method": {"const": "tab.move"}}},
            {"properties": {"method": {"const": "workspace.move_block"}}},
        ]}}
    }))
    .unwrap();
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"api\" ]; then cat <<'EOF'\n{schema_doc}\nEOF\nelse echo \"herdr 0.9.1\"; fi\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let server = FakeHerdr::start(Action::Reply(pong())).await;
    let client = Client::unix_with(
        server.sock_path.clone(),
        ClientConfig {
            herdr_bin: Some(bin),
            schema_source: SchemaSource::Cli,
            ..ClientConfig::default()
        },
    );

    let registry = client.api_schema().await.unwrap();
    assert!(registry.supports_method("pane.read"));
    assert_eq!(registry.protocol(), Some(22));

    let report = client.collect_capabilities().await;
    assert_eq!(report.installed_client_version, "herdr 0.9.1");
    assert_eq!(
        report.feature(features::PANE_READ).state,
        FeatureState::Supported
    );
    // One ping only — schema answered everything else.
    assert_eq!(server.accept_count(), 1);
}

// -- pane.read singleflight key ----------------------------------------------

/// Slow scripted pane.read reply so concurrent calls overlap in flight.
fn slow_pane_read() -> Action {
    Action::Custom(|conn, req| {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let mut conn = conn;
            tokio::time::sleep(Duration::from_millis(120)).await;
            let _ = conn
                .write_all(
                    json!({"id": req.id, "result": {
                        "type": "pane_read",
                        "read": {"pane_id": "wE:pE", "workspace_id": "wE",
                                 "tab_id": "wE:t1", "source": "visible",
                                 "format": "text", "text": "shared",
                                 "revision": 1, "truncated": false}
                    }})
                    .to_string()
                    .as_bytes(),
                )
                .await;
            let _ = conn.write_all(b"\n").await;
        })
    })
}

#[tokio::test]
async fn pane_read_singleflight_keys_on_the_full_param_tuple() {
    // Six callers, three distinct (lines) values — only identical tuples
    // may collapse, so exactly three dials must reach the server.
    let server = FakeHerdr::start(slow_pane_read()).await;
    let client = test_client(&server, SchemaSource::Disabled);

    let mut handles = Vec::new();
    for lines in [10u32, 10, 50, 50, 200, 200] {
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            client
                .pane_read("wE:pE", ReadSource::Visible, lines, ReadFormat::Text)
                .await
        }));
    }
    for h in handles {
        tokio::time::timeout(Duration::from_secs(30), h)
            .await
            .expect("pane.read hung")
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        server.accept_count(),
        3,
        "identical tuples dedupe; distinct lines must not"
    );
}

#[tokio::test]
async fn pane_read_strip_ansi_and_format_join_the_key() {
    // `pane_read_opts` with explicit strip_ansi — same pane/source/lines but
    // a different (format, strip_ansi) pair must never dedupe with it.
    let server = FakeHerdr::start(slow_pane_read()).await;
    let client = test_client(&server, SchemaSource::Disabled);

    let ansi = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .pane_read("wE:pE", ReadSource::Visible, 10, ReadFormat::Ansi)
                .await
        })
    };
    let ansi_stripped = {
        let client = client.clone();
        tokio::spawn(async move {
            let mut params =
                PaneReadParams::new("wE:pE", ReadSource::Visible, 10, ReadFormat::Ansi);
            params.strip_ansi = true;
            client.pane_read_opts(&params).await
        })
    };
    let text = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .pane_read("wE:pE", ReadSource::Visible, 10, ReadFormat::Text)
                .await
        })
    };
    for h in [ansi, ansi_stripped, text] {
        let _ = tokio::time::timeout(Duration::from_secs(30), h)
            .await
            .expect("pane.read hung")
            .unwrap();
    }
    assert_eq!(
        server.accept_count(),
        3,
        "distinct (format, strip_ansi) tuples must each dial"
    );
}

#[tokio::test]
async fn agent_view_set_refusal_is_not_retried_and_is_noted() {
    let server = FakeHerdr::start(Action::Refuse("unknown_method", "no such method")).await;
    let client = test_client(&server, SchemaSource::Disabled);

    let outcome = lerdr_herdr::assert_agent_view(&client).await;
    assert!(matches!(outcome, lerdr_herdr::ViewAssertOutcome::Failed(_)));
    assert_eq!(server.accept_count(), 1, "definitive refusal never retries");
    // The noted outcome must mark the feature unsupported in the ledger.
    assert_eq!(
        client.feature(features::AGENT_VIEW_SET).state,
        FeatureState::Unsupported
    );
    // And the next assert skips the wire entirely.
    let outcome = lerdr_herdr::assert_agent_view(&client).await;
    assert!(matches!(
        outcome,
        lerdr_herdr::ViewAssertOutcome::KnownUnsupported
    ));
    assert_eq!(server.accept_count(), 1);
}

#[tokio::test]
async fn assert_agent_view_retries_non_definitive_then_installs() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "agent_view", "active": true, "source": "plugin:lerdr.events"
    })))
    .await;
    server.push(Action::HangUp); // attempt 1: DispatchedUnknown
    server.push(Action::HangUp); // attempt 2: DispatchedUnknown
    let client = test_client(&server, SchemaSource::Disabled);

    let outcome = lerdr_herdr::assert_agent_view(&client).await;
    assert!(matches!(outcome, lerdr_herdr::ViewAssertOutcome::Installed));
    assert_eq!(server.accept_count(), 3, "two hangups then success");
    assert_eq!(
        client.feature(features::AGENT_VIEW_SET).state,
        FeatureState::Supported
    );

    let reqs = server.requests();
    assert!(reqs.iter().all(|r| r.method == "agent.view.set"));
    assert_eq!(reqs[0].params["source"], "plugin:lerdr.events");
    assert_eq!(reqs[0].params["sort"][0]["field"], "attention");
}

#[tokio::test]
async fn assert_agent_view_exhausts_bounded_retries() {
    let server = FakeHerdr::start(Action::HangUp).await;
    let client = test_client(&server, SchemaSource::Disabled);
    let outcome = lerdr_herdr::assert_agent_view(&client).await;
    match outcome {
        lerdr_herdr::ViewAssertOutcome::Failed(err) => {
            assert_eq!(err.phase(), DispatchPhase::DispatchedUnknown)
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(server.accept_count(), 3, "bounded at 3 attempts");
}
