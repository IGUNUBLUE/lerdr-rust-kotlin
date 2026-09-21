//! Post-handshake snapshot composition (`sendConnectionSnapshot` parity).
//!
//! Order matters — the oracle sends `push_config` first, then the
//! inventory/state frames, so clients see capabilities before content.

use std::sync::Arc;

use lerdr_core::json::MaybeNull;
use lerdr_core::protocol::{
    AgentsMessage, HerdrStatus, HerdrStatusMessage, Outbound, PushConfig, WorkspacesMessage,
    CAPABILITIES, VERSION,
};

use crate::topology::Topology;

/// The frames pushed to a client right after the E2EE handshake commits.
///
/// Baseline (Phase-1): `push_config` + `workspaces` + `agents` +
/// `herdr_status`. The oracle additionally sends `activity_history`,
/// `push_policy`, `speech_voices`, `update_status`, `inventory_status` —
/// those subsystems land in later slices; their absence is honest state,
/// not an error (each frame is independently optional on the client).
pub fn compose_snapshot(topology: &Topology) -> Vec<Outbound> {
    let status = herdr_status(topology);
    vec![
        Outbound::PushConfig(Box::new(PushConfig {
            r#type: "push_config".to_owned(),
            protocol: VERSION,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            release_version: env!("CARGO_PKG_VERSION").to_owned(),
            capabilities: MaybeNull::Value(CAPABILITIES.iter().map(|s| s.to_string()).collect()),
            herdr_status: status.clone(),
            ..PushConfig::default()
        })),
        Outbound::HerdrStatus(HerdrStatusMessage {
            status: Some(MaybeNull::Value(status)),
            r#type: "herdr_status".to_owned(),
            ..HerdrStatusMessage::default()
        }),
        Outbound::Workspaces(WorkspacesMessage {
            workspaces: Some(MaybeNull::Value(topology.workspaces())),
            r#type: "workspaces".to_owned(),
        }),
        Outbound::Agents(AgentsMessage {
            agents: Some(MaybeNull::Value(topology.agents())),
            r#type: "agents".to_owned(),
        }),
    ]
}

/// `herdr_status` payload from the projection — `server_version`/`protocol`
/// come from the snapshot envelope, `health_check` from staleness.
fn herdr_status(topology: &Topology) -> HerdrStatus {
    HerdrStatus {
        server_version: topology.snapshot.version.clone(),
        server_protocol: topology.snapshot.protocol as i64,
        server_protocol_known: true,
        health_check: Some(!topology.stale),
        ..HerdrStatus::default()
    }
}

/// Broadcast frames for one topology revision — the per-connection
/// forwarder sends these when the watch fires (all replaceable, so a burst
/// of revisions coalesces in the client's send buffer).
pub fn topology_broadcast(topology: &Arc<Topology>) -> Vec<Outbound> {
    vec![
        Outbound::HerdrStatus(HerdrStatusMessage {
            status: Some(MaybeNull::Value(herdr_status(topology))),
            r#type: "herdr_status".to_owned(),
            ..HerdrStatusMessage::default()
        }),
        Outbound::Workspaces(WorkspacesMessage {
            workspaces: Some(MaybeNull::Value(topology.workspaces())),
            r#type: "workspaces".to_owned(),
        }),
        Outbound::Agents(AgentsMessage {
            agents: Some(MaybeNull::Value(topology.agents())),
            r#type: "agents".to_owned(),
        }),
    ]
}
