# 00 — Inventory: what exists today

The product is now the Rust workspace under `relay/` plus the Kotlin/Compose
app under `app/`. The Go catalog in the second half of this document is the
original scope map — provenance for *what* was built, not the source of
truth for *how* it works.

## Current implementation

### Rust relay (`relay/`)

| Crate | Role |
|---|---|
| `lerdr-core` | `protocol v3` types, action catalog, `CAPABILITIES` — the declared capability set (see [03 §4.1](03-protocol.md)) |
| `lerdr-e2ee` | `herdr-e2ee-v2` handshake + AES-256-GCM session codecs (JSON frame codec; no binary codec, no WebRTC) |
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

## Go source inventory (provenance)

Source: `IGUNUBLUE/lerdr` @ `d01ec32` (v0.26.3). Numbers are non-test LOC
unless noted. Total: **291 Go files, ~57k non-test LOC** across 42 packages;
frontend ~15.5k LOC of TS plus Svelte components; `src-tauri` is a thin
native shell. This Go implementation is retired — it survives only as the
scope map and behavioral oracle below.

## Binaries

| Binary | Role |
|---|---|
| `lerdr` | Per-computer relay. Serves the PWA (`web/` embedded via ldflags or disk), terminates E2EE WebSocket, talks to Herdr over Unix socket, pushes web-push notifications, watches panes, coordinates multi-device actions. |
| `lerdr-gateway` | Optional public rendezvous: multiplexed WS carrying per-relay client connections for phones that have no direct path. Stateless — copies already-encrypted frames. |
| `fake-herdr` | Herdr simulator for tests — becomes the contract harness for the Rust implementation. |

## Go packages by reimplementation weight

### Core — reimplement first

| Package | LOC | What it does |
|---|---|---|
| `internal/app` | ~4.9k | HTTP routes, WS session lifecycle, action dispatch, pane watch loop, uploads wiring, hybrid (WS→WebRTC) orchestration |
| `internal/transport` | ~2.3k | WS framing, per-client send buffer (64 msgs / 4 MiB, coalescing, eviction), E2EE handshake + AES-GCM sessions, WebRTC link handling |
| `internal/protocol` | ~0.5k | `protocol v3`: `Inbound` envelope, ~70-action catalog, receipts, error shaping |
| `internal/herdr` | ~3.5k | Unix-socket API client (~30 methods), CLI fallback, event-stream subscription, pane-read capabilities |
| `internal/panedelta` | ~0.3k | Line-level diff between pane snapshots (ops: copy/insert/delete) |
| `internal/panesize` | ~0.6k | Column/row lease manager — resizes the shared pane through `stty`, TTL ~120 s |
| `internal/deviceauth` | ~0.9k | Paired-device store: credentials, invitations, roles (reader/controller), revocation |
| `internal/state`/`session` | ~1k | Agent inventory snapshots, targeted lookups, generation counters |

### Product features — implement second

| Package | LOC | What it does |
|---|---|---|
| `internal/conversation` | ~7k | Reads native agent session files (Claude Code, Codex, OpenCode, Hermes, Pi, OMO, browser chains) → structured `Entry{role,text,tools[]}` pages. **The semantic layer the new app is built on.** |
| `internal/question` | ~2.8k | Parses pane content → structured approvals/questions (`QuestionInteraction`, options, navigation state) |
| `internal/coordinator` | ~5k | Per-pane command scheduler: queues mutating actions, leases, receipts, multi-device ordering, ledger |
| `internal/push` | ~2.4k | Web Push (VAPID self-generated), per-device policy, snooze, queue, "viewed pane" suppression |
| `internal/activity` | ~0.8k | Activity journal (audited mutations, attention events) |
| `internal/workspace` | ~0.5k | Workspace tree/file/git-status/diff read-only inspection |
| `internal/slashcmd` | ~2.4k | Slash-command discovery per agent kind |
| `internal/upload` | ~1.3k | Chunked phone→computer uploads (begin/chunk/finish/cancel) |
| `internal/copyresponse` | ~0.5k | "Copy agent response" extraction |
| `internal/speech` | ~1k | TTS voice catalog + `speak_text` |
| `internal/agentroots` | ~0.5k | Locates per-agent session file roots |

### Infrastructure — implement third

| Package | LOC | What it does |
|---|---|---|
| `internal/gateway` + `gatewaywire` | ~2.8k | Public gateway relay (the `lerdr-gateway` binary), STUN, quotas |
| `internal/webrtclink` | ~1k | Server side of the direct WebRTC DataChannel upgrade (`herdr-dc-v1`) |
| `internal/appdeploy` | ~1.5k | Deploys/updates the PWA on the relay host |
| `internal/update` | ~1.3k | Self-update checker + staged installer |
| `internal/release` | ~0.6k | Release manifest + web bundle validation |
| `internal/config`, `localize`, `audit`, `noecho`, `framing`, `history`, `profiles`, `portmap`, `reachability`, `stablestate`, `support`, `web`, `clipboard`, `eventhook`, `seqmatch`, `setuphelper` | ~6k | Config loading, i18n error strings, audit log, secret handling, UPnP/NAT-PMP, QR/setup helpers, misc |

