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

Lerdr began with the idea of reaching coding agents from a phone, inspired
by [0cv/herdr-mobile-relay](https://github.com/0cv/herdr-mobile-relay).
My first take used a Go relay and a Tauri-based mobile app. With access to
Cognition's SWE-2, I used Rust and Kotlin — a stack new to me — to build
this independent relay and native Android experience with coding agents.
The product now centers on a semantic feed, interactive terminal, and
workspace controls; its own specifications and frozen fixtures define its
behavior. It is not a fork, and development does not depend on consulting
the earlier project's source.

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

## Quick start

Requires [Herdr](https://github.com/0cv/herdr) ≥ 0.7.5 on macOS or Linux,
Tailscale, and an Android phone.

### 1 · Install the plugin — on the machine running Herdr

```sh
herdr plugin install IGUNUBLUE/lerdr-rust-kotlin/plugin
```

Herdr clones the repo, runs the `[[build]]` hook, and registers the
`lerdr.events` plugin. The hook downloads the checksum-verified
`lerdr-relay` bundle from the matching GitHub release — no Rust toolchain
required — then opens the **Lerdr: Setup** pane. Choose **Tailscale Serve**
there to publish the relay on this machine's tailnet HTTPS name and print
the private setup QR.

### 2 · Install the app — on your phone

Download `lerdr_<version>_universal.apk` from the
[latest release](https://github.com/IGUNUBLUE/lerdr-rust-kotlin/releases/latest)
and open it — Android asks once to allow installs from the source app.
From v0.0.12 the app checks GitHub releases on its own and can update
in place (Settings → App update).

### 3 · Pair

Scan the setup QR from the app's pairing screen — or paste the
`lerdr://pair` link — and the phone shows `1 computer · live`. Reprint
the QR anytime with the **Lerdr: Show Phone Setup QR** plugin action.

## Build from source

### Relay

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

## Contributing

Contributions welcome — including AI-assisted ones (disclose them; see
[CONTRIBUTING.md](CONTRIBUTING.md)). Bugs and ideas go through the issue
templates; security reports through
[private vulnerability reporting](SECURITY.md). Releases are tracked in
[CHANGELOG.md](CHANGELOG.md).

## Acknowledgements

Thanks to the creators of
[0cv/herdr-mobile-relay](https://github.com/0cv/herdr-mobile-relay)
for the original inspiration, and to
[Herdr](https://github.com/0cv/herdr) for the host integration.

## License

MIT — see [LICENSE](LICENSE).
