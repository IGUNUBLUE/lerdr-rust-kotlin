# 02 — Target architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Phone — Kotlin app (Compose, M3 Expressive)                │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────────┐  │
│  │ feed UI  │  │terminal  │  │workspace │  │ settings /  │  │
│  │ (semantic│  │ renderer │  │ git UI   │  │ pairing UI  │  │
│  │  layer)  │  │          │  │          │  │             │  │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └──────┬──────┘  │
│       └──────────────┴────────────┴───────────────┘          │
│              :core:store (Flow state, single source)         │
│       ┌──────────────┴──────────────┐                        │
│  :core:protocol            :core:e2ee                        │
│  (kotlinx.serialization)   (JCE/Conscrypt)                   │
│       └──────────────┬──────────────┘                        │
│              :core:transport (OkHttp WS / WebRTC)            │
└──────────────────────┬──────────────────────────────────────┘
                       │ herdr-e2ee-v2 (AES-256-GCM frames)
        ┌──────────────┼───────────────┐
        │   Tailscale  │  gateway/rust │ Cloudflare
        ▼              ▼               ▼
┌─────────────────────────────────────────────────────────────┐
│  Computer — Rust relay (single static binary)               │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ axum HTTP: /ws + /healthz — no web assets (Android-    │ │
│  │ only product; the Go relay can keep serving the PWA    │ │
│  │ in parallel during coexistence if wanted)              │ │
│  ├────────────────────────────────────────────────────────┤ │
│  │ session actor per client: send buffer, coalescing,     │ │
│  │ eviction, E2EE session (p256 + aes-gcm + hkdf)         │ │
│  ├────────────────────────────────────────────────────────┤ │
│  │ coordinator: per-pane scheduler, leases, receipts      │ │
│  ├───────────────┬────────────────────────────────────────┤ │
│  │ pane watch    │ herdr client (Unix socket + CLI shim)  │ │
│  │ fingerprint + │ events stream → inventory/projections  │ │
│  │ delta engine  │                                        │ │
│  ├───────────────┴────────────────────────────────────────┤ │
│  │ conversation readers (JSONL), question parser,         │ │
│  │ push (web-push), uploads, speech, device store         │ │
│  └────────────────────────────────────────────────────────┘ │
│                          │ Unix socket / herdr CLI          │
└──────────────────────────┼──────────────────────────────────┘
                           ▼
                    Herdr (unchanged)
```

## Rust workspace (`relay/`)

```
relay/
├── Cargo.toml                 # workspace
├── crates/
│   ├── lerdr-core/            # protocol types, action catalog, framing
│   ├── lerdr-e2ee/            # handshake + AEAD sessions + frame codecs
│   ├── lerdr-herdr/           # Unix socket API, event stream, CLI fallback
│   │                          #   singleflight + dial semaphore +
│   │                          #   DispatchError taxonomy (see doc 08)
│   │                          #   SchemaRegistry (`herdr api schema --json`)
│   │                          #   events supervisor w/ events_lost
│   │                          #   reconcile + wait primitives
│   ├── lerdr-watch/           # pane fingerprints, delta engine, probe loop
│   ├── lerdr-coord/           # per-pane scheduler, receipts, ledger
│   ├── lerdr-store/           # device credentials, config, stable state
│   ├── lerdr-push/            # web-push (VAPID), policy, queue
│   ├── lerdr-conversation/    # JSONL readers → Entry pages
│   ├── lerdr-question/        # pane→structured question parser
│   ├── lerdr-gateway/         # gateway relay (lerdr-gateway binary)
│   └── lerdr-relay/           # binary: axum server wiring everything
└── tests/
    ├── vectors/               # golden fixtures generated from Go impl
    └── interop/               # Rust↔Go handshake + frame roundtrips
