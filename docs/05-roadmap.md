# 05 — Roadmap

Guiding rule: **each phase ends with something usable against production
infrastructure.** The protocol seam lets app and relay progress
independently; the phases below interleave them so validation is always
against a real counterpart.

## Phase 0 — Fixtures and contract harness

**Deliverables**
- `fixtures/` generator in the Go repo exporting:
  - E2EE handshake transcripts (hello/proof/keys/finish, both auth kinds)
  - Sealed/opened frame pairs (JSON + binary codecs, both directions)
  - `pane_delta` op sequences + expected applied buffers
  - ANSI line corpus → styled-span expectations (from `terminal.ts` tests)
  - `QuestionInteraction` parse fixtures (from `attention_test.go`)
  - `get_conversation_history` pages for each agent kind
- This repo's CI job that runs vectors in Rust and Kotlin harnesses.

**Exit**: `lerdr-core`/`protocol` DTOs decode every fixture;
`:core:e2ee` round-trips every crypto vector.

## Phase 1 — Kotlin core against the Go relay (the big one)

Build the app half first — it derisks crypto + protocol + UX against a
server that already works.

1. `:core:protocol` — DTOs, action catalog, receipts.
2. `:core:e2ee` — handshake + session (interop test: pair a real device
   against the Go relay — this *is* the acceptance test).
3. `:core:transport` — OkHttp WS, backoff/keepalive, `push_config` intake.
4. `:core:store` — agents/workspaces/connections StateFlows; identity-
   preserving merge (port the v0.26.3 merge semantics).
5. `:core:terminal` — ANSI parser + delta applier + fingerprint chain +
   ack gate client.
6. `:core:data` — relay registry, Keystore credentials, drafts.

**App MVP on top:**
- Pairing (QR + clipboard + link), biometric lock.
- Home mission control + attention rail.
- Agent Feed mode (conversation pages, tool cards, question cards,
  composer with prompts/answers/uploads).
- Notifications: foreground service + channels + deep links.

**Exit**: daily-driver quality for monitoring + approvals + prompting.
Terminal mode can ship a milestone later inside this phase — it's the
largest single component; don't block the feed on it.

## Phase 2 — Terminal parity + remaining surfaces

- Terminal mode: virtualized ANSI renderer, special-keys bar, IME/key
  interception, size lease w/ `adjustResize`, find-in-buffer.
- Details mode: workspace tree/file/git, tabs, worktrees.
- Activity journal + detail; speech controls; update/app-deploy flows;
  device management; push policy UI.
- `client_shell` investigation: evaluate whether the app should consume
  Herdr's client-shell surface projections (`client_shell.surface.set`,
  `command.invoke`) instead of only pane text — richer semantics designed
  for remote UI. Prototype-read only; no commitment.
- WebRTC direct path (evaluate `webrtc` AAR size cost vs benefit — the
  gateway path already works; likely worth it only for bandwidth-heavy
  sessions).

**Exit**: zero features that only exist in the old web app. The Tauri
shell is retired — Android is the only client going forward.

## Phase 3 — Rust relay core (shadow parity)

- `lerdr-core` + `lerdr-e2ee` + `lerdr-herdr` + `lerdr-watch` +
  `lerdr-coord` + `lerdr-store` + `lerdr-push` + minimal `lerdr-relay`
  binary serving `/ws` + `/healthz` + actions (no web assets).
- Run **shadow**: Rust relay on a second port against the same Herdr;
  compare outbound event streams (agents, panes, questions) with the Go
  relay — a diff harness is the parity oracle.
- Keep scope: skip appdeploy/update/speech/appdirs niceties until the core
  is proven; they are leaf packages.

**Exit**: Rust relay serves the production Kotlin app for a week with
no behavioral divergence vs the shadow-diff harness; CPU/RSS ≤ Go
baseline on identical load; **installs as the same `lerdr.events`
plugin** — `plugin install`/`link`/`build`, all actions, panes, the
`event-hook` subcommand, and the `[[startup]]` hook work end-to-end
(see doc 09).

## Phase 4 — Rust completes

- Remaining packages: conversation readers, question parser, slashcmd,
  uploads, speech, update, appdeploy, portmap, audit, localize.
- ~~`lerdr-gateway` binary in Rust; gatewaywire parity.~~ **Removed
  upstream** — the oracle's CHANGELOG: "Tailscale is now the only
  transport"; `lerdr-gateway`, the WebRTC gateway path, portmap/UPnP,
  and the app-deploy stage were deleted from the reference. Not ported;
  wire names stay reserved for compatibility.
- ~~WebRTC server side (`webrtc` crate) for `herdr-dc-v1`.~~ Removed
  upstream with the gateway path (see above).
- `[[startup]]` hook + `agent.view.set` canonical view + `[[link_handlers]]`
  deep links wired into the plugin manifest (doc 09).
- CI matrix: interop tests both directions; release pipeline producing
  static musl binaries + the same tarballs/APK + `herdr-plugin.toml`
  version sync (bump in the release PR, never at build time).

**Exit**: `lerdr` Go binary superseded; tag as the reference
implementation. Repo decision (mono vs split) deferred to this point —
keeping both halves in one repo until then maximizes fixture sharing.

## Phase 5 — Protocol v2 (post-parity improvements, negotiated)

Now that both ends are native code we control:
- Binary inner codec (the E2EE path already supports binary frames —
  `CodecBinary` exists; inner payload JSON→binary is the ~37% win).
- Pre-seal compression for pane frames (zstd — deltas already compress
  well, frames don't).
- Conversation **subscriptions** (push instead of `get_conversation_history`
  polling).
- Upload binary chunks (today base64-in-JSON).
- Capability negotiation revision.

## Order-of-work rationale

1. **App first** because the Kotlin client validates against a known-good
   server and delivers user value immediately (native UX on the existing
   relay).
2. **Relay second** because by then the protocol is proven from the client
   side and the Go server remains as oracle.
3. **Gateway/webrtc last** — they're the least-differentiated bits and the
   riskiest native dependency on Android.

## Effort honesty

~57k LOC Go + ~15.5k LOC TS/Svelte is not a weekend rewrite. Rough
decomposition: protocol+e2ee+transport cores ≈ 3–4k LOC Rust / 4–5k LOC
Kotlin; app MVP ≈ 8–10k LOC Kotlin; terminal renderer ≈ 2k LOC; relay
feature parity ≈ 12–15k LOC Rust for the non-core packages. The fixtures
and shadow-diff harnesses are what make this safe rather than heroic.
