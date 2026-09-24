# Changelog

All notable changes to this project will be documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Releases are **pre-release / beta** — no compatibility guarantees yet.

## [0.0.3] — 2026-09-24

### Added

- **Terminal send + watch cadence** — the terminal input bar's Send
  action now rides `send_input` (text + Enter as one action) instead of
  literal `send_text`; the special-keys bar still exposes literal input.
  `watch_pane` requests `interval_ms=100` (the lowest whitelisted
  cadence) now that `pane_realtime_delta` is advertised and watches arm
  automatically.
- **report_metadata badges** — `agents[]` rows decode herdr
  `pane.report_metadata` projections (`tokens`, `state_labels`) and
  `workspaces[]` rows decode `tokens`. Home renders a watching eye on
  panes carrying `lerdr_watching`, a device-count chip on workspace
  groups carrying `lerdr_devices`, and state-label chips on agent rows.
  Snapshot semantics: absent keys clear (herdr TTL expiry), deltas keep.
- **Capability contract completed** (`docs/03` §4.1 is canonical) — the
  relay now advertises `pane_realtime_delta` (gated on `pane.read`),
  `tab_reorder`, `workspace_reorder_block`, `typed_push`,
  `push_policy`, and `device_management`, so app-side feature gates arm
  on connect.
- **Speech capabilities wired** — `speech_synthesis` /
  `speech_voice_management` advertise from live local speech facts
  (flite/espeak/say system fallbacks or the managed Piper engine), flip
  mid-session via `caps_update`, and populate
  `push_config.speech_languages`.

### Changed

- **inner_codec_binary dropped** (`docs/13` §2.1) — the deferred CBOR
  inner-codec path is removed outright; `frame_zstd` captured the real
  win. `preferred_inner_codec` is gone from `client_caps` (relays
  ignore it as an unknown field either way).

### Removed

- Dead capability constants (`herdr-hybrid-v2` — hybrid transport is
  out of scope per docs/12; a duplicated `agent_response_copy`).

## [0.0.2] — 2026-09-24

### Added

- **Phase-5 Track A** (`docs/13-phase5-wire-spec.md`) — capability
  negotiation + pane-content actions on the frozen v3 envelope:
  - App announces `client_caps` as the first post-handshake frame and
    applies the relay's symmetric `caps_update`; the negotiated live set
    is `server-advertised ∩ app-announced`.
  - New actions: `focus_pane`/`focus_tab`/`focus_workspace`/`focus_agent`,
    `pane_search` (match-position metadata), `pane_selection_read`,
    `pane_link_resolve` (cell regions) / `pane_link_activate`
    (`handled` + `url`), `layout_export`/`layout_apply`.
  - `TargetRef` gains `workspace_id`/`tab_id`; `Inbound` gains the
    structured Phase-5 raw fields (`query`, `cursor` objects,
    `anchor`, `previous`, `row`/`col`, `root`, `tab_label`, `focus`).
- **Phase-5 Track B** (`docs/13-phase5-wire-spec.md` §2) — negotiated
  transport upgrades, all gated on the live capability set:
  - `frame_zstd` — `pane_content` frames may carry
    `encoding:"zstd"` + base64 zstd `payload`; the envelope stays
    plaintext and the inflated `{content}` restores before the
    terminal surface consumes it. Malformed payloads drop safely.
  - `convo_sub` — `subscribe_conversation`/`unsubscribe_conversation`
    per pane; `conversation_update` pushes a `reset` snapshot then
    append-only tails, deduplicated by entry id and dropped when the
    pane `generation` is stale. The Feed subscribes while the screen
    is open; history polling remains the fallback path.
  - `upload_binary` — chunks ride the `0x03` carrier
    `[0x03][upload_id:32][chunk_seq:BE64][bytes]` inside the sealed
    channel when `upload_begin_result.chunk_encoding == "binary"`.
    Acks stay JSON (`upload_chunk_result`, empty `request_id`,
    correlated by send order); JSON and binary carriers share one
    sequence domain per upload.

## [0.0.1] — 2026-09-23

First public release. Working end-to-end, still in development.

### Added

- **Rust relay** — axum WebSocket server, per-pane watchers, bounded send
  buffers, Herdr Unix-socket client, health endpoints (`/health`,
  `/healthz`, `/readyz`), update worker, `speech-voices` binary surface,
  release packaging (`lerdr-relay_<v>_<os>_<arch>.tar.gz`).
- **Android app** (Kotlin / Jetpack Compose, Material 3 Expressive):
  - Pairing via QR / `lerdr://pair` deep link, biometric lock.
  - Mission-control home: agents grouped by workspace, working /
    attention / idle states, needs-you rail with one-tap approvals.
  - Session: Feed (conversation, question and approval cards, composer,
    history), Terminal (full ANSI pane, key bar, find, leases), Files
    (workspace tree, preview, git status/diff).
  - Notifications with deep links, Settings + relay detail, computers,
    activity journal, worktrees, session rename/reorder.
- **Protocol** — `protocol v3` / `herdr-e2ee-v2`: P-256 handshake, HKDF,
  AES-GCM sealed frames; anchored by frozen golden vectors (`fixtures/`).
- **Tailscale transport** — `tailscale serve` integration scripts in
  `plugin/scripts/`; relay binds loopback, tailscaled owns the tailnet
  listener.
- **Determinism harness** — `tools/shadow/` runs two fresh relays against
  one fake Herdr and diffs normalized outbound streams.
- **Herdr plugin packaging** — manifest + install/verify scripts under
  `plugin/`.

### Notes

- The Android APK ships **unsigned** until keystore secrets are
  configured — sideload at your own discretion.
- This is an AI-generated codebase (built with Cognition's SWE-2);
  see `README.md` and `CONTRIBUTING.md` for the disclosure policy.
