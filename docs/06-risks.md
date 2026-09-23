# 06 — Risks and mitigations

## High severity

### Crypto byte-parity drift
The handshake has sharp edges: uncompressed 65-byte P-256 points,
`RawURLEncoding` (not standard b64), the exact `\x00`-joined binding
strings, direction labels in AAD, BE64 sequences, HKDF info strings.
A single byte of drift = silent auth failure or worse, cross-compat bugs.

**Mitigation**: golden vectors generated from the Go implementation before
any porting; both new implementations must pass 100% of vectors including
negative cases (bad proof, wrong sequence, replay). Interop test: real
Kotlin↔Go pairing before any UI work.

### Ack-gate / delta-chain divergence
Pane deltas are a stateful protocol: base fingerprints, `ack_required`,
the ~4 s pending timeout, resync semantics. A subtly-wrong client wedges
the watch or thrashes resyncs (we already fixed this once — server-side
pending timeout, v0.26.1).

**Mitigation**: port `applyPaneDelta` semantics literally; fixture-test
op sequences; add a client-side watchdog mirroring the server's (watch
stalls → re-watch); keep the crash-banner equivalent (a native
uncaught-exception overlay in debug builds, logcat-friendly in release).

### Scope reality check
~57k LOC Go, ~15.5k LOC frontend, ~70 actions, 7 agent-kind conversation
readers. A "rewrite it all" framing will stall.

**Mitigation**: the phased roadmap — app MVP ships with feed + actions
only; relay ships core-first behind shadow testing; per-package porting
order keeps every landing reviewable.

## Medium severity

### Material 3 Expressive is alpha-only (sharper than "alpha")
Expressive APIs were **removed from stable 1.4.x entirely** — they exist
only in `1.5.0-alphaXX` (alpha28 latest). Components graduate piecemeal:
`ButtonGroup` went stable in alpha22, `Flexible*AppBar` family in
alpha24; some remain `@ExperimentalMaterial3ExpressiveApi`.

**Mitigation**: `compose-bom-alpha` pin is mandatory (no stable path
exists); isolate every expressive component behind `designsystem`
wrappers with stable-signature fallbacks; Roborazzi screenshot baseline
catches API reshuffles at upgrade; track the 1.5 stable milestone and
re-pin the moment it lands.

### WebRTC on Android is heavy
Google's `webrtc` AAR adds ~30–40 MB of native libs. The direct path is a
latency/bandwidth optimization, not a correctness path — the gateway
covers reachability.

**Mitigation**: ship phase 1–2 without it (WS only); evaluate the lighter
builds or deferred delivery via Play Dynamic Delivery; the protocol treats
WebRTC as optional capability already (`hybrid` descriptor is advisory).

### Foreground-service notifications vs OEM battery killers
Xiaomi/OPPO/vivo aggressively kill persistent services; a killed socket =
missed attention notifications — the exact Omnara complaint we're
avoiding.

**Mitigation**: `FOREGROUND_SERVICE_DATA_SYNC` type, `onTaskRemoved`
restart, battery-optimization exemption UX, WorkManager watchdog
(reconnect attempt every 15 min floor), and the documented FCM adapter
interface as the opt-in fallback for hostile OEMs.

### Conversation-reader drift
The JSONL readers track upstream agent formats (Claude Code, Codex…)
which change without notice; porting them to Rust doubles the chase.

**Mitigation**: readers stay behind `get_conversation_history` — the app
never sees raw formats. Rust readers port *from fixtures* per agent kind;
when a format drifts, fix once in the relay, both clients benefit.

## Low severity / watchlist

- **Pane lease thrash** on keyboard open/close — coalesce lease requests
  per frame like the web app does (v0.26.3 rAF pattern → `snapshotFlow`
  debounce in Compose).
- **Unicode width** — grapheme clusters + East Asian wide chars in ANSI
  rows: port the segmentation tests, use `BreakIterator` not `length`.
- **4 MiB send-buffer ceiling** — giant full frames evict the client;
  monitor pane sizes, prefer deltas, document the limit before raising it.
- **Spec drift** — `docs/` and `fixtures/` are the contract; behavior
  changes land only through spec updates + vector regeneration, or the
  spec lies. Mitigation: the shadow-diff harness doubles as a
  determinism/regression alarm on the outbound stream.
- **Keystore loss on backup/restore** — credentials wrapped by Keystore
  keys may not survive device replacement; pairing recovery UX (re-pair
  QR) must be easy.

## Explicitly accepted

- **Android-only client** — no PWA; the Rust relay ships no `web/`
  bundle. iOS/desktop are out of product scope.
- **No wire-format changes in flight** — deferred to the Phase-5
  protocol revision.
