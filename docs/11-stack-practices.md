# 11 — Stack practices (perfectionist pass)

Consolidated best-practice findings per stack, verified against current
docs/releases. Each section lists what the plan *adopts* — not generic
advice.

## Herdr API — usage upgrades beyond docs 08/09

From `api/herdr-api.schema.json` (protocol 22, 131 methods):

| Method | Exact shape | What it unlocks |
|---|---|---|
| `session.snapshot` | → `{version, protocol, workspaces[], tabs[], panes[], layouts[], agents[], focused_*}` | One call = full topology reconcile after `events_lost`; the relay's bootstrap base |
| `pane.wait_for_output` | `{pane_id, source, match{substring\|regex}, strip_ansi, lines?, timeout_ms?}` | Server-side output matching — question/boot detection without read polling |
| `events.wait` | `{match_event, timeout_ms?}` | One-shot waits ("block until workspace.created") |
| `command.invoke` | `{command_id, pane_id?, tab_id?, workspace_id?, selection?}` | Endpoint-issued command ids validated against pane content revision — semantic actions Herdr designed for remote UIs |
| `layout.apply` | `{root: LayoutNode, workspace_id?, tab_id?, tab_label?, focus}` | Whole layout trees from the phone — workspace templates ("3-pane agent setup" as one tap) |
| `layout.export` | → LayoutNode tree | Save the current arrangement as a reusable template |
| `agent.explain` | `{target}` → detection snapshot + matched rule + evidence | Diagnostics surface: "why does Herdr think this agent is blocked" |
| `notification.show` | `{title, body?, position?, sound}` | Desktop toast when the phone acts — closes the "did it work" loop |
| `pane.input.set` | `{pane_id, right_click}` | Right-click/context-menu target — not text input (name is misleading) |
| `integration.{install,list,uninstall}` | — | Manage Herdr agent integrations from the phone |
| `plugin.action.invoke` / `plugin.pane.open` / `plugin.log.list` | — | The relay can drive **other** Herdr plugins through the socket — the phone gains every installed plugin's actions for free |
| `server.agent_manifests` / `server.reload_agent_manifests` | — | Agent-detection rule status/reload — powers a diagnostics screen |
| `server.reload_config` | — | Apply config edits pushed from the phone |
| `client.window_title.set/clear` | — | Surface phone presence in Herdr's chrome |

**Design consequence**: `lerdr-herdr` should expose these as typed methods
with SchemaRegistry gating — a `HerdrCapabilities` struct consumed by
feature flags, not per-call probing.

## Herdr 0.9.1 capability audit (post-implementation, vs `api schema --json`)

129 schema methods; the relay exercises 66. The remaining 63 decompose as:

