# 09 — Distribution as a Herdr plugin

Hard requirement: the Rust relay installs exactly like today —
`herdr plugin install IGUNUBLUE/lerdr`. The plugin contract is part of the
product, not packaging detail. Source: `herdrdev/herdr` docs
(`docs/next/website/src/content/docs/plugins.mdx`, `socket-api.mdx`) and
the current `herdr-plugin.toml`.

## The manifest contract (unchanged)

```toml
id = "lerdr.events"
name = "Lerdr"
version = "0.27.0"          # bump per release
min_herdr_version = "0.7.5" # raise only when a used method requires it
platforms = ["macos", "linux"]

[[build]]
command = ["bash", "relay/plugin-build.sh"]   # downloads verified binary

[[actions]] / [[panes]] / [[events]] / [[startup]] / [[link_handlers]]
```

Plugin rules from upstream docs that shape our packaging:

- `plugin install` clones the repo, shows a preview, runs `[[build]]`
  commands, registers entry points. **Build commands get no socket env and
  no toolchain guarantee** — the current `plugin-build.sh` pattern
  (download the checksum-verified release tarball, never compile) is
  exactly right for Rust: same script, different artifact inside.
- Changing `herdr-plugin.toml` during build aborts install — version bump
  happens in the release PR, not at build time.
- `plugin link` skips `[[build]]` — local dev compiles the checkout itself.
- No `plugin update` in v1 — reinstall refreshes a managed plugin; our
  self-update path (`install_update`/`deploy_app_update` actions) remains
  the in-place upgrade story.
- Manifest is re-read at every server start — keep it stable and valid.

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

## What the Rust port must preserve

1. **Same artifact layout**: `~/.local/share/lerdr/current/lerdr` symlinked
   release tree; `install.sh` swaps `current` atomically. `plugin-build.sh`
   resolves `LERDR_RELEASE_ROOT`/`HERDR_RELEASE_ROOT` — keep both spellings.
2. **Subcommands**: the binary keeps `event-hook` (UDP forward used by the
   `[[events]]` hook), `setup-link`, `status`, service management, etc. —
   the pane scripts call them by argv.
3. **Action/pane scripts**: all `relay/plugin-*.sh` stay shell — Herdr runs
   argv directly; they invoke the binary. No Rust needed there.
4. **`event-hook` still exists** even though the relay can subscribe to
   `pane.agent_status_changed` over the socket — the hook fires in Herdr's
   process context and covers windows where the relay's own subscription
   was down (e.g. mid-restart). Defense in depth for the
   blocked/finished notification path.
5. **Journald/log capture**: `herdr plugin log list` shows plugin command
   output — hook scripts stay chatty; the binary keeps journald layers.

## Upgrades the plugin contract unlocks (new in this plan)

| Mechanism | Upgrade |
|---|---|
| `[[startup]]` hook | Relay re-asserts itself after **live handoff** (`server.live_handoff`) and session restore — today a handoff can leave the relay disconnected until the next external nudge |
| `[[link_handlers]]` | Regex URL patterns → actions: `https://github.com/.../pull/N` clicked in a pane can deep-link to the phone's git diff view — or "open this PR's workspace" |
| `plugin.pane.open` placements | `popup` with `width`/`height` for transient pickers (setup chooser becomes a modal popup instead of a zoomed takeover) |
| `HERDR_PLUGIN_CONTEXT_JSON` | Actions invoked with real context — a "Send this pane to my phone" action gets `pane_id`/`workspace_id` for free |
| `herdr api schema --json` | Relay dumps the installed API schema at startup → **exact** capability table (methods + events present) instead of probe-by-failure |

## Release packaging delta (Go → Rust)

Current: `package-release.sh` produces `lerdr_V_linux_{amd64,arm64}.tar.gz`
+ darwin + checksums + manifest verification.

Rust equivalent:
- `cargo build --release --target {x86_64,aarch64}-{unknown-linux-musl,apple-darwin}`
  — musl for static Linux binaries (drops the glibc version floor).
- Same tarball names/contents layout so `install.sh` and
  `check-installed-release.sh` keep working unchanged.
- CI signs the same way; `plugin-build.sh` never compiles on user hosts.
- Binary must stay **single-file static** — plugin build environments are
  minimal by design.

## Open questions for upstream tracking

- `client_shell.endpoint` capability + `command.invoke` (endpoint-issued
  command ids from the client-shell projection): Herdr has a designed
  remote-UI surface that already drives a "mobile Agents list". Whether
  the Kotlin app should consume client-shell projections directly (richer
  semantics than pane text) is a phase-2 investigation — could shortcut
  parts of the semantic feed.
- `agent.view.set` projections are transient and per-server — reapply from
  our `[[startup]]` hook.
