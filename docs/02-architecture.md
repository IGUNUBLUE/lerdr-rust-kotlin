# 02 — Target architecture

```
┌─────────────────────────────────────────────────────────────┐
│ Phone — Kotlin/Compose Android app                           │
│ feed · terminal · workspace · settings/pairing               │
│ :core protocol/e2ee/transport/store                          │
└──────────────────────────┬──────────────────────────────────┘
                           │ WebSocket over Tailscale
                           │ herdr-e2ee-v2 / protocol v3
┌──────────────────────────▼──────────────────────────────────┐
│ Computer — Rust relay                                        │
│ axum: /ws, /healthz, /readyz · session actors · send buffer │
│ coordinator: topology, watches, leases, receipts             │
│ conversation, questions, push, uploads, speech, devices     │
└──────────────────────────┬──────────────────────────────────┘
                           │ Unix socket / Herdr CLI fallback
                           ▼
                         Herdr
```

The supported network path is Tailscale to a loopback-bound relay. Gateway,
Cloudflare tunnel, and WebRTC paths are not part of the deployed architecture.

## Rust workspace (`relay/`)

```
relay/
├── Cargo.toml                 # workspace
├── crates/
│   ├── lerdr-core/            # protocol types, action catalog, framing
│   ├── lerdr-e2ee/            # handshake, AEAD sessions, frame codecs
│   ├── lerdr-fixture/         # frozen fixture loader
│   ├── lerdr-herdr/           # Unix socket API, events, capabilities
│   ├── lerdr-coord/           # topology, actions, store-backed services
│   ├── lerdr-relay/           # WebSocket sessions and relay runtime
│   └── lerdr-shadow/          # scripted self-determinism harness
└── tests/                     # vectors and integration coverage
```

**Service topology**: actor model — channels own the boundaries, tasks
own the state. TopologyActor projects the Herdr event stream onto a
`watch::Sender`; per-pane watch tasks feed ordered frame channels; per-client
SessionActors own E2EE state + bounded send queues (lag → evict). Full
diagram and rules in [08 — Herdr boundary](08-herdr-boundary.md).

### Crate choices

| Need | Crate | Why |
|---|---|---|
| Async runtime | `tokio` | ecosystem gravity |
| HTTP/WS server | `axum` + `tokio-tungstenite` | mature, with raw frame control for the frozen protocol |
| Serialization | `serde` + `serde_json` | JSON wire contract; `serde_bytes` for binary payloads |
| Crypto | `p256`, `aes-gcm`, `hkdf`, `hmac`, `sha2` | pure-Rust, audited primitives matching the frozen vectors |
| Unix socket | `tokio::net::UnixStream` | Herdr socket API |
| Persistence | JSON files first (`serde_json` + atomic rename) then `rusqlite` if needed | current store is file-based; don't add a DB without need |
| Logging | `tracing` + `tracing-journald` | journald acceptance test exists |
| Web push | `web-push` crate or minimal VAPID+AES128GCM built from `p256`+`aes-gcm`+`hkdf`+`http` | evaluate dependency freshness against the required surface |
| CLI | `clap` | explicit Lerdr command interface |
| WebRTC | not in the current transport plan | Tailscale-only deployment |

## Kotlin app (`app/`)

The current Gradle settings declare:

```
app/
├── core/
│   ├── model/          # shared wire and UI models
│   ├── protocol/       # protocol-v3 DTOs and codecs
│   ├── e2ee/           # handshake and encrypted session
│   ├── terminal/       # ANSI and pane-delta rendering state
│   ├── testing/        # fakes and fixture loading
│   ├── transport/      # WebSocket connection and E2EE transport
│   ├── store/          # local state and synchronization
│   ├── conversation/   # conversation data
│   ├── designsystem/   # Compose design system
│   └── data/           # repositories
├── navigation/         # app navigation
└── app/                # Android application
```

### Current dependencies

| Need | Choice |
|---|---|
| UI | Compose + Material 3 |
| Serialization | `kotlinx.serialization` |
| WebSocket | OkHttp |
| E2EE | JCE primitives plus the in-repo handshake implementation |
| Local state | DataStore-backed store modules |
| DI | Hilt |
| QR scan | CameraX + ML Kit Barcode Scanning |
| Biometric | `androidx.biometric` |
| Compression | `zstd-jni` for negotiated `frame_zstd` payloads |
| Testing | JUnit, Truth, Turbine, MockWebServer, Robolectric, Roborazzi |

## Push notifications on a native app — decision

The Android app uses a foreground service holding the E2EE socket to raise
local notifications over Tailscale/LAN. This is self-hosted and needs no
third-party notification provider; its cost is a persistent-service
notification and OEM battery-management UX.

**Decision: foreground service is the primary channel.** An FCM or
UnifiedPush adapter remains a future opt-in product decision, not a required
or currently shipped client path.

## The seam strategy

```
Rust relay ⇄ Kotlin app   (the shipped combination — protocol v3 over
                           herdr-e2ee-v2, E2EE end to end)
Rust relay ⇄ test client  (self-mode determinism and regression coverage
                           for the outbound stream)
```

Each side validates independently against the frozen contract: the app
against golden vectors and scripted relay sessions, the relay against the same
vectors plus the protocol-level test client in `tools/shadow`. A peer that
speaks `protocol v3` and passes the applicable vectors is conformant.

## Deliberate boundaries

- **Herdr** — the relay is its client; Herdr remains an external integration.
- **The protocol** — v3 + `herdr-e2ee-v2` stay the contract. Improvements
  land as negotiated capabilities, never silently.
- **Web client and Go relay** — retired. Lerdr ships the Rust relay and the
  Kotlin Android app only; historical provenance is recorded in
  [00 — Inventory](00-inventory.md).