## The wire protocol surface (what Kotlin must speak)

- **Transport**: WSS (direct/Tailscale/Cloudflare) or gateway-relayed, with
  optional WebRTC DataChannel upgrade.
- **Handshake**: `herdr-e2ee-v2` — P-256 ECDH + HMAC proof over pairing
  credential/invitation → HKDF-SHA256 → AES-256-GCM, per-direction keys,
  monotonic sequence nonces. Two frame codecs: JSON (base64 ciphertext) and
  binary. Full spec in [03 — Protocol](03-protocol.md).
- **Inbound**: ~70 actions in `internal/protocol/protocol.go` — reads
  (`read_pane`, `workspace_tree`, `get_conversation_history`…), mutations
  (`send_text`, `send_keys`, `respond`, `answer_question`, `agent_start`…),
  admin (`device_list`, `revoke_device`, `push_policy_set`…), realtime
  (`watch_pane`, `pane_applied`, `acknowledge_pane`, `lease_pane_size`…).
- **Outbound**: `agents`/`agent_list` snapshots, `workspaces`, `pane_content`
  (full frames), `pane_delta`/`pane_resync` (line diffs with fingerprint
  chain + ack gate), `question`/`attention` events, `activity`,
  `command_result`, `action_receipt`, `push_config`, `update_status`,
  `session_snapshot`, `webrtc_*` signaling, uploads progress.
- **Ack gate**: pane deltas chain on fingerprints; each `ack_required` frame
  must be answered with `pane_applied` or the watch stalls (timeout →
  resync). The Kotlin client must implement this exactly — it is the
  correctness boundary for realtime.

## Frontend feature surface (what the app must cover)

22 Svelte components + ~40 lib modules. Feature checklist:

- **Agents**: multi-relay merged inventory, status grouping, workspace
  grouping, attention pinning, rename/clear/restart/stop, start new agent
- **Terminal**: ANSI render, virtualized scrollback, deltas, find-in-buffer,
  size leases, send keys/text/prompt, slash commands, composer with drafts,
  attachments upload
- **Approvals/questions**: structured `QuestionInteraction` forms
  (single/multi select, other-text, back/next navigation, chat fallback)
- **Workspaces**: list/create/rename/reorder/close, tabs, worktrees
  (list/create/open/remove), file tree, file viewer (text/image), git
  status + diffs
- **Conversation**: paginated native history with tool cards, OMO plan
  panel, search
- **Activity**: journal view + detail, copy response
- **Notifications**: push subscribe/policy/snooze/test, push-open deep
  resolution to a target pane
- **Pairing/devices**: QR scan, clipboard link, device list/rename/revoke,
  invitation creation, role handling (reader vs controller)
- **Misc**: settings (per-relay config, transports), biometric lock,
  haptics, speech (speak responses, voice install/remove), self-update UI,
  app-deploy UI, global jump (⌘K-style), lock screen

## Native surface the Tauri shell provides today

Just five plugins — all trivial in native Kotlin:

| Tauri plugin | Kotlin equivalent |
|---|---|
| `notification` | `NotificationManager` + channels + `POST_NOTIFICATIONS` |
| `biometric` | `BiometricPrompt` (androidx.biometric) |
| `clipboard-manager` | `ClipboardManager` |
| `haptics` | `Vibrator`/`HapticFeedback` |
| `barcode-scanner` | ML Kit Barcode Scanning or ZXing |

Plus deep links (`lerdr://` pairing) → Android App Links / intent filters.
No fs/shell/http plugins — the sandbox is already minimal.

## Key non-obvious behaviors to preserve

1. **Pane size lease**: the client leases cols×rows (40–240 × 10–120), the
   relay `stty`s the shared pane, TTL ~120 s with renewal; release on
   background. The Kotlin terminal must implement this or the pane renders
   at desktop width.
2. **Send-buffer contract**: 64 messages / 4 MiB per client; `replaceable`
   state messages coalesce; oversize or overflow → client evicted. Kotlin
   must expect eviction and reconnect cleanly.
3. **Delta chain**: `pane_delta` carries `base_fingerprint` +
   `frame_fingerprint`; client applies line ops onto its copy, acks via
   `pane_applied`. Mismatch → `pane_resync`.
4. **Coordinator ordering**: mutating actions on the same pane serialize
   through per-pane slots — receipts carry `phase` progression
   (`prepared` → `awaiting_evidence` → `confirmed`). The UI must surface
   pending/confirmed states.
5. **Invitation auth**: a device can pair without ever seeing the relay
   key — `auth_kind: "invitation"` derives gateway reachability one-way
   from the relay key. Reader role = read-only catalog enforced server-side.
6. **Conversation browsing is lazy+progressive**: `get_conversation_history`
   pages opaque cursors, may return `state: preparing` with progress —
   the native app must handle multi-stage loads, not just lists.
