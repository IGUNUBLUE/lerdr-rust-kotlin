# 06 — Risks and mitigations

## High severity

### Crypto byte-contract drift
The handshake has sharp edges: uncompressed 65-byte P-256 points,
`RawURLEncoding` (not standard b64), the exact `\x00`-joined binding
strings, direction labels in AAD, BE64 sequences, HKDF info strings.
A single byte of drift = silent auth failure or worse, cross-compat bugs.

**Mitigation**: the committed golden vectors define the expected bytes. Rust
and Kotlin must pass every applicable vector, including negative cases (bad
proof, wrong sequence, replay), plus Rust↔Kotlin interop.

### Ack-gate / delta-chain divergence
Pane deltas are a stateful protocol: base fingerprints, `ack_required`,
the ~4 s pending timeout, resync semantics. A subtly-wrong client wedges
the watch or thrashes resyncs (we already fixed this once — server-side
pending timeout, v0.26.1).

**Mitigation**: implement the documented boundary-table semantics in
`docs/specs/pane-delta.md`; fixture-test operation sequences; add a
client-side watchdog (watch stalls → re-watch); keep the native debug crash
surface logcat-friendly.

### Scope reality check
The relay, Android app, and catalog are substantial systems. Treating the
current repository as an unbounded rewrite would stall delivery.

**Mitigation**: keep changes within the numbered specifications, make each
landing independently reviewable, and use fixtures plus self-determinism
coverage for regression detection.

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

### Unsupported transport expansion
WebRTC, gateways, and public tunneling are outside the Tailscale-only product.

**Mitigation**: do not add an alternate path unless the transport ADR and wire
specification are deliberately revised first.

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
which change without notice.

**Mitigation**: readers stay behind `get_conversation_history` — the app
never sees raw formats. Update the relay reader and its fixtures together when
a supported format changes.

## Low severity / watchlist

- **Pane lease thrash** on keyboard open/close — coalesce lease requests per
  frame using Compose `snapshotFlow` debounce.
- **Unicode width** — grapheme clusters + East Asian wide chars in ANSI
  rows: exercise segmentation fixtures; use `BreakIterator` rather than
  `length`.
- **4 MiB send-buffer ceiling** — giant full frames evict the client;
  monitor pane sizes, prefer deltas, document the limit before raising it.
- **Spec drift** — `docs/` and `fixtures/` are the contract; behavior
  changes land only through specification updates and deliberate fixture
  revisions. The Rust self-determinism harness is a regression alarm on the
  outbound stream.
- **Keystore loss on backup/restore** — credentials wrapped by Keystore
  keys may not survive device replacement; pairing recovery UX (re-pair
  QR) must be easy.

## Explicitly accepted

- **Android-only client** — no PWA; the Rust relay ships no `web/`
  bundle. iOS/desktop are out of product scope.
- **No wire-format changes in flight** — any future change requires a
  deliberate protocol revision with specification and fixture updates.
