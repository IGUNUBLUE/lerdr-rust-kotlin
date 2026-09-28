# Changelog

All notable changes to this project will be documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Releases are **pre-release / beta** — no compatibility guarantees yet.

## [0.2.2] — 2026-09-28

### Fixed

- **Pairing retry wedged after a rejection** — an `AuthRejected` (or
  `Closed`) session stayed in the session map with its dial loop already
  exited, so every later Connect replayed the stale verdict instantly
  (zero wire traffic) until the app was force-stopped. `connect()` now
  recreates a terminal session only while a pending `RelayInvitation`
  exists — re-pairing writes one, a revoked credential keeps its record
  and must not redial forever — and the session factory reads auth
  records straight from the store instead of a lagging mirror. Verified
  on-device: consumed-token rejection, then a re-armed invite redeems on
  plain retry with no restart.

## [0.2.1] — 2026-09-28

### Fixed

- **QR pairing crashed on release builds** — R8 stripped the ML Kit
  `*Registrar` constructors that `ComponentDiscovery` instantiates
  reflectively, so `BarcodeScanning.getClient()` produced a scanner with
  a null delegate and the first analysed frame NPE'd. Keep rules now
  cover `com.google.mlkit.**` + `com.google.firebase.components.**`.

## [0.2.0] — 2026-09-28

### Added

- **Real push delivery via UnifiedPush** — the app registers with any
  installed UnifiedPush distributor (tested with ntfy), sends the
  endpoint + Web Push keys to each relay via `push_subscribe`, and the
  relay delivers RFC 8291-encrypted pushes signed with its VAPID key.
  Pushes render through the same notification ids as socket events (no
  duplicates), deep-link into the app (`lerdr://settings`,
  `lerdr://agent`), and revive a dead process — delivery is verified to
  survive `force-stop`. Settings shows the real subscription state per
  stage and a "Send test" action.

### Changed

- **Keep-alive service is now the fallback, not the primary** — the
  `dataSync` foreground service only pins while push cannot cover
  dead-process delivery (no distributor, registration incomplete, or no
  relay subscription acked). Once a relay acks the current endpoint the
  pin is released, ending the persistent-foreground battery warning and
  the Android 15+ `dataSync` quota exposure. Pin activation/release is
  debounced (1.5s/3s) and a system-restarted service self-stops when
  push already covers. Settings reports the keep-alive state honestly.
- **Relay: push endpoint validation is structural** — any `https` host
  on port 443/default is accepted (no userinfo, no fragment) instead of
  a five-host allowlist, matching the UnifiedPush model where the user
  picks the distributor.

## [0.0.5] — 2026-09-24

### Added

- **Terminal text selection** — long-press-drag marks a cell range and
  the release menu offers "Copy selection" / "Share selection", so part
  of an agent's output can be copied into another session's input. A
  held press without travel still opens the row menu; a tap dismisses
  the selection. Wide graphemes and multi-codepoint clusters select
  atomically. Dragging raises the platform magnifier above the finger,
  and committed selections grow draggable teardrop handles that re-range
  the endpoints — a tap inside the selection reopens the copy menu.
  While a selection is marked the grid freezes its rendered rows
  (copy-mode semantics), so live pane output can no longer slide the
  text out from under the highlight in a truncated scrollback.

### Changed

- **Terminal is edge-to-edge** — the pane card and the cell grid no
  longer pay double horizontal margins, so the negotiated lease tracks
  the real screen width (~8 more columns on a phone).

## [0.0.4] — 2026-09-24

### Added

- **`pane_links` in the terminal** — the long-press menu now hit-tests
  the cell server-side (`pane_link_resolve`) and gains an "Open link on
  desktop" item when herdr reports link regions. This reaches OSC8
  escape-sequence links whose URL never appears in the served text;
  `pane_link_activate` opens on the pane host's browser and reports the
  target, with a local-open fallback when upstream resolves but cannot
  handle. Negotiated via the `pane_links` capability — older relays keep
  the client-side linkified items only.
- **`pane_search` scrollback count** — the terminal find bar annotates
  the local "n of m" with the server's full-scrollback hit count
  ("· N in scrollback") when `pane_search` is negotiated and hits live
  beyond the rendered buffer. Debounced per query; local matching stays
  the source of truth for highlights and navigation.

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
