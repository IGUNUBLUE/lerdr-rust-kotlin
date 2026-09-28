# 08 — The Herdr boundary (deep dive) + improved service topology

This document defines Lerdr's contract with Herdr. It is implemented by
`lerdr-herdr` and anchored by the current Herdr schema/capability facts
recorded here; the retired Go client is historical provenance only.

## The wire contract

**Transport**: Unix socket (path from config/env), newline-delimited JSON.
One request per connection — Herdr closes the socket after each response.
Connection pooling is impossible by design; each Lerdr read dials a fresh
connection per attempt.

```jsonc
→ {"id":"lerdr-api-42","method":"pane.read","params":{...}}\n
← {"id":"lerdr-api-42","result":{"type":"pane_read","read":{...}}}\n
  // or {"id":…,"error":{"code":"…","message":"…"}}
  // pre-dispatch errors: id:"", code:"invalid_request"
```

Response bounded at `maxOutputBytes`; `id` echo must match (mismatch =
transport corruption, not an app error).

### Dispatch-boundary error taxonomy — encode as a Rust enum

This is the semantic heart of the client; it decides what a failed
mutation *means*:

| Variant | Meaning | Retryable? |
|---|---|---|
| `NotStarted` | No request bytes reached the socket | safe to retry |
| `Refused{code,message}` | Structured Herdr error — the op did NOT apply | no; surface code |
| `DispatchedUnknown` | Bytes were written, no usable response — the op **may have applied** | only idempotent ops |

```rust
enum HerdrError {
    NotStarted(io::Error),
    Refused { code: String, message: String },
    DispatchedUnknown(Box<HerdrError>),
}
```

### Method inventory (socket path)

`agent.list`, `pane.list`, `pane.read`, `pane.send_input`, `tab.list`,
`tab.move`, `workspace.list`, `workspace.close`, `workspace.move`,
`workspace.move_block`, plus the event subscription (request id
`lerdr-events`). Everything else falls back to `herdr` CLI subprocess —
each subprocess call = fork+exec+JSON parse.

`pane.read` params: `{pane_id, source: "recent_unwrapped"|…, lines,
format: "ansi"|"plain", strip_ansi}` → `{type:"pane_read", read:{text,
truncated}}`.

`pane.send_input` params: `{pane_id, text?, keys?}` — routed through
Herdr's input method so paste-mode is honored (send_input.go).

`herdr terminal session observe <pane_id>` (CLI subprocess, one per
watch) — pushes NDJSON `terminal.frame` records (`seq`, `full`,
base64 ANSI `bytes`, `width`/`height`) rendered at the pane's real
geometry, read-only; `terminal.closed` reports pane exit. The relay
feeds the bytes to a `vt100` screen emulator and renders the same
`(source, format, lines)` text `pane.read` produces — push instead of
poll, no mouse-scroll scrollback harvesting. `LERDR_PANE_STREAM=off`
disables it; spawn failure/EOF/`closed` fall back to `pane.read`
polling.

### Event stream — the reactive spine

`Bootstrap()`: subscribe (`lerdr-events`, topology subscription set) →
returns `EventStream` + `SessionSnapshot` (workspaces, tabs, panes,
agents, focus ids, `version`, `protocol`, `revision`,
`state_change_seq`). Events arrive canonicalized; legacy names map
(`workspace_created` → `workspace.created`; 26 aliases). Fallback:
`workspace.reordered` unsupported → re-subscribe without it + probe.

26 canonical events: `pane.output_changed`, `pane.agent_status_changed`,
`pane.agent_detected`, `pane.{created,closed,updated,focused,moved,
exited}`, `workspace.*`, `worktree.*`, `tab.*`, `layout.updated`.

### Capability flags (probed, cached)

`ordinary_json`, `workspace.move_block`, `workspace.reordered`,
`pane.read`, `tab.move`, `client_shell.endpoint`, `direct_terminal`.
Each carries `FeatureState{supported|unsupported|unknown}` + evidence.
Rust side: `Capabilities` struct, refreshed on reconnect, exposed to
clients in `push_config.herdr_status`.

## The service topology (Rust)

Lerdr uses an explicit actor model — **channels own the boundaries, tasks own
the state**:

```
                    Herdr (unix socket / CLI)
                          │
        ┌─────────────────┼──────────────────────┐
        │                 │                      │
┌───────▼───────┐ ┌───────▼────────┐   ┌─────────▼────────┐
│ herdr::Client │ │ herdr::Events  │   │ herdr::Capabs    │
│ req: fresh    │ │ supervised sub │   │ probe + TTL cache│
│ dial, semaphore│ │ reconnect +    │   │                  │
│ singleflight, │ │ bootstrap resync│  │                  │
│ dispatch errs │ └───────┬────────┘   └──────────────────┘
└───────┬───────┘         │ watch::Sender<Topology>
        │                 ▼
        │         ┌──────────────┐
        │         │ TopologyActor │── watch::Receiver<Topology>
        │         │ (projection:  │    to all subscribers
        │         │  agents, ws,  │
        │         │  focus, revs) │
        │         └──────┬───────┘
        │                │ notify (changed agent ids)
        │                ▼
        │         ┌──────────────┐   spawn per watch_pane
        │         │ PaneWatch     │   task — probe→read→delta→
        │         │ task ×pane    │   fingerprint chain
        │         └──────┬───────┘
        │                │ mpsc (ordered frames)
        ▼                ▼
┌─────────────────────────────────┐
│ CoordinatorActor                 │  per-pane mpsc queue,
│ (mutating action ordering,       │  lease table, receipts,
│  dispatch-boundary propagation)  │  ledger
└───────┬──────────────────────────┘
        │ mpsc::Sender<OutboundMsg>
        ▼
┌─────────────────────────────────┐
│ SessionActor ×N (per WS client)  │  owns: e2ee session, send
│  write pump drains bounded queue │  buffer (64/4MiB), coalescing,
│  + WS socket; read pump → cmds   │  eviction on lag
└─────────────────────────────────┘
```

