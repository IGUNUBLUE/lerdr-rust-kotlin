# 07 — Historical execution record

This captures the migration-era orchestration prompt. All phases described
below are complete; it is retained for provenance, not as a live build plan.
For present-day work, use `AGENTS.md`, the numbered specifications, current
source, and frozen `fixtures/`.

---

## Master prompt

```
You are working on Lerdr: Rust relay + Kotlin/Compose app. The spec
lives in this repo — read these first, they are the authority, do not
re-derive them:

  ~/Projects/lerdr-rust-kotlin/docs/00-inventory.md      (what exists)
  ~/Projects/lerdr-rust-kotlin/docs/02-architecture.md   (target layout)
  ~/Projects/lerdr-rust-kotlin/docs/03-protocol.md       (the contract)
  ~/Projects/lerdr-rust-kotlin/docs/04-app-design.md     (UX spec)
  ~/Projects/lerdr-rust-kotlin/docs/05-roadmap.md        (phases/gates)

The project is self-contained: docs/ + fixtures/ define correct
behavior. There is no external implementation to consult or modify.

GLOBAL RULES — non-negotiable:
1. The wire protocol is frozen: protocol v3 over herdr-e2ee-v2. Any
   deviation = bug. Golden vectors decide correctness, not vibes.
2. Every commit must build and pass its tests. No dead-code commits.
3. English only: code, docs, commit messages.
4. Parallel stations get DISJOINT file ownership. Shared/generated files
   belong to the orchestrator.
5. fixtures/ vectors are frozen — they change only inside a deliberate
   protocol revision.
6. One PR per coherent unit; CI green before merge; no direct pushes to
   main.

HISTORICAL PHASE LOOP — this was the migration workflow:
  a) Pick the next item from the phase's work list.
  b) Implement in the assigned station/module only.
  c) Verify: unit tests + interop/vector tests for that item.
  d) Commit (branch per item) → PR → merge when CI green.
  e) Report: what landed, what failed, what's next.

The completed roadmap is recorded in `docs/05-roadmap.md`; do not restart
these phases or look outside this repository for implementation guidance.
```

## Phase 0 — fixtures — complete

```
fixtures/ is committed and frozen. The corpus contains:

  fixtures/crypto/     — handshake transcripts, derived keys, sealed/opened
                         frame pairs, codecs, and negative cases
  fixtures/pane/       — delta sequences + expected applied buffers
  fixtures/ansi/       — ANSI lines → expected span runs
  fixtures/questions/  — pane content → QuestionInteraction
  fixtures/conversation/ — JSONL samples → Entry pages per agent kind
  fixtures/protocol/   — inbound/outbound JSON samples for the catalog

The Rust and Kotlin fixture consumers are the executable contract. Vectors are
revised only with a deliberate in-repository protocol change.
```

## Phase 1 — Kotlin core + app MVP (historical stations)

```
Station A (:core) — protocol DTOs, E2EE session, OkHttp transport, terminal
engine, store, and data modules. Verification used fixture tests plus pairing
against the Rust relay.

Station B (:app) — M3E theme, navigation, home/pairing surfaces, notification
channels + foreground service, feed, and biometric lock. Verification used
unit tests, Roborazzi screenshot tests, and a fake `:core:store` until the
real seam landed.

The historical shared-seam rule was to publish interfaces/fakes before a
consumer feature. The completed app supports QR pairing, inventory, attention
notifications, approvals, prompts, and live feed updates without a WebView.
```


## Phase 2 — terminal and remaining features — complete

```
The delivered Android client includes the documented terminal, workspace,
activity, speech, update, device-management, and upload surfaces.

Historical exit gate: every supported catalog action had an intentional UI
reach or capability-gated omission. The retired web client is not a feature
authority.
```

## Phase 3 — Rust relay core — complete

```
The Rust relay, Herdr client, pane-watch path, coordinator, store, and push
subsystems are implemented. `tools/shadow` runs scripted traffic against fake
Herdr and compares repeated Rust runs; self-mode determinism is the regression
gate.

Historical exit gate: the Kotlin app and scripted protocol client pass against
the Rust relay.
```

## Phase 4+ — completion and protocol revision — complete

The completed phase record is in `docs/05-roadmap.md`. Conversation readers
are maintained from their in-repository specifications and fixtures.

## Orchestration notes for whoever runs the prompt

- **Verify `HERDR_ENV=1`** if orchestrating via Herdr panes; otherwise use
  background subagents. Four stations max — beyond that, coordination
  cost exceeds the parallelism win.
- **Interfaces first**: when two stations need a shared type (e.g.
  `:core` models vs `:feature` consumers), the orchestrator writes or
  approves the interface file before either station codes against it.
- **Spec-first mindset**: any ambiguity in "what should this do" →
  check docs/ + fixtures/; if the spec is silent, write the gap into
  docs/10-spec-gaps.md, don't guess.
- **CI**: set up the repo's check workflow in phase 0 (Gradle check +
  cargo test + fixture conformance) so every later PR is gated.
