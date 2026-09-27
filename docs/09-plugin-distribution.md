# 09 — Distribution as a Herdr plugin

Hard requirement: the Rust relay installs through Herdr's plugin workflow:
`herdr plugin install IGUNUBLUE/lerdr-rust-kotlin/plugin`. The manifest
lives under `plugin/`; local development uses `herdr plugin link plugin/`.
This specification records the relevant Herdr integration facts and the
current `plugin/herdr-plugin.toml`.

Marketplace discovery: the repo carries the GitHub topic `herdr-plugin`,
which is how `herdr.dev/plugins` indexes it — the crawler lists any public
repo with that topic and a parseable `herdr-plugin.toml` on the default
branch (subdirectory manifests count; one card per repo, one row per
manifest). Do not remove the topic; the index refreshes roughly every 30
minutes and rescans on default-branch head changes.

## The manifest contract

```toml
id = "lerdr.events"
name = "Lerdr"
version = "0.0.10"          # bump per release
min_herdr_version = "0.7.5" # raise only when a used method requires it
platforms = ["macos", "linux"]

[[build]]
command = ["bash", "scripts/plugin-build.sh"] # downloads verified binary

[[actions]] / [[panes]] / [[events]] / [[startup]] / [[link_handlers]]
```

Plugin rules that shape Lerdr packaging:

- `plugin install` clones the `plugin/` subdirectory, shows a preview, runs
  `[[build]]`, and registers entry points. **Build commands get no socket env
  and no toolchain guarantee** — `scripts/plugin-build.sh` downloads the
  checksum-verified release tarball and never compiles.
- Changing `herdr-plugin.toml` during build aborts install — version bump
  happens in the release PR, not at build time.
- `plugin link plugin/` skips `[[build]]`; local development uses the checkout.
- Managed-plugin refresh is performed by reinstalling the subdirectory.
- The manifest is re-read at every server start.

## Runtime environment Herdr injects

| Var | Use |
|---|---|
| `HERDR_SOCKET_PATH` | socket discovery — the Rust client reads this first |
| `HERDR_BIN_PATH` | portable CLI fallback (Unix socket vs Windows pipe) |
| `HERDR_ENV=1` | "inside Herdr" marker |
| `HERDR_PLUGIN_ID` / `_ROOT` / `_CONFIG_DIR` / `_STATE_DIR` | plugin identity + dirs — credentials/state go under CONFIG/STATE, never ROOT (managed checkout) |
| `HERDR_PLUGIN_CONTEXT_JSON` | workspace/tab/pane/agent/selection/link context per invocation |
| `HERDR_PLUGIN_EVENT` (=startup for hooks) + `HERDR_PLUGIN_EVENT_JSON` | event hook payloads |
| `HERDR_PLUGIN_ACTION_ID` / `HERDR_PLUGIN_ENTRYPOINT_ID` | which action/pane fired |

## Runtime contract

1. **Artifact layout**: `~/.local/share/lerdr/current/lerdr-relay` is the
   installed binary. `plugin/scripts/plugin-build.sh` installs the verified
   release and resolves `LERDR_RELEASE_ROOT`/`HERDR_RELEASE_ROOT`.
2. **Binary helpers**: `event-hook`, `startup-hook`, `setup-fragment`,
   `normalize-origin`, `qr`, `support`, and `version` are binary subcommands.
3. **Setup, status, and service management** are implemented by
   `plugin/scripts/*.sh`; Herdr executes those scripts directly.
4. **`event-hook` exists alongside socket events**: the hook runs in Herdr's
   process context and covers a relay subscription outage such as a restart.
5. **Plugin logs**: `herdr plugin log list` surfaces hook-script output.

## Plugin integration capabilities

| Mechanism | Lerdr use |
|---|---|
| `[[startup]]` hook | Relay re-asserts socket/event/view state after session restore or live handoff |
| `[[link_handlers]]` | Matched GitHub URLs open a QR deep-link pane for the phone |
| `plugin.pane.open` placements | Setup, status, and link actions open managed panes |
| `HERDR_PLUGIN_CONTEXT_JSON` | Actions receive workspace/tab/pane/agent context |
| `herdr api schema --json` | Runtime capability discovery |

## Release packaging

- CI builds `lerdr-relay` for
  `{x86_64,aarch64}-{unknown-linux-musl,apple-darwin}`; musl produces static
  Linux binaries.
- Release tarballs contain `lerdr-relay`, the operator script set, and a
  release manifest consumed by `plugin/scripts/plugin-build.sh`.
- `plugin-build.sh` checksum-verifies the selected tarball and never compiles
  on user hosts.

## Herdr integration backlog

- `client_shell.endpoint` capability + `command.invoke`: evaluate whether the
  Kotlin app should consume client-shell projections directly.
- `agent.view.set` projections are transient and per-server; the
  `[[startup]]` hook reapplies Lerdr's view when enabled.
