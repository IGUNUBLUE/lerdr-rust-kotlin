# 08 — The Herdr boundary (deep dive) + improved service topology

Source: `internal/herdr/{socket_api,client,events,send_input,capabilities}.go`.
This is the only contract the relay has with the host — getting its Rust
shape right is what makes the rest of the service cheap.

## The wire contract

**Transport**: Unix socket (path from config/env), newline-delimited JSON.
**One request per connection** — Herdr closes the socket after each
response. Connection pooling is impossible by design; the Go client
learned this the hard way (cached conn → every read hit a dead write →
retry). Reads dial fresh per attempt (v0.26.3 fix).

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

## The improved service topology (Rust)

The Go relay grew organically: mutexes around shared maps, a coordinator
crate bolted on, watch loops holding locks. In Rust we make the actor
model explicit — **channels own the boundaries, tasks own the state**:

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
   readers hold `watch::Receiver`s. Session outbound is `mpsc` (bounded —
   lag → evict, same contract as Go sendbuffer).
2. **Singleflight on Herdr reads.** N watchers of the same pane → one
   `pane.read` in flight; results fan out. Cuts Herdr load ~N:1 on the
   hot path the phone-heavy UX creates.
3. **Semaphore around dials.** Each request = a socket; cap concurrent
   dials (e.g. 32) so a thundering herd of watch probes can't fd-storm
   Herdr.
4. **Dispatch boundary travels end-to-end.** `HerdrError` maps onto
   `ActionReceipt.phase`: `NotStarted` → `failed_before_dispatch`,
   `DispatchedUnknown` → `dispatched_unknown`, `Refused` → `confirmed`
   error path. The mobile UI gets honest receipts.
5. **Event-driven, not polled.** Inventory is a projection of the event
   stream + bootstrap, not periodic `agent.list` polling (the Go relay
   still polls in places — the Rust one shouldn't).
6. **Watch tasks die with their subscribers.** `tokio::select!` on
   unsubscribe/shutdown; no orphan reads.

## Kotlin app — revised module layout (nowinandroid-aligned)

Current official guidance (nowinandroid, 2026): 3 layers
(data/domain/UI), repositories expose **Flows** (never snapshots), UDF
with ViewModel+StateFlow, **single feature modules** (the api/impl split
was dropped for Navigation 3), test doubles over mocks, Baseline
Profiles for cold start.

```
app/
├── core/
│   ├── model/          # shared DTOs (Agent, Workspace, Question…)
│   ├── data/           # repositories — expose Flows, merge WS+local
│   ├── network/        # WS transport, E2EE, reconnect (was transport)
│   ├── crypto/         # e2ee handshake/session (Keystore-wrapped keys)
│   ├── terminal/       # ANSI parser, delta applier, frame store
│   ├── conversation/   # paging source for Entry feeds
│   ├── designsystem/   # M3E theme + isolated expressive wrappers
│   ├── ui/             # shared compose components (agent row, tool card)
│   ├── datastore/      # DataStore prefs, Keystore credentials, drafts
│   ├── notifications/  # channels, push-open deep links
│   ├── service/        # foreground connection service + lifecycle
│   └── testing/        # fakes for every repository + fixture loaders
├── feature/
│   ├── agents/         # home mission control
│   ├── session/        # feed + terminal + details modes
│   ├── activity/       # journal
│   ├── workspaces/     # tree/files/git
│   ├── pairing/        # QR, devices, invitations
│   └── settings/       # relays, push, speech, app
├── navigation/         # Nav3 entry providers + top-level destinations
├── app/                # Application, MainActivity, nav host, DI graph
├── app-benchmarks/     # Macrobenchmark + Baseline Profile generator
└── androidTest/        # device tests
```

### App best-practice rules (enforce via skills + lint)

- **Repositories expose `Flow`, never suspend-get.** UI collects with
  `collectAsStateWithLifecycle`. Offline-first: DataStore/DB is the
  source of truth; WS deltas reconcile into it.
- **`@Immutable`/`@Stable` on every model** crossing into compose;
  `key()` in all lazy lists; state reads deferred into layout/draw
  phases where possible.
- **No mocking libs in tests** — fakes in `:core:testing` (nowinandroid
  convention). Fixture-driven unit tests for protocol/terminal/crypto.
- **Baseline Profile + Startup Profile** generated from a paired-session
  journey — cold start is the first impression of "native".
- **Foreground service** type `dataSync`, visible persistent
  notification, `onTaskRemoved` → restart intent, battery-exemption UX
  behind a settings flag.
- **One ViewModel per screen**, scoped to Nav3 entries; no god-store.
  The WS session is in `:core:service` + `:core:data` — survives config
  changes and nav.

## Repo-level skills to ship

Created in `.devin/skills/` (see commit): `herdr-api` (boundary rules),
`protocol-parity` (fixture/vector workflow), `rust-relay` (actor
topology + error taxonomy), `android-app` (module + Compose rules).

## Authoritative findings from the upstream repo (herdrdev/herdr)

Read `docs/next/website/src/content/docs/{socket-api,plugins}.mdx` +
`api/herdr-api.schema.json` (131 methods). Corrections/upgrades over the
reverse-engineered notes above:

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