**Output revisions (upstream Discussion #1277, landed in 0.9.x)** —
`pane.read`/`PaneInfo` carry `content_revision` (u64, seqlock-style: even
when stable, odd mid-write; ops taking `content_revision` fail with
`stale_content` on mismatch); `pane_output_changed{pane_id,workspace_id,
revision}` is a subscription event; `events.wait`/`pane.wait_for_output`
accept `min_revision`. `lerdr-herdr` already models all of it
(`types.rs:616-724`) but `lerdr-coord` does not consume it — the watch
path is still tick-polling with a `pane.*` invalidation fast path, and
the mid-read fence counts the coordinator's own `content_rev` rather
than the upstream revision. Both are **internal upgrades, no wire
change**: (a) wake watched panes on `pane_output_changed` instead of /
ahead of the poll tick; (b) pass the upstream `content_revision` through
`pane.read` and treat `stale_content` as the fence-trip signal. Pane
respawn resets the upstream counter — our generation fence already owns
that axis; the two compose.

**Remote navigation/focus** — `agent.focus`, `pane.focus`,
`pane.focus_direction`, `tab.focus`, `workspace.focus`, `pane.zoom`,
`pane.neighbor`, `pane.edges`: attention deep-link (phone tap → desktop
jumps to the pane). Needs new wire actions → Phase-5/app coordination.

**Pane/layout lifecycle** — `pane.{split,move,swap,resize,rename,close,
get,current}`, `tab.{close,get}`, `layout.export`, `layout.
set_split_ratio`, `pane.scroll`: remote layout management. Wire actions
needed for app use.

**Revision-validated content ops** — `pane.copy_search` (server-side
search → `{matches,total}`), `pane.copy_motion`, `pane.selection.read`,
`pane.edit_scrollback`, `pane.link.{resolve,activate}`: terminal
search/selection/link-opening from the phone. `link.resolve`+
`link.activate` are the programmatic complement to our `link_handlers`
manifest regex.

**Reporting/metadata** — `pane.report_metadata` (title, `display_agent`,
`state_labels`, named `tokens`, `ttl_ms`+`seq` ordering),
`workspace.report_metadata`, `pane.report_agent`,
`pane.report_agent_session`, `pane.{release_agent,clear_agent_authority}`:
these are where the projection's `tokens`/`state_labels` come from —
**correction to spec-gaps: they are herdr-reported metadata, not Go-only
inventions**; `PaneInfo` already deserializes them. Outbound use: the
relay could annotate panes ("phone watching") or report semantic state
for agents herdr doesn't detect.

**Plugin driving** — `plugin.pane.{open,focus,close}`,
`plugin.{enable,disable,link,unlink,list,log.list,action.list}`:
open our own setup/status panes via socket instead of the CLI
(`open-plugin-pane.sh` goes through `herdr` CLI today — socket path
removes that dep); drive *other* plugins' actions from the phone.
`HERDR_PLUGIN_CONTEXT_JSON` carries selected text, clicked URL, and
link-handler fields — our link scripts already consume it.

**Server/admin** — `server.reload_config`, `server.agent_manifests`,
`server.reload_agent_manifests` (diagnostics screen: which detection
rules are active), `integration.{install,uninstall}` (`list` already
used), `server.live_handoff`, `server.stop` (dangerous — probably never
expose), `client.window_title.{set,clear}` + `client_shell.surface.set`
(desktop chrome: "phone connected" indicator — shell-surface is for
herdr's own thin clients, not us).

**Not worth it** — `pane.graphics.{set,info,clear}` (kitty/sixel — our
read path carries text, not images), `popup.close`,
`product_announcement.dismiss`, `release_notes.dismiss`,
`pane.input.set` (right-click mode; misleading name), `agent.read`/
`agent.send_keys` (agent-scoped duplicates of the pane ops we use).

**Phase-5 wire candidates — app-side priority ranking (2026-09):**
1. `focus` family (`agent.focus`/`pane.focus`/`workspace.focus`) — tap a
   notification → desktop jumps to the pane. The app already deep-links
   into sessions; the relay→herdr leg is what's missing.
2. `pane.copy_search` / `pane.selection.read` — terminal find is
   currently client-side over the served buffer; server-side search is
   revision-validated and covers full scrollback.
3. `pane.link.resolve` / `pane.link.activate` — programmatic link
   handling complementing the manifest `link_handlers`.
4. `layout.export` / `layout.apply` — workspace templates from the
   phone; valuable, lower priority.

**Remote/federation note** — herdr's own remote path is SSH thin-client
+ `herdr machine` endpoint federation with `SCM_RIGHTS` live handoff.
Our Tailscale relay is a parallel phone path, not a thin client — no
conflict, but `server.live_handoff` explains why `agent.view.set` must
be re-asserted after takeover (handoff moves PTY fds to a new server).

## Material 3 Expressive — verified state (Sept 2026)

- Stable `material3` = **1.4.0 does NOT ship Expressive** — APIs removed
  from the stable line; they live only in `1.5.0-alphaXX` (alpha28).
- Dependency pin: `compose-bom-alpha` **or** explicit
  `material3 = 1.5.0-alphaXX` — there is no stable alternative.
- Graduation is per-component: `ButtonGroup` stable in alpha22;
  `Flexible*AppBar`/`FlexibleBottomAppBar` stable alpha24. Track which of
  our components are still `@ExperimentalMaterial3ExpressiveApi` — keep
  that list in `designsystem/EXPRESSIVE.md`.
- Compose stable = 1.9 (BOM 2025.08): 2D scroll APIs, list perf fixes.

## Kotlin / Compose performance rules (encode in android-app skill)

- Strong skipping is **default since Kotlin 2.0.20** — keep it; mark
  models `@Immutable`/`@Stable`; `kotlinx.collections.immutable` for all
  list/set params crossing composable boundaries.
- State discipline: defer reads (`Modifier.offset {}`), `derivedStateOf`
  for derived rapid state, stable `key{}` in lazy layouts, never
  backwards-writes.
- Baseline Profiles **+ Startup Profiles** (dex layout) — macrobenchmark
  module generates both; ~30% startup win is the documented figure.
- Release: R8 full mode + resource shrinking; Compose compiler
  stability/metrics reports generated in CI (diff review on PRs).
- Terminal surface specifically: per-line `LazyColumn` is wrong for
  60fps deltas — use a custom `Layout`/canvas draw over a ring-buffer
  model with `drawWithCache`; invalidate by fingerprint ranges.

## Kotlin testing strategy (aligned to nowinandroid)

- **Fakes, never mocks** — `Test*Repository` implementations with
  test-only hooks; Hilt test doubles or constructor injection.
- ViewModels: JVM unit tests targeting ~100% coverage; `runTest` +
  `UnconfinedTestDispatcher`; **Turbine** for multi-emission Flow
  assertions.
- Screenshot tests: **Roborazzi on Robolectric RNG**
  (`@GraphicsMode(NATIVE)`) — not Paparazzi (incompatible with
  Robolectric/Hilt). `captureMultiTheme` for theme coverage.
- Real `DataStore` with temp folder in tests (don't mock persistence).
- Compose UI tests live in `test/` under Robolectric — no shadows or
  device-only APIs, so they also run as instrumented tests.
- Protocol conformance: fixture-driven JVM tests replaying
  `fixtures/**` vectors (golden-vector harness, not hand-written cases).

## Rust practices (encode in rust-relay skill)

- **State-machine + driver split**: actor cores are plain synchronous
  state machines; a thin tokio driver owns I/O. Cores unit-test without a
  runtime — the Polar Signals DST pattern applied pragmatically.
- Virtual time: `#[tokio::test(start_paused = true)]` +
  `tokio::time::{pause,advance}` for backoff, lease TTL, keepalive,
  eviction tests (requires `test-util` feature).
- `proptest` for codec invariants: `Apply(Build(a,b)) == b`,
  `open(seal(x)) == x`, delta chains terminate — property tests on top of
  golden vectors, not instead of them.
- `insta` snapshots for response/event shapes; `rstest` for parametrized
  fixture tables.
- Error taxonomy: `thiserror` in library crates (typed `DispatchError`),
  `anyhow` only at the binary top level.
- Cancellation: `CancellationToken` + `TaskTracker` for graceful
  shutdown; audit every `select!` arm for cancellation safety.
- Observability: `tracing` spans per actor; `tokio-console` in dev
  builds.
- Candidate (phase 3+): `madsim` deterministic simulation for the actor
  system — heavy commitment, evaluate after core parity.

## Cross-cutting decisions confirmed

| Topic | Decision | Note |
|---|---|---|
| DI | Hilt | matches nowinandroid test-double story |
| Serialization | `kotlinx.serialization` | DTO parity with Rust serde |
| Screenshot lib | **Roborazzi** (not Paparazzi) | Hilt/Robolectric compat |
| Rust test stack | `tokio test-util` + `proptest` + `insta` + `rstest` | |
| Time in tests | virtual on both stacks | `start_paused` / `runTest` |

## Version policy (user directive)

- **Latest stable always** — no alpha/beta/RC pins. Refresh catalogs
  (`libs.versions.toml`, `relay/Cargo.toml` workspace deps) at phase
  boundaries, not per-PR.
- Single documented exception: `material3` 1.5.0-alphaXX (M3 Expressive
  has no stable line — tracked in docs/06-risks.md).
- New deps must still be ≥7 days published (supply-chain floor) — a
  version that landed yesterday is "not latest" for our purposes.
- Snapshot of pins at Phase-0 scaffolding: kotlin 2.4.20, coroutines
  1.11.0, serialization 1.11.0, turbine 1.2.1, truth 1.4.5, AGP 9.4.1
  (app module later — JDK floor TBD), tokio 1.53, serde 1.0, p256 0.14,
  aes-gcm 0.11, hkdf 0.13, sha2 0.11, hmac 0.13, base64 0.23.
