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

## Omp lifecycle status

Install Herdr's omp integration in the profile that actually runs the
agent. Without it, Herdr recognizes omp but can report `idle` during
working turns, including in-process subagent work. Lerdr preserves that
upstream lifecycle; it does not infer it from terminal spinners.

For the default omp agent directory:

```sh
env -u PI_CODING_AGENT_DIR -u PI_CONFIG_DIR herdr integration install omp
```

For a named profile, replace `<profile>` with its name:

```sh
env -u PI_CODING_AGENT_DIR PI_CONFIG_DIR=".omp/profiles/<profile>" \
    herdr integration install omp
```

Unsetting `PI_CODING_AGENT_DIR` for the installer avoids resolving Pi and
omp to the same extension directory, which Herdr refuses. This does not
change the running agent's environment. New sessions load the extension
on startup; use `/reload` in already-open omp sessions.

Verify inside the intended profile's Herdr pane:

```sh
herdr integration status
herdr agent explain "$HERDR_PANE_ID"
```

The integration should be current at that profile's extension path and
live explain should use lifecycle reporting rather than
`default_known_agent_idle_fallback`. Worktree-based `orchestrating`
presentation only covers separate busy child panes, not tasks inside
one omp process.

## Codex Feed history

Install Herdr's Codex integration in the Codex home used by the agent:

```sh
herdr integration install codex
```

The integration reports the conversation identifier after Codex starts a
session. If Codex requests hook review, inspect and trust the installed Herdr
hook through `/hooks`. A current integration file alone does not prove that
the running agent has loaded and executed it.

Lerdr launches Codex with `--no-daemon` when its installed `--help` advertises
that option. The shared Codex daemon can inherit another pane's Herdr context,
preventing the session from being associated with the phone's agent. Older
Codex versions retain their existing arguments. For a manually started Codex
with this behavior, use `codex --no-daemon` in its Herdr pane.

After the first message, `herdr agent get <agent-name-or-pane-id>` should
include `agent_session.value`. Feed requires that identifier and an accessible
transcript. An empty identifier must not be replaced by the newest conversation
in the same directory: several agents can share a workspace.


## How the setup QR is produced

With the installed user service running, print another QR directly:

```sh
lerdr-relay pairing qr
```

The native command reads the running systemd service configuration on Linux,
or the shipped LaunchAgent's environment file on macOS. It detects an existing
Tailscale HTTPS root proxy or TCP forward to the relay's actual port, sends
`SIGUSR1`, and waits for the fresh invitation before printing `lerdr://pair`.
It does not configure Tailscale, restart the relay, rotate its key, or revoke
phones. Missing or ambiguous TCP routes fail before rearming. A valid HTTPS
root proxy takes precedence over TCP; subpath proxies and `/ws` overrides
are not supported.
Treat the QR and link as secrets. Available since 1.1.0.

The plugin actions' script chain:

1. `tailscale-serve.sh start` (Setup menu → Tailscale Serve, or the
   `tailscale-setup` action) publishes the relay on this machine's tailnet
   HTTPS name. On a fresh install the pane first runs
   `install-tailscale-service.sh`, so the relay is running and `relay.env`
   holds its token. The relay port comes from `relay.env` (default 8375).
   An existing HTTPS root proxy or TCP forward to that port is kept.
2. With the service installed, the QR comes from `lerdr-relay pairing qr`
   (above) for either route: a `lerdr://pair#…` link that opens the app.
   On macOS a Tailscale.app-only CLI is exposed to it through a temporary
   `tailscale` wrapper on PATH.
3. Fallback, for HTTPS only, when no service is installed or `pairing qr`
   fails: `setup-link.sh` asks the binary for the fragment
   (`lerdr-relay setup-fragment <token> <host-label> <wss-url>`), renders
   `https://<tailnet>/#<fragment>` with `lerdr-relay qr --columns <n>`, and
   `arm_setup_link` sends `SIGUSR1` to the pid in `relay.pid`. The relay
   serves no page at that URL, so the output points at the app's scanner
   and paste field.

Reprinting (`tailscale-serve.sh link`, or the **Lerdr: Show Phone Setup
QR** action) repeats step 2, or step 3 when step 2 is unavailable. Every
print re-arms the **one-shot invitation** (one phone, ~10 minutes). Both
link shapes carry the relay key as the bootstrap `setup=` secret, which is
why the output warns against sharing screenshots.

The invitation/credential handshake itself is wire-level and lives in
`docs/03-protocol.md`.

## Local development (link)

```sh
herdr plugin link plugin/        # from the repo root
# or: cd plugin && herdr plugin link .
```

`plugin link` **skips `[[build]]`** — no download happens. The actions, panes,
and hooks run from this checkout. To point them at a locally built binary:

```sh
cd relay && cargo build --release -p lerdr-coord --bin lerdr-relay
LERDR_RELAY_BIN=relay/target/release/lerdr-relay \
    herdr plugin action invoke setup --plugin lerdr.events
```

`LERDR_RELAY_BIN`/`HERDR_RELAY_BIN` override binary resolution everywhere;
`LERDR_RELEASE_ROOT`/`HERDR_RELEASE_ROOT` override the install root.

## Building the binary

```sh
cd relay
cargo build --release -p lerdr-coord --bin lerdr-relay   # native dev build
```

Distribution builds must be **single-file static**: musl on Linux.

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo build --release -p lerdr-coord --bin lerdr-relay --target x86_64-unknown-linux-musl
cargo build --release -p lerdr-coord --bin lerdr-relay --target aarch64-unknown-linux-musl
# macOS (from Apple SDK hosts):
cargo build --release -p lerdr-coord --bin lerdr-relay --target x86_64-apple-darwin
cargo build --release -p lerdr-coord --bin lerdr-relay --target aarch64-apple-darwin
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
`release_version`/`revision` for exact-release verification.

## Current plugin contract

- The manifest lives at `plugin/herdr-plugin.toml`; command paths use
  `scripts/…`, and installation uses the subdirectory form.
- Release archives use `lerdr-relay` and `lerdr-relay_V_*` asset names.
  Legacy names remain readable only for existing installations.
- `[[startup]]` runs `scripts/plugin-on-startup.sh`, which invokes
  `lerdr-relay startup-hook` to re-assert `agent.view.set` after session
  restore or `live_handoff`.
- Release tarballs carry `scripts/` rather than a `relay/` directory.
  `verify-release` validates the manifest's file hashes; installation and
  rollback health checks require the exact release version and revision.
  Rust-only bundles do not require a frontend `web_hash` or `bundle_hash`.
- `speech-voices` is a native binary subcommand; no wrapper script is needed.
- `version` is `0.0.0`, synced to the workspace crates.
