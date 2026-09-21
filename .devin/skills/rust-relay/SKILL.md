---
name: rust-relay
description: "Trigger: rust relay, crate, tokio, axum, actor, session actor, send buffer, websocket server. Conventions for the Rust relay workspace: actor topology, bounded channels, error taxonomy."
license: Apache-2.0
metadata:
  author: "IGUNUBLUE"
  version: "1.0"
---

# Rust Relay Conventions

## Activation Contract

Use when creating or editing crates under `relay/crates/`.

## Hard Rules

- **Actors own state; channels own boundaries.** No `Mutex<HashMap>` in hot paths. Shared projections go through `tokio::sync::watch` (latest-value) or `broadcast`; ordered per-subscriber work goes through bounded `mpsc`.
- **All queues bounded.** A lagging session actor is evicted — this mirrors the Go sendbuffer contract (64 msgs / 4 MiB). Never `unbounded_channel`.
- Error taxonomy per layer: `thiserror` enums at crate edges (`HerdrError`, `E2eeError`, `CoordError`); `anyhow` only in the binary wiring. Dispatch boundary (`NotStarted`/`DispatchedUnknown`/`Refused`) must survive to `ActionReceipt.phase`.
- Structured concurrency: spawned tasks tracked in `JoinSet`/`CancellationToken`; watch tasks die with their last subscriber; graceful shutdown via token cascade, not task leaks.
- `tracing` instrumentation: `#[instrument]` on actor loops and request handlers; `tracing-journald` layer behind a feature flag (journald acceptance test exists).
- **State-machine + driver split**: actor cores are synchronous state machines; a thin tokio driver owns I/O — cores unit-test without a runtime.
- **Virtual time in tests**: `#[tokio::test(start_paused = true)]` + `time::{pause,advance}` for backoff/lease-TTL/keepalive/eviction (`test-util` feature).
- **Test stack**: `proptest` codec invariants (`Apply(Build(a,b))==b`, `open(seal(x))==x`) layered on golden vectors; `insta` for response/event snapshots; `rstest` fixture tables.
- No new dependency without a note in the PR justifying it vs. std/existing crates; prefer crates ≥7 days published.

## Decision Gates

| Need | Pattern |
|---|---|
| Fan-out same event to many | `broadcast::Sender`, Lagged → evict/resync |
| Latest state to many | `watch::Sender` |
| Ordered work per entity | dedicated task + `mpsc` inbox |
| Request/response across tasks | `mpsc` + `oneshot` reply |

## Output Contract

Every crate exposes a narrow `pub` surface; internal types stay private. Tests: unit per module + `tests/vectors` conformance when wire-facing.

## References

- `docs/02-architecture.md`, `docs/08-herdr-boundary.md` — topology and boundaries.
- `docs/11-stack-practices.md` — Rust test/pattern stack rationale.
- Installed skill: `rust-async-patterns` (generic Tokio reference).
