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

`herdr terminal session observe <pane_id> --cols C --rows R` (CLI
subprocess, one per watch) pushes NDJSON `terminal.frame` records (`seq`,
`full`, base64 ANSI `bytes`, `width`/`height`); `terminal.closed` reports
pane exit. Capture uses the confirmed native controller geometry when a
size lease owns the grid, otherwise the desktop's native geometry.
The relay feeds bytes to a `vt100` screen emulator and renders the same
`(source, format, lines)` text `pane.read` produces — push instead of poll,
without mouse-scroll harvesting. `LERDR_PANE_STREAM=off` disables it;
spawn failure/EOF/`closed` fall back to `pane.read` polling.

Size leases own one `herdr terminal session control <pane_id> --cols C
--rows R` subprocess per pane, without `--takeover`. This resizes both
Herdr's native VT grid and the process PTY; `stty` alone resizes only the
PTY and can clip a full-screen CLI's prompt/footer in captured frames.
The controller receives `{"type":"terminal.resize","cols":C,"rows":R}`
and confirms a newer matching geometry frame before reporting success.
Unchanged live renewals do not repaint. Grace expiry, disconnect expiry
and relay shutdown release only the owned controller using
`{"type":"terminal.release"}`, returning geometry to the latest desktop
layout. An existing external controller is never forcibly replaced.

For live verification, build the executable with
`cargo build -p lerdr-coord --bin lerdr-relay`. The `lerdr-relay` package is
the transport library; building it alone does not refresh the executable.

### Event stream — the reactive spine

`Bootstrap()`: discover current pane IDs → subscribe (`lerdr-events`,
global topology events plus per-pane lifecycle entries) → read an authoritative
`SessionSnapshot` (workspaces, tabs, panes, agents, focus IDs, `version`,
`protocol`, `revision`, `state_change_seq`). Rebuild the subscription if pane
membership changed between discovery and the snapshot. Creation, closure and
cross-workspace moves also trigger a new subscription and snapshot.
Membership-triggered resubscription first forwards the complete bootstrap gap
and the live burst already queued at the trigger. The live drain uses a fixed
queue count, so continued traffic cannot postpone subscription refresh forever.
These remain invalidations, with the gap semantics below.

`pane.agent_status_changed` requires a concrete `pane_id`; omitting it rejects
the entire handshake. Events arrive canonicalized; legacy names map
(`workspace_created` → `workspace.created`; 26 aliases). A named unknown-variant
refusal drops only that optional variant. Herdr 0.9.3 decoder refusals can echo
`lerdr-events`; this handshake recognizes both echoed and empty IDs, without
changing the dispatch taxonomy for other RPCs.

Resync adopts sampled lifecycle status. Events buffered during the snapshot
gap trigger fresh reads, never status replay over the snapshot: the streams
share no sequence boundary. Subsequent live lifecycle events commit their
carried status, preserving bursts shorter than the reconcile interval.

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
- **`pane.report_agent` / `pane.release_agent` / `pane.report_agent_session`**:
  any source may claim a pane under a `source` that does not start with
  `herdr:` — `agent` is the identity users see, `state`
  (`idle`/`working`/`blocked`) drives waits, notifications, and rollups,
  and `seq` must increase per source or the report is dropped (a
  timestamp works). The optional `resume_argv` array (0.9.2+; older
  Herdr ignores it) makes Herdr re-launch that command in the pane's cwd
  after a server restart. `resume_argv` is validated strictly — plain
  PATH command first, ≤64 elements, ≤8 KiB, no apostrophes or control
  characters; a bad value refuses the whole report with
  `invalid_resume_argv` — and requires the source to hold the pane
  (`resume_not_accepted`), which a `pane.report_agent` carrying the
  field satisfies in one call. The command is kept only while the same
  `source`+`agent` holds the pane; `pane.release_agent` clears it, and
  Herdr's safety net clears a stale claim once the pane returns to an
  idle shell prompt. Lerdr uses this on the argv `agent_start` path: a
  custom command Herdr never detects gets a `lerdr`-sourced claim once
  the detection deadline runs out, instead of staying a nameless shell
  pane. Profiles Herdr detects natively keep their vendor integration —
  dual claims are just multiple sources reporting on one identity. A
  Herdr without agent reporting refuses the method and the outcome
  degrades to the prior dispatched-unknown result.
- **`pane.agent_status_changed` is the authoritative status stream, not a
  watch nudge**: its payload carries the new `agent_status` (plus `agent`,
  `display_agent`, `title`, `state_labels`), ordered and reliable. The
  relay subscribes it on the topology stream (gated like
  `workspace.reordered` — older builds may reject the name, the
  handshake drops it per refusal) and commits the carried status into
  the committed row — running the transition pipeline so a `working`
  burst too short for a `session.snapshot` sample still produces the
  working→idle `done`+unseen arc. Snapshot commits remain
  status-preserving on the Event path; the `agent_event` UDP datagram
  stays as a wake but now also commits its carried `status`/`pane_id`
  instead of being reduced to a sampling poke.
- **`terminal session control`** is a persistent CLI terminal stream, not
  a socket RPC. The relay's separate bounded driver uses it only for
  native geometry ownership and release; ordinary text/key input still
  uses `pane.send_input`. It does not forward `terminal.input`, scroll or
  mouse commands, inject mouse escape sequences, or force takeover.
- **`plugin.pane.open` placements**: `overlay|popup|split|tab|zoomed`;
  popup supports `width`/`height` — setup pickers become modals.
- **Socket paths**: `~/.config/herdr/herdr.sock` or
  `~/.config/herdr/sessions/<name>/herdr.sock`; env resolution order
  `--session` > `HERDR_SOCKET_PATH` > `HERDR_SESSION` > default.
- **`server.live_handoff`**: upgrades the server without dropping
  sessions; plugin `[[startup]]` hooks re-run on handoff — ours must
  re-assert socket/event/view state.
