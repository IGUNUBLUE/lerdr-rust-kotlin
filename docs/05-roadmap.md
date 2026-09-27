# 05 — Roadmap

Guiding rule: **each phase ends with something usable against production
infrastructure.** The protocol seam lets app and relay progress
independently; the phases below interleave them so validation is always
against a real counterpart.

## Phase 0 — Fixtures and contract harness — complete

**Deliverables**
- The committed `fixtures/` corpus: E2EE handshake transcripts,
  sealed/opened frame pairs, pane-delta sequences, ANSI spans, structured
  questions, and conversation-history pages.
- Rust and Kotlin fixture consumers that exercise those vectors.

**Exit**: `lerdr-core`/`protocol` DTOs decode every fixture;
`:core:e2ee` round-trips every crypto vector. The vectors are frozen; protocol
changes require a deliberate revision in this repository, not regeneration
from a retired implementation.

## Phase 1 — Kotlin core — complete

The app half established the current protocol and UX contract. Early work used
the then-active predecessor during migration; the current regression gate is
the fixture corpus and the Rust/Kotlin integration path.

1. `:core:protocol` — DTOs, action catalog, receipts.
2. `:core:e2ee` — handshake + session (live pairing test against a
   running Rust relay).
3. `:core:transport` — OkHttp WS, backoff/keepalive, `push_config` intake.
4. `:core:store` — agents/workspaces/connections StateFlows with documented
   identity-preserving merge semantics.
5. `:core:terminal` — ANSI parser + delta applier + fingerprint chain +
   ack gate client.
6. `:core:data` — relay registry, Keystore credentials, drafts.

**Delivered scope**
- Pairing (QR + clipboard + link), biometric lock.
- Home mission control + attention rail.
- Agent Feed mode (conversation pages, tool cards, question cards,
  composer with prompts/answers/uploads).
- Foreground-service notifications + channels + deep links.

**Exit**: daily-driver quality for monitoring, approvals, and prompting.

## Phase 2 — Terminal and remaining surfaces — complete

- Terminal mode: virtualized ANSI renderer, special-keys bar, IME/key
  interception, size lease with `adjustResize`, and find-in-buffer.
- Details mode: workspace tree/file/git, tabs, and worktrees.
- Activity journal, speech controls, update flows, device management, and
  push-policy UI.
- `client_shell` remains a capability-gated Herdr integration investigation;
  it is not required for the shipped app.
- WebRTC and gateway paths are out of scope for the Tailscale-only transport.

**Exit**: the Android client covers the supported product surface. The retired
web client is not a deployment target.

## Phase 3 — Rust relay core — complete

- The workspace contains `lerdr-core`, `lerdr-e2ee`, `lerdr-fixture`,
  `lerdr-herdr`, `lerdr-coord`, `lerdr-relay`, and `lerdr-shadow`; together
  they provide `/ws`, `/healthz`, actions, and no web assets.
- `tools/shadow` drives scripted `herdr-e2ee-v2` traffic against fake Herdr
  and compares repeated Rust runs. An identical normalized outbound trace is
  the determinism regression gate.

**Exit**: the Rust relay serves the production Kotlin app and installs as the
`lerdr.events` plugin — `plugin install`/`link`/`build`, actions, panes, the
`event-hook` subcommand, and the `[[startup]]` hook work end-to-end (see doc
09).

## Phase 4 — Rust completes — complete

- Conversation readers, question parsing, slash commands, uploads, speech,
  updates, audit, and localization are part of the relay.
- Gateway, WebRTC, port-mapping, and app-deploy paths are deliberately absent
  from the Tailscale-only product. Their protocol-v3 names remain reserved for
  compatibility; this is a Lerdr scope decision.
- `[[startup]]`, `agent.view.set`, and `[[link_handlers]]` are wired into the
  plugin manifest (doc 09).
- CI builds release binaries and APKs with manifest/version synchronization.

**Exit**: this repository is the implementation and release source. Both
halves share the same specifications and frozen fixtures.

## Phase 5 — Protocol v3 capability revision — complete

**Landed end-to-end both sides** — spec ratified in `docs/13`, relay
through `3b86093` + `7febd29`, app through `dc7ade1` + `8de761c`; all
capabilities negotiated live on `:8377` (`caps=22` advertised).

- Capability negotiation revision — **done**: `client_caps`
  post-handshake + symmetric `caps_update` (§0), live =
  advertised ∩ announced.
- Conversation **subscriptions** (push instead of
  `get_conversation_history` polling) — **done**: per-pane
  `subscribe_conversation`, `conversation_update` reset/append.
- Pre-seal compression for pane frames — **done** as `frame_zstd`:
  negotiated zstd `pane_content` payloads (~10× on realistic frames).
- Upload binary chunks (was base64-in-JSON) — **done** as
  `upload_binary`: raw `[0x03][id:32][seq][bytes]` inside the E2EE
  channel; JSON carrier remains for non-negotiated clients.
- Track-A feature actions — **done**: `focus_*`, `pane_search`,
  `pane_selection_read`, `pane_link_resolve`/`activate`,
  `layout_export`/`apply`.
- Binary inner codec — **dropped** (`docs/13` §2.1): `frame_zstd`
  captured the real compression win; a second inner codec is not worth
  its fixture/conformance surface.

## Order-of-work rationale (historical)

1. The Kotlin client established the Android experience and protocol consumer.
2. The Rust relay then became the production service, anchored by the fixtures.
3. Gateway and WebRTC were removed from the product scope; the Tailscale path
   is the supported transport.

## Historical sizing note

The predecessor was a substantial Go and TypeScript system. That history
explains the phased migration, but does not define present-day scope. The
fixture suite and Rust self-determinism harness keep the current implementation
safe to change.
