# lerdr

> **Status: beta / in development.** Working end-to-end, but still
> pre-release software — expect rough edges, moving pieces, and no
> compatibility guarantees between versions yet. Releases are marked
> pre-release on purpose.

Your coding agents, live on your phone — sessions, questions, approvals,
terminals, and workspace files, over an end-to-end encrypted channel.

A **Rust relay** + **Kotlin / Jetpack Compose (Material 3 Expressive)
Android app** speaking `protocol v3` over `herdr-e2ee-v2`, backed by a
[Herdr](https://github.com/0cv/herdr) host. Remote transport: Tailscale.

![mission-control mockup](docs/mockup.png)

## Why lerdr exists

I was inspired by [0cv/herdr-mobile-relay](https://github.com/0cv/herdr-mobile-relay)
— the idea of reaching your coding agents from your phone. My first take
kept the same stack (a Go relay) plus a Tauri-based mobile app. When I got
access to Cognition's SWE-2 and wanted to put the model through a real
test, I picked a stack I don't work in — Rust + Kotlin — and let the
agents rebuild the product. That's how lerdr started.

The name: the iguana is an animal I've always found curious.

## What it does

- **Mission-control home** — agents grouped by workspace with
  working / attention / idle status, a needs-you rail with one-tap
  approvals, per-relay connectivity.
- **Session** — Feed (structured conversation, question and approval
  cards), Terminal (full ANSI pane, key bar, find), Files (workspace
  tree, preview, git status/diff).
- **Pairing** — QR or `lerdr://pair` deep link; biometric lock; device
  and relay management from Settings.
- **E2EE** — P-256 handshake + AES-GCM sealed frames between the phone
  and the relay, riding over Tailscale's WireGuard path.
- **Frozen wire contract** — `protocol v3` is anchored by committed
  golden vectors (`fixtures/`) and a determinism harness
  (`tools/shadow/`): two fresh relays against one fake Herdr must emit
  byte-identical normalized streams.

## Run it

### Relay — on the machine running Herdr

```sh
cd relay && cargo build --release -p lerdr-relay
./target/release/lerdr-relay serve \
    --host 127.0.0.1 --port 8377 --token <32-byte-secret>
```

### Tailscale transport

```sh
tailscale serve --bg --tcp=8377 tcp://localhost:8377
# or the managed scripts: plugin/scripts/tailscale-serve.sh start
```

### Pair the app

```sh
./target/release/lerdr-relay qr        # prints a one-shot link/QR
kill -USR1 <relay-pid>                 # re-arm a fresh invitation
```

Scan in the app → `1 computer · live`.

### Android app

```sh
cd app && ./gradlew :app:assembleDebug   # JDK 17, Android SDK 37.2
```

### Releases

Tag `v<x.y.z>` and `.github/workflows/release.yml` builds
`lerdr-relay` tarballs (linux musl amd64/arm64, darwin amd64/arm64),
`checksums.txt`, and the universal APK (signed when keystore secrets
are configured). See [docs/release.md](docs/release.md).

## Layout

| Path | Contents |
|---|---|
| `relay/` | Rust workspace — axum WS server, per-pane watchers, send buffers, Herdr socket client, update worker |
| `app/` | Kotlin + Compose M3E — nowinandroid-style modules (`:core:*`, `:app`) |
| `docs/` | The spec: protocol, architecture, UX design, roadmap, spec-gaps |
| `fixtures/` | Frozen golden vectors anchoring `protocol v3` |
| `tools/shadow/` | Determinism harness (`rust-a` vs `rust-b` through one fake Herdr) |
| `plugin/` | Herdr plugin manifest + operator scripts (install, tailscale-serve, release packaging) |

## Acknowledgements

Thanks to [0cv/herdr-mobile-relay](https://github.com/0cv/herdr-mobile-relay)
for the original idea — this project exists because that one did.

## License

MIT — see [LICENSE](LICENSE).
