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
