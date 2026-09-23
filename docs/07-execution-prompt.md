# 07 — Execution prompt

The master prompt to drive the build. Designed for an orchestrator
agent session (Devin/Herdr) that can run parallel stations. Each phase is
a loop with an explicit exit gate; never advance a phase on red.

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

PHASE LOOP — for the current phase, repeat until its exit gate is green:
  a) Pick the next item from the phase's work list.
  b) Implement in the assigned station/module only.
  c) Verify: unit tests + interop/vector tests for that item.
  d) Commit (branch per item) → PR → merge when CI green.
  e) Report: what landed, what failed, what's next.
  f) If blocked twice on the same item: stop, document the blocker in
     docs/blockers.md, escalate to me with options.

Start at PHASE 0. Print the phase exit checklist when you believe it is
met and wait for my confirmation before starting the next phase.
```

## Phase 0 — fixtures (parallel, ~1 session)

```
fixtures/ is committed and frozen — this phase is DONE. Layout kept
for reference:

  fixtures/e2ee/       — handshake transcripts (credential+invitation),
                         derived keys, sealed/opened frame pairs, both
                         codecs, negative cases (bad proof, replayed seq)
  fixtures/panedelta/  — op sequences + expected applied buffers
  fixtures/ansi/       — ANSI lines → expected span runs
  fixtures/question/   — pane content → QuestionInteraction
  fixtures/conversation/ — JSONL samples → Entry pages, per agent kind
  fixtures/envelope/   — Inbound/outbound JSON samples for every action
                         in the catalog (field shapes, optionality)

Gate: every fixture reproducible by a script in fixtures/gen/; vector
tests green on both sides.
```

## Phase 1 — Kotlin core + app MVP (2 parallel stations)

```
Station A (:core) — owns app/core/**:
  protocol DTOs, e2ee session, transport (OkHttp WS + backoff),
  terminal engine (ANSI parser + delta applier + ack gate),
  store (StateFlows), data (Keystore creds, DataStore, drafts).
  Verify: fixture tests + REAL pairing against the Rust relay
  (LERDR_RUST_INTEROP=1 spawns one; or a live device session).

Station B (:app/:feature) — owns app/app/** + app/feature/**:
  M3E theme, nav graph, Home (needs-you rail + agent list + relays
  strip), pairing screens, notification channels + foreground service,
  Agent Feed (conversation pages, tool cards, question cards, composer),
  biometric lock.
  Verify: unit + Paparazzi screenshot tests; manual run against a fake
  :core:store until Station A lands the real one (define the interface
  boundary FIRST — store interfaces in :core:api if needed).

Shared-seam rule: Station A publishes interfaces/fakes first; Station B
codes against them. Any seam change = orchestrator merges it.

Exit gate: on a real phone — pair via QR, see agents, get attention
notification, answer an approval, send a prompt, watch the feed update.
All without a WebView.
```

## Phase 2 — terminal parity + remaining features (2 stations)

```
Station A (:core:terminal hardening + terminal UI):
  virtualized LazyColumn renderer, special-keys bar, IME/KeyEvent input,
  pane size lease with adjustResize, find-in-buffer, scroll-to-live.
  Verify: ANSI fixtures render pixel-comparable (golden screenshots),
  delta sequence fixtures apply exactly.

Station B (remaining features):
  workspaces/worktrees/files/git views, activity journal, speech,
  updates, device management, push policy UI, uploads.

Exit gate: the ~70-action catalog is reachable in UI; no feature exists
only in the old web app.
```

## Phase 3 — Rust relay core (2 stations + shadow harness)

```
Station A (transport core): lerdr-core, lerdr-e2ee, session actor,
  send buffer semantics, axum /ws endpoint. Verify: fixture vectors +
  the Kotlin app pairs and works against it.

Station B (herdr + watch): lerdr-herdr (socket API + CLI fallback +
  events), lerdr-watch (fingerprints, deltas, ack gate), lerdr-coord,
  lerdr-store, lerdr-push. Verify: shadow harness — scripted scenarios
  against the fake Herdr produce a deterministic normalized outbound
  stream (`tools/shadow` self-mode is the regression gate).

Exit gate: shadow traces deterministic; Kotlin app + scripted client
both pass against Rust.
```

## Phase 4+ — completion, gateway, protocol v2

Per docs/05-roadmap.md. Conversation readers port from fixtures last in
phase 4 (they drift; do them when everything else is stable).
```

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
