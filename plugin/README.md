# Lerdr Herdr plugin

This directory is the Herdr plugin root for the Rust relay (`lerdr-relay`).
`herdr-plugin.toml` registers `lerdr.events`, its actions and panes, the
`pane.agent_status_changed` event hook, and the `[[startup]]` hook.

## Install (managed)

The manifest lives in this subdirectory, so install with the subdirectory
form:

```sh
herdr plugin install IGUNUBLUE/lerdr-rust-kotlin/plugin
```

This repository is the canonical Lerdr release stream. `plugin install`
clones the repo, shows a
preview, runs the `[[build]]` hook (`scripts/plugin-build.sh`), and registers
the actions/panes/hooks. The build hook never compiles Rust: it resolves the
manifest's `version` to tag `v<version>` on the release repository, downloads
`lerdr-relay_<version>_<os>_<arch>.tar.gz` plus `checksums.txt`, verifies the
SHA-256, extracts, and lets the binary self-verify
(`verify-release`/`seal-release`/`activate-release`/`prune-releases`).

## How the setup QR is produced

The chain, end to end:

1. `tailscale-serve.sh start` (Setup menu → Tailscale Serve, or the
   `tailscale-setup` action) publishes the relay on this machine's tailnet
   HTTPS name, then calls `setup-link.sh <fqdn>`.
2. `setup-link.sh` loads `relay.env` and asks the binary for the pairing
   fragment: `lerdr-relay setup-fragment <token> <host-label> <wss-url>`.
   The fragment carries the bootstrap payload for `https://<tailnet>/#<fragment>`.
3. The same binary renders the code: `lerdr-relay qr --columns <n> <url>`
   draws a terminal QR sized to the pane (skipped when the pane is too
   narrow — the plain link is always printed too, as an OSC 8 hyperlink).
4. `arm_setup_link` sends `SIGUSR1` to the pid in `relay.pid` — printing a
   QR is asking for one more pairing, so the running relay arms a fresh
   **one-shot invitation** (one phone, ~10 minutes) before the link is shown.

Reprinting (`tailscale-serve.sh link`, the **Lerdr: Show Phone Setup QR**
action, or `kill -USR1 <relay-pid>`) repeats steps 2–4. The QR embeds the
relay token — that is why the scripts warn against sharing screenshots of
it. The invitation/credential handshake itself is wire-level and lives in
`docs/03-protocol.md`.

## Local development (link)

```sh
herdr plugin link plugin/        # from the repo root
# or: cd plugin && herdr plugin link .
```

`plugin link` **skips `[[build]]`** — no download happens. The actions, panes,
and hooks run from this checkout. To point them at a locally built binary:

```sh
cd relay && cargo build --release -p lerdr-relay
LERDR_RELAY_BIN=relay/target/release/lerdr-relay \
    herdr plugin action invoke setup --plugin lerdr.events
```

`LERDR_RELAY_BIN`/`HERDR_RELAY_BIN` override binary resolution everywhere;
`LERDR_RELEASE_ROOT`/`HERDR_RELEASE_ROOT` override the install root.

## Building the binary

```sh
cd relay
cargo build --release -p lerdr-relay          # native dev build
```

Distribution builds must be **single-file static**: musl on Linux.

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo build --release -p lerdr-relay --target x86_64-unknown-linux-musl
cargo build --release -p lerdr-relay --target aarch64-unknown-linux-musl
# macOS (from Apple SDK hosts):
cargo build --release -p lerdr-relay --target x86_64-apple-darwin
cargo build --release -p lerdr-relay --target aarch64-apple-darwin
```

## Cutting a release

```sh
plugin/scripts/package-release.sh <VERSION> <REVISION> [OUTPUT_DIR]
```

`<VERSION>` must equal `version` in `herdr-plugin.toml` (bump the manifest in
the release PR — changing it during a build aborts in-flight installs). The
script builds all four targets, stages `lerdr-relay` + the operator script
subset under `scripts/` + `release-manifest.json` (via `lerdr-relay
release-manifest`), verifies each bundle, and writes
`lerdr-relay_<V>_<os>_<arch>.tar.gz` + `checksums.txt` to `dist/release/`.
`LERDR_BUILD_VERSION`/`LERDR_BUILD_REVISION` are exported for compile-time
stamping — the crate should read them via `option_env!`/build.rs.

Then upload the four tarballs and `checksums.txt` to the GitHub release tagged
`v<VERSION>`.

Verify a produced archive end-to-end on a matching host:

```sh
plugin/scripts/check-installed-release.sh \
    dist/release/lerdr-relay_<V>_<os>_<arch>.tar.gz \
    dist/release/checksums.txt <V> <REVISION> <os>/<arch>
