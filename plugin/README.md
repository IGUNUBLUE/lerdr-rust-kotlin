# Lerdr Herdr plugin

This directory is the Herdr plugin root for the Rust relay (`lerdr-relay`).
`herdr-plugin.toml` registers the plugin as `lerdr.events` — the same plugin
id the Go implementation ships under — with identical actions, panes, and the
`pane.agent_status_changed` event hook, plus a `[[startup]]` hook (new for the
Rust port; see *Deviations*).

## Install (managed)

The manifest lives in this subdirectory, so install with the subdirectory
form:

```sh
herdr plugin install <owner>/<repo>/plugin
```

Once this repository becomes the canonical `lerdr` release stream this is
simply `herdr plugin install IGUNUBLUE/lerdr` with the manifest at repo root —
the doc 09 hard requirement. `plugin install` clones the repo, shows a
preview, runs the `[[build]]` hook (`scripts/plugin-build.sh`), and registers
the actions/panes/hooks. The build hook never compiles Rust: it resolves the
manifest's `version` to tag `v<version>` on the release repository, downloads
`lerdr-relay_<version>_<os>_<arch>.tar.gz` plus `checksums.txt`, verifies the
SHA-256, extracts, and lets the binary self-verify
(`verify-release`/`seal-release`/`activate-release`/`prune-releases`).

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
→ `current/lerdr-relay` → `current/lerdr` (Go-era bundle) →
`current/herdr-mobile-relay` (pre-rename). The fallbacks keep hooks working
while `current` still points at a Go-era release during a transition.

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

## Deviations from the original Go implementation

- Manifest location: `plugin/herdr-plugin.toml` vs repo root — command paths
  are `scripts/…` instead of `relay/…`; install uses the subdir form.
- Binary name `lerdr` → `lerdr-relay`; assets `lerdr_V_*` → `lerdr-relay_V_*`.
- No asset-name fallback to `lerdr_*` tarballs — those contain the Go binary.
- Added `[[startup]]` → `scripts/plugin-on-startup.sh` execs
  `lerdr-relay startup-hook` (doc 09: re-assert `agent.view.set` after
  session restore/`live_handoff`). The original has no startup hook.
- Release tarball carries `scripts/` instead of `relay/` and no `web/` bundle
  or LICENSE yet; `web_hash`/`bundle_hash` manifest fields are kept for
  contract parity (the binary defines what they hash).
- `speech-voices.sh` not ported — unnecessary: the binary's `speech-voices`
  subcommand the script wrapped is implemented natively.
- `version` is `0.0.0`, synced to the workspace crates.