### Rules this topology enforces

1. **No `Mutex<HashMap>` for hot state.** Topology lives in one actor;
   readers hold `watch::Receiver`s. Session outbound is bounded `mpsc`;
   lag evicts the session.
2. **Singleflight on Herdr reads.** N watchers of the same pane → one
   `pane.read` in flight; results fan out. This bounds Herdr load on the hot
   phone-driven path.
3. **Semaphore around dials.** Each request = a socket; cap concurrent
   dials (e.g. 32) so a thundering herd of watch probes cannot fd-storm Herdr.
4. **Dispatch boundary travels end-to-end.** `HerdrError` maps onto
   `ActionReceipt.phase`: `NotStarted` → `failed_before_dispatch`,
   `DispatchedUnknown` → `dispatched_unknown`, `Refused` → `confirmed`
   error path. The mobile UI gets honest receipts.
5. **Event-driven, not polled.** Inventory is a projection of the event
   stream + bootstrap, with polling only as an explicit fallback.
6. **Watch tasks die with their subscribers.** `tokio::select!` on
   unsubscribe/shutdown; no orphan reads.

## Kotlin app modules

The current Gradle settings are authoritative; the Android project contains:

```
app/
├── core/{model,protocol,e2ee,terminal,testing,transport,store,
│         conversation,designsystem,data}
├── navigation/
└── app/
```

The app uses Flow-backed repositories, ViewModel + StateFlow UI state,
fixture-driven protocol/terminal/crypto tests, and a foreground connection
service. See [02 — Target architecture](02-architecture.md) for the current
module inventory and dependencies.

### App integration rules

- Repositories expose `Flow`; UI collects lifecycle-aware state.
- Models crossing into Compose are `@Immutable`/`@Stable` where applicable,
  and lazy lists use stable keys.
- Tests use fakes in `:core:testing` and frozen fixtures for
  protocol/terminal/crypto behavior.
- The foreground service pins the long-lived connection only while push
  cannot reach a dead process; with a subscribed distributor the socket
  is free to die with the process.
- Screen ViewModels own screen state; transport and synchronized store state
  survive configuration and navigation changes.

## Repository skills

The repository provides `herdr-api` (boundary rules), `protocol-parity`
(fixture/vector workflow), `rust-relay` (actor topology + error taxonomy), and
`android-app` (module + Compose rules). The legacy skill name
`protocol-parity` means conformance to this repository's frozen contract, not
comparison with another implementation.

## Recorded Herdr integration findings

The following are the Herdr API facts Lerdr relies on. They are recorded here
so Lerdr's implementation guidance remains self-contained; the installed
runtime schema is the capability-discovery input.

- **Runtime schema introspection**: `herdr api schema --json` dumps the
  installed API's full JSON Schema. `lerdr-herdr` gains a `SchemaRegistry`:
  enumerate methods + event types at connect, build the exact capability
  table, degrade features deterministically. Replaces probe-by-failure.
- **`events_lost` recovery is specified, not implied**: on `events_lost`
  Herdr closes the subscription connection. Recovery = resubscribe, wait
  `subscription_started`, pull `session.snapshot`, treat subsequent events
  as *invalidation signals* (serialize refreshes; re-read if events arrive
  mid-read). Snapshots and events share **no sequence boundary** — never
  replay buffered events onto a snapshot. Encode this loop verbatim in the
  events supervisor.
- **Waits are first-class**: `agent.wait{until:[blocked|done|...]}`,
  `events.wait{match_event}`, `pane.wait_for_output{match:{substring|
  regex}}`. Question/attention detection goes event-driven; polling becomes
  fallback, not primary.
- **`agent.view.set`**: transient declarative filter+sort projection
  (`plugin:<id>` source) that drives the sidebar **and Herdr's own mobile
  Agents list**. The slot is a single global last-writer-wins resource,
  so asserting lerdr's canonical attention-sorted view is opt-in:
  `LERDR_RELAY_AGENT_VIEW=on` (default off, like herdr-radar's own view
  toggle) installs it on the first sync and reapplies it from the
  `[[startup]]` hook — never on event-stream resubscribes, which cannot
  lose it.
- **`client_shell.surface.set` + `command.invoke`**: Herdr's designed
  remote-UI surface (endpoint generation negotiation, `surface_interest`,
  `health_check` capabilities). Phase-2 investigation: the Kotlin app may
  consume client-shell projections directly.
- **`notification.show`**: desktop toasts for phone-originated actions.
- **`plugin.pane.open` placements**: `overlay|popup|split|tab|zoomed`;
  popup supports `width`/`height` — setup pickers become modals.
- **Socket paths**: `~/.config/herdr/herdr.sock` or
  `~/.config/herdr/sessions/<name>/herdr.sock`; env resolution order
  `--session` > `HERDR_SOCKET_PATH` > `HERDR_SESSION` > default.
- **`server.live_handoff`**: upgrades the server without dropping
  sessions; plugin `[[startup]]` hooks re-run on handoff — ours must
  re-assert socket/event/view state.