```

## Runtime layout

```
~/.local/share/lerdr/
    current -> releases/<version>-<revision>-<os>-<arch>/
        lerdr-relay                 # the static binary
        scripts/                    # wrappers shipped inside the tarball
        release-manifest.json       # {version, revision, web_hash, ...}
    .lerdr-installation             # ownership sentinel
~/.local/bin/lerdr-relay            # PATH shim -> current/lerdr-relay
$HERDR_PLUGIN_CONFIG_DIR/relay.env  # token, instance id (plugin-managed)
~/.config/systemd/user/lerdr.service            (Linux service)
~/Library/LaunchAgents/com.lerdr.service.plist  (macOS service)
```

Binary resolution order in every script: `$LERDR_RELAY_BIN`/`$HERDR_RELAY_BIN`
→ `current/lerdr-relay` → `current/lerdr` (legacy name) →
`current/herdr-mobile-relay` (pre-rename). The fallbacks support existing
installations while the current release is upgraded.

## Herdr-injected environment

| Var | Use |
|---|---|
| `HERDR_SOCKET_PATH` | Herdr API socket — the relay connects here |
| `HERDR_BIN_PATH` | absolute path to the `herdr` CLI (used by `open-plugin-pane.sh`) |
| `HERDR_ENV=1` | "inside Herdr" marker |
| `HERDR_PLUGIN_ID` | `lerdr.events` |
| `HERDR_PLUGIN_ROOT` | this directory (managed clone or link target) |
| `HERDR_PLUGIN_CONFIG_DIR` | persistent config — `relay.env`, `device-auth/`, `push/` |
| `HERDR_PLUGIN_STATE_DIR` | persistent state dir |
| `HERDR_PLUGIN_CONTEXT_JSON` | pane/agent/workspace context per invocation |
| `HERDR_PLUGIN_EVENT` / `HERDR_PLUGIN_EVENT_JSON` | hook payloads (`startup` for `[[startup]]`) |
| `HERDR_PLUGIN_ACTION_ID` / `HERDR_PLUGIN_ENTRYPOINT_ID` | fired action/pane id |

Operator overrides use the `LERDR_` spelling first and the `HERDR_` spelling
as fallback (`relay_env` in `scripts/common.sh`); host-injected `HERDR_*`
variables are never folded through that helper.

## Binary subcommand contract

The scripts assume `lerdr-relay` implements this argv surface:
`serve`, `event-hook`, `startup-hook` (new), `setup-fragment`,
`normalize-origin`, `qr`, `support`, `speech-voices`, `release-manifest`,
`verify-release`, `seal-release`, `activate-release`, `prune-releases`, plus
SIGUSR1 re-arm of the setup invitation and `relay.pid` beside `relay.env`.
`/healthz` must report `status`/`instance`/`version`/`protocol` and
`release_version`/`revision`/`bundle_hash` for exact-release verification.

## Current plugin contract

- The manifest lives at `plugin/herdr-plugin.toml`; command paths use
  `scripts/…`, and installation uses the subdirectory form.
- Release archives use `lerdr-relay` and `lerdr-relay_V_*` asset names.
  Legacy names remain readable only for existing installations.
- `[[startup]]` runs `scripts/plugin-on-startup.sh`, which invokes
  `lerdr-relay startup-hook` to re-assert `agent.view.set` after session
  restore or `live_handoff`.
- Release tarballs carry `scripts/` rather than a `relay/` directory.
  `web_hash` and `bundle_hash` remain release-manifest fields defined by the
  binary.
- `speech-voices` is a native binary subcommand; no wrapper script is needed.
- `version` is `0.0.0`, synced to the workspace crates.
