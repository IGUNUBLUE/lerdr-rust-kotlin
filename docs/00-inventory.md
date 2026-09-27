# 00 — Inventory: what exists today

The product is now the Rust workspace under `relay/` plus the Kotlin/Compose
app under `app/`. The current source, numbered specifications, and frozen
fixtures are authoritative. A concise retired-implementation provenance note
appears at the end of this document.

## Current implementation

### Rust relay (`relay/`)

| Crate | Role |
|---|---|
| `lerdr-core` | `protocol v3` types, action catalog, `CAPABILITIES` — the declared capability set (see [03 §4.1](03-protocol.md)) |
| `lerdr-e2ee` | `herdr-e2ee-v2` handshake + AES-256-GCM session codecs; deployed WebSocket transport uses JSON frames, while the binary codec remains fixture-covered |
| `lerdr-herdr` | Unix-socket API client, event stream, capability evidence probing |
| `lerdr-coord` | The coordinator: topology actor, snapshot projection, action handlers (speech, workspaces, conversation, push, uploads), `lerdr-relay` binary |
| `lerdr-relay` | WebSocket transport, per-client sessions, send buffer, watch/delta loops |
| `lerdr-fixture` | Frozen golden-vector fixtures loader (`fixtures/`) |
| `lerdr-shadow` | Determinism harness — replays sessions against recorded behavior |

Architectural improvements over the original design: actor topology with
bounded channels, watch/broadcast cells for state, explicit commit kinds,
bounded caches, durable queue salvage/quarantine, runtime schema
introspection, explicit action receipts.

### Android app (`app/`)

nowinandroid-style modules: `app` + `core/{protocol,e2ee,transport,model,
data,store,conversation,terminal,designsystem,testing}` + `navigation`.
Jetpack Compose / Material 3 Expressive. Capabilities are consumed as the
intersection of advertised ∩ announced — features gate on live capability
names.

### Transport decision

Tailscale only. `tailscaled` owns the tailnet listener
(`tailscale serve` persists `tcp://<tailnet-ip>:<port> → localhost:<port>`);
the relay binds loopback. No gateway, no Cloudflare tunnel, no WebRTC/hybrid
transport — those paths were deliberately not reimplemented
([12 — transport alternatives](12-transport-alternatives.md)).

### Deployment

| Piece | Location |
|---|---|
| Service | `lerdr.service` systemd user unit — `Restart=on-failure`, `EnvironmentFile=<runtime>/relay.env`, journald logs |
| Env file | `$LERDR_RELAY_ENV` (default `~/.config/lerdr/relay.env`) — its **directory is the runtime dir** |
| Runtime dir | `~/.config/lerdr/` — `device-auth/`, `push/` (VAPID), `activity/`, `audit/`, `uploads/`, `support-state.json`. Persistent across reboots; never `/tmp` |
| Install | `plugin/scripts/install-tailscale-service.sh` (also retires legacy `herdr-mobile-relay.service`) |
| Dev redeploy | `plugin/scripts/deploy-relay.sh` — release build → restart → health gate; `SKIP_BUILD=1` to skip the build |
| Health | `GET /healthz`, `/readyz` on the loopback bind |
| Tailnet publish | `plugin/scripts/tailscale-serve.sh` — serve config persists in tailscaled |

Ops under systemd (the PID rotates on every restart — never script against
it): restart `systemctl --user restart lerdr.service`, logs
`journalctl --user -u lerdr.service -f`, re-arm a pairing invitation
`systemctl --user kill -s USR1 lerdr.service`. The file log was only a
`nohup` artifact of pre-service dev runs.

### Capability contract

`docs/03` §4.1 is the canonical table; `tests/capability_contract.rs` fails
CI if `CAPABILITIES` drifts from it. `agent_response_copy` stays
unadvertised by design (no clipboard backend). `pane_output_changed`
activates by itself once Herdr upstream adds the subscription variant.

---

## Retired implementation provenance

Lerdr began as `IGUNUBLUE/lerdr` at `d01ec32` (v0.26.3): a Go relay with a
web client. That retired codebase informed the initial scope map and the
committed fixture corpus, but it is not an implementation target or a source
of present-day requirements.

The authoritative replacement is this repository: the current Rust and Kotlin
sources, the numbered specifications, and the frozen vectors under
`fixtures/`. Historical source paths and implementation notes remain in the
individual specs only to explain the origin of immutable protocol details.