```

**Service topology**: actor model — channels own the boundaries, tasks
own the state. TopologyActor projects the Herdr event stream onto a
`watch::Sender`; per-pane watch tasks feed ordered frame channels;
per-client SessionActors own E2EE state + bounded send queues (lag →
evict, mirroring the Go sendbuffer contract). Full diagram and rules in
[08 — Herdr boundary](08-herdr-boundary.md).

### Crate choices

| Need | Crate | Why |
|---|---|---|
| Async runtime | `tokio` | ecosystem gravity |
| HTTP/WS server | `axum` + `tokio-tungstenite` | mature, and we need raw frame control for the dual codec |
| Serialization | `serde` + `serde_json` | wire parity is JSON today; `serde_bytes` for binary codec |
| Crypto | `p256`, `aes-gcm`, `hkdf`, `hmac`, `sha2` | pure-Rust, audited, exact parity with Go stdlib usage |
| Unix socket | `tokio::net::UnixStream` | Herdr socket API |
| Persistence | JSON files first (`serde_json` + atomic rename) then `rusqlite` if needed | current store is file-based; don't add a DB without need |
| Logging | `tracing` + `tracing-journald` | journald acceptance test exists |
| Web push | `web-push` crate or port minimal VAPID+AES128GCM via `p256`+`aes-gcm`+`hkdf`+`http` | the dependency surface is small; evaluate freshness |
| CLI parity | `clap` | same flags/subcommands as cmd/lerdr |
| WebRTC (phase 2) | `webrtc` (webrtc-rs) | only for the server side of `herdr-dc-v1` |

## Kotlin app (`app/`)

Aligned to the nowinandroid modularization guide (single feature
modules — the api/impl split was dropped for Navigation 3):

```
app/
├── settings.gradle.kts
├── gradle/libs.versions.toml    # pinned: compose-bom-alpha
├── core/
│   ├── model/                   # shared DTOs — mirrors protocol.go
│   ├── data/                    # repositories; expose Flows, never
│   │                            #   snapshots; WS deltas reconcile in
│   ├── network/                 # OkHttp WS, E2EE session, backoff,
│   │                            #   keepalive, gateway path
│   ├── crypto/                  # handshake (ECDH P-256, AES-GCM, HKDF),
│   │                            #   Keystore-wrapped credential storage
│   ├── terminal/                # ANSI→AnnotatedString, delta applier,
│   │                            #   frame store, fingerprint chain
│   ├── conversation/            # paging source for Entry feeds
│   ├── designsystem/            # M3E theme + expressive wrappers
│   ├── ui/                      # shared components (agent row, tool card)
│   ├── datastore/               # prefs, credentials, per-agent drafts
│   ├── notifications/           # channels, push-open deep links
│   ├── service/                 # foreground connection service
│   └── testing/                 # fakes for all repos + fixture loaders
├── feature/
│   ├── agents/                  # home mission control
│   ├── session/                 # feed + terminal + details modes
│   ├── workspaces/              # tree, files, git status/diffs
│   ├── activity/                # journal + detail
│   ├── pairing/                 # QR, clipboard, invitations, devices
│   └── settings/                # relays, push, speech, updates
├── navigation/                  # Nav3 entries + top-level destinations
├── app/                         # Application, MainActivity, nav host, DI
└── app-benchmarks/              # Macrobenchmark + Baseline Profile
```

### Library choices

| Need | Choice | Why |
|---|---|---|
| UI | Compose, `material3` 1.5.x-alpha (expressive) via `compose-bom-alpha` | the design target |
| Serialization | `kotlinx.serialization` | shared DTO definitions, protobuf-ready later |
| WebSocket | `OkHttp` `WebSocket` | binary frames, ping control, battle-tested |
| Crypto | JCE/Conscrypt (`Cipher` AES/GCM, `KeyAgreement` ECDH P-256) + hand-rolled HKDF (~20 lines) | no new dependency for crypto that must match byte-for-byte; Keystore for at-rest credential protection |
| Storage | `DataStore` (prefs) + Keystore-backed secret store + `Room` only if history caching needs it | current app uses localStorage equivalents |
| DI | `Hilt` (or hand-rolled if we keep it small) | conventional |
| QR scan | ML Kit Barcode Scanning | parity with tauri barcode-scanner |
| Biometric | `androidx.biometric:biometric` | parity |
| Markdown | compose-markdown (multiplatform-markdown-renderer) | conversation text |
| Images | `Coil` | workspace file previews, attachments |
| Voice input | Android `SpeechRecognizer`; TTS stays relay-side (`speak_text`) | parity |
| WebRTC | `stream-webrtc-android` or Google's `webrtc` AAR — **phase 2 decision**, it adds ~30-40 MB | direct-path upgrade can ship later; WS path is complete |
| Notifications | foreground service + local `NotificationManager` — see below |

## Push notifications on a native app — decision

The relay pushes via Web Push (VAPID). A native app has three paths:

1. **Foreground service holding the E2EE socket** → local notifications.
   Works over Tailscale/LAN, zero third-party, true to self-hosting. Cost:
   a persistent-service notification + OEM battery killers (Xiaomi/OPPO
   need `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` UX).
2. **FCM**: relay posts to FCM HTTP v1 — needs a Firebase project and a
   service-account key on every self-hosted relay. Bad fit for the
   install model.
3. **UnifiedPush** distributor (ntfy etc.): optional third-party again.

**Decision: option 1 as the primary channel** (the socket is already
always-on for realtime), with a documented FCM adapter interface in
`:core:push` for a future opt-in flavor. The existing web-push infra
keeps serving PWA clients unchanged.

## The seam strategy — why this is low-risk

```
Go relay ⇄ Web PWA        (today, production — stays running for any
                           non-Android device during transition)
Go relay ⇄ Kotlin app     (phase 1 target — validates the client half)
Rust relay ⇄ test client  (shadow parity harness vs the Go oracle)
Rust relay ⇄ Kotlin app   (end state — the only shipped combination)
```

Every combination in the matrix is a supported configuration at every
point in time. No flag day on either side. The Rust relay validates
against a **protocol-level test client** (fixtures + scripted watch
sessions) rather than the PWA, since the PWA is out of product scope.

## What deliberately does NOT move

- **Herdr** — the relay is a client of it; unchanged.
- **The protocol** — v3 + `herdr-e2ee-v2` stay the contract. Improvements
  land as negotiated capabilities, never silently.
- **The Go codebase** — stays as reference implementation and test-vector
  generator until cutover criteria in [05 — Roadmap](05-roadmap.md) pass.
  Its PWA-serving side stays useful for any non-Android device during the
  transition, but no new web UI work is planned.
