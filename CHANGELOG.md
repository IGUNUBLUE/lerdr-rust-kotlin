# Changelog

All notable changes to this project will be documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

The `protocol v3` / `herdr-e2ee-v2` wire contract remains frozen.

## [Unreleased]

## [1.0.0] — 2026-10-04

### Known limits

- Release authorization is separate from full app audit acceptance. Remaining
  checks require genuine capability-omitting peers, a provider supporting
  native question clarification, and complete audio-observed TalkBack coverage.
- Workspace rename, camera attachment and diagnostics UI entry points are
  not exposed; directory fallback and bounded Activity checks retain their
  documented capability boundaries.
- Official signed app upgrade and isolated managed relay upgrade/rollback
  have separate passing evidence. Current app update trust/staging and the
  native managed relay updater still need verification against newer matching
  published artifacts; the obsolete published installer hook is fixed here.

### Fixed

- **Audit evidence uses synthetic identifiers** — replace operator-specific
  paths and run markers in regression inputs, screenshots and documentation.
  Raw device evidence stays private; public release evidence is redacted.

- **Visible Terminal output is accessible to TalkBack** — expose the drawn
  viewport without composing every row or reading hidden scrollback.
- **Large-text notification settings remain readable** — move the system
  settings action below the status instead of squeezing the heading.
  Unpaired Settings guidance now points to Computers.
- **Large-text theme choices retain complete labels** — use measured label
  widths to switch narrow Settings controls to full-width radio rows,
  preserving exclusive selection and 48dp targets.
- **Relay update details keep their width** — put Update and Check below the
  status instead of squeezing version, revision and failure text between them.
- **File-preview Back has a full 48dp container** — its native clickable
  rectangle no longer relies on fractional expansion of a smaller icon button.
- **Large-text session modes stay complete** — when measured labels cannot
  fit the segmented row, use a single-height mode menu with explicit current
  selection. Feed, Terminal and Files remain complete; ordinary segments stay.

- **Rust release installation accepts its verified manifest** — stop requiring
  a nonexistent frontend web hash in the plugin installer. Installation and
  rollback still check the exact version and revision reported by the relay;
  the binary verifier retains manifest file-hash validation.

- **Long Feed responses stay where the reader scrolled** — an offscreen end
  inside the final turn no longer counts as near-bottom. Passive updates do not
  yank scrollback to the tail; ordinary pinned live following is preserved.

- **Older Feed pages retain the visible expanded turn** — anchor to the first
  visible entry when the exhausted pagination header disappears, including its
  pixel offset. Removed transcript rows no longer leave animation drawings over
  historical turns or the fixed mode controls; the viewport is explicitly clipped.

- **Cold restored Terminals resume live content** — re-arm the pending initial
  read after authoritative agent inventory arrives, without a mode toggle,
  manual refresh, or re-pairing. Rendered, closed and hidden panes stay untouched.

- **Pane membership preserves queued status invalidations** — forward the
  remaining bootstrap gap and already queued live events before replacing
  per-pane Herdr subscriptions. Continuous traffic cannot starve the refresh.

- **Viewed sessions do not interrupt with duplicate alerts** — Feed now
  publishes its visible pane, and local notifications retract or suppress that
  pane while it is visible and unlocked. Other-agent alerts remain active;
  background and locked-app deliveries are preserved.

- **New push distributors work on return** — discover a freshly installed
  UnifiedPush distributor when Lerdr returns to the foreground, without
  requiring an app restart or replacing an already active registration.

- **Approval choices retain their policy and permission scope** — show complete
  multiline labels in Home and Feed instead of clipping them to indistinguishable
  prefixes. Preserve Codex's wrapped scope text without weakening approval
  recognition or stale-menu guards.

- **Terminal draft restoration keeps the caret** — save plain input as full
  text-and-selection state instead of restoring a string at the start.
  Secret input remains memory-only and is never saved for restoration.

- **Invitation clipboard previews hide the enrollment secret** — mark copied
  one-use links as sensitive so Android masks its clipboard preview. Native
  paste and the explicit pairing preview remain available.

- **Large-text navigation stays on one line** — ellipsize destination labels
  instead of splitting words and enlarging the whole bottom bar at 200% text.
  Icons retain the full accessible destination names; text scaling is unchanged.

- **Rightward tab reordering moves the tab** — send Herdr's insertion boundary
  past the next tab, accounting for removal of the source. The previous index
  was a confirmed no-op that left the app waiting for an order change.

- **Session management remains usable after renaming** — consume handled
  dismissal results before closing the sheet, so reopening does not immediately
  close again. Rename text, selection and IME composition update synchronously;
  delayed inventory changes cannot corrupt an edited name.

- **Warm re-pairing waits for the new enrollment** — replace the previous
  transport before awaiting the invitation handshake, instead of reporting
  failure from the old connection's already-connected state.

- **Feed typing preserves uploaded references and the caret** — update text,
  selection and IME composition synchronously instead of feeding editor text
  back through asynchronous screen-state flows. Draft persistence, replacement
  targets and edits made during submission retain their existing guards.

- **Generic Feed attention is not a question** — keep approval and question
  headings tied to their actual kinds; unknown blockers show "Attention needed"
  and retain the terminal inspection path.

- **Unknown agent timestamps do not invent ages** — omit elapsed time when
  the relay has not observed activity yet. Known activity and update times
  retain their existing precedence and age formatting.

- **Background terminals return their viewport to the desktop** — release
  the size lease when the app hides instead of renewing it for five minutes.
  Hidden grid callbacks cannot re-acquire it; resume re-watches and restores
  the latest measured grid immediately.

- **Wrapped native Codex questions stay inline** — accept navigation and cancel
  hints wrapped below the live submit footer instead of dropping the form.
  Later transcript output still prevents historical questions from reopening.

- **Native OMP ASCII questions stay inline** — recognize `+- Ask` frames and
  their ASCII borders instead of falling back to terminal-only interaction.
  Completed Ask output without a live footer remains history, not a new question.

- **Feed search names its conversation scope** — show "Find in conversation"
  in the Feed field instead of the terminal-specific label. Terminal search
  keeps "Find in terminal".

- **Restart and clear follow their replacement pane** — keep the management
  sheet open while the old agent disappears, then update Feed, Terminal and
  Files routes to the returned pane without leaving closed-pane Back targets.
  Both actions use the relay's 45-second replacement window; Restart no
  longer times out at the generic 15-second deadline.

- **Uploads survive pane session replacement** — validate the current
  pane generation and normalized session ID from the agent inventory
  instead of requiring generation zero; stale targets remain rejected.
- **Terminal headers retain agent lifecycle** — pane dimensions no longer
  replace `working`, `blocked`, or `idle` with a lease label. Geometry
  remains in the terminal metadata row; the header keeps working and
  waiting accents independently of the viewport.
- **Omp lifecycle setup covers named profiles** — document installation
  into the active profile and reloading already-open sessions. The
  worktree-based orchestration fallback cannot detect subagents inside
  the same omp process; Herdr's omp integration supplies that lifecycle.
- **Short agent status transitions no longer drop** — the relay
  subscribes to `pane.agent_status_changed` for each concrete pane and rebuilds
  coverage as panes enter, leave or move. Live events commit their carried
  status, so a `working`→`idle` burst inside one poll window produces its
  transition and done/unseen projection. Resync snapshots adopt current state;
  buffered status events trigger reads, not stale replay. Optional-subscription
  fallback handles Herdr decoder refusals with either empty or echoed request
  IDs. The plugin's `agent_event` datagram still commits its payload on older
  Herdr versions that refuse lifecycle subscriptions.
- **Warm deep links do not duplicate destinations** — repeated settings or
  agents intents keep one destination; a new pairing link refreshes the
  existing form without silently connecting.
- **App update permission handoff waits for its result** — the Allow action
  opens the app-specific unknown-source settings page instead of clearing
  its gate during composition. Returning denied does not start a download.
- **Downloaded updates are privately staged and verified** — reject absent
  archives, a different package, non-newer version codes, and incompatible
  signing identities before sharing read-only bytes with the installer.
  Background completion opens update review, never an unsolicited installer.
- **Feed drafts retain the current local edit** — restore saved text once per
  identity, ignore delayed reads after typing, and keep queued writes alive
  when the screen closes instead of replaying older persisted prefixes.
- **Raw command replies stay scoped to their relay and session** — foreign
  results cannot complete another relay's request; disconnect ends pending
  work with an unknown dispatch outcome rather than waiting for a timeout.
- **Native OMP questions recognize glyph key hints** — detect the current
  `⏎ select` / `⏎ submit` dialog footer instead of losing structured choices;
  completed Ask output without a live footer remains ordinary history.
- **OMP multi-select follows native key controls** — reconcile checked
  options with Space, confirm with Enter, and finish custom answers after
  the native text editor returns. Legacy menu controls stay unchanged.
- **Live JSONL tool results replace earlier call rows** — a late output or
  error updates an existing tool card even when its raw-record id is unchanged.
- **Reader Home omits creation actions** — agent/workspace launch menus
  require a paired controller; reader-only and unknown-role inventories
  no longer offer an unusable launch button.
- **Reader speech catalogs remain read-only** — keep voice status and
  encrypted playback available, but omit install/remove controls and
  reject stale voice-management actions locally without sending them.
- **Sent drafts cannot be restored by older queued writes** — serialize
  application-lifetime persistence and clear only the submitted snapshot;
  edits made during submission remain unsent drafts.
- **Reopened sessions survive stale screen cleanup** — local view ownership
  keeps an old Feed or Terminal from removing its successor's subscription,
  watch, size lease, or viewed signal. New connections inherit hidden state.
- **Cancelled app downloads release update state** — a removed download row
  fails visibly and clears pending bookkeeping instead of polling forever
  or resuming the dead download after process restart.
- **Directory browsers refresh when reopened** — newly created project
  folders become selectable without reconnecting or restarting the app.
- **Raw agent launch waits for native registration** — an accepted start
  command can precede Herdr's agent record. Wait within the existing bound
  without launching twice, and preserve the already-dispatched boundary if
  later detection or naming fails instead of reporting a safe-to-retry refusal.
- **Custom raw launches retain their requested identity** — name the admitted
  native agent after `pane.report_agent`, within the existing response budget,
  so the app header and name-based lifecycle target the same agent. A later
  naming refusal cannot turn the already-launched command into a safe retry.
- **Early connections receive the launch catalog** — resolve bounded native
  and local profile discovery before accepting clients. A first-handshake
  catalog can no longer remain empty while slower inventory/capability probes
  finish later; frozen v3 messages and fixtures are unchanged.
- **Chosen launch names survive directory changes** — retain explicit name
  edits when directory results arrive or the suggested profile name changes,
  including an edit equal to the current suggestion. Fresh forms and relay
  selection still reset the name.
- **Phone terminal leases no longer crop native output** — size the read-only
  observer from Herdr's native grid rather than the smaller process TTY lease.
  Live tail rows and hidden-password prompts remain visible after the phone
  keyboard shrinks its viewport; wire fields and size-lease behavior are unchanged.
- **Terminal searches retain zero coordinates** — serialize required copy-engine
  row/column fields and range endpoints even at the origin. Full-scrollback
  searches and first-row/column requests no longer fail because a valid point
  became an empty or incomplete object.

## [0.2.7] — 2026-10-01

### Added

- **Fit width for wide terminals** — the session ⋯ menu gains a
  "Fit width" action that scales the widest committed row edge-to-edge,
  so a native-size omp TUI (≥160 columns) is fully visible in one
  viewport instead of three horizontal swipes away. Once fitted the
  entry flips to "Actual size" for the way back. Pinch and fit share
  the same bounds, now 0.25–2.5× so wide TUIs are actually reachable.

### Fixed

- **Pane tail missing on oversized panes** — the observe stream
  rendered onto a fixed 120×40 surface when no geometry was supplied,
  so panes taller than 40 rows lost their bottom rows entirely: omp's
  fixed-position chrome (TODO panel, activity line, statusline, box
  border) never entered the emulator that backs watch/read content.
  The relay now resolves each pane's real geometry — lease dims while
  a size lease is held, else the committed layout rect — and respawns
  the observer when it drifts.

## [0.2.6] — 2026-10-01

### Fixed

- **omp panes keep their native TTY size** — the phone's pane-size lease
  no longer runs `stty` on omp sessions, so the full-width TUI (status
  strip, spinner, task and subagent indicators) renders intact on both
  the desktop and the handset. The terminal view scrolls horizontally
  and pinch-zooms to inspect the native-width content; a provider that
  resolves to omp after the viewport was measured drops any lease it
  took, and resolving to a different provider acquires one.
- **Orchestrated cohorts are visible again** — hook-less orchestrator
  panes (omp launched outside `agent start`) were reported `idle` by
  Herdr's `default_known_agent_idle_fallback` even while their TUI
  visibly coordinated work. The app now derives `orchestrating` from
  the wire's own worktree data: a hook-less pane in a repo-root
  workspace whose linked-worktree siblings hold busy agents shows
  `orchestrating · N` in the working section and `orchestrating` on
  the terminal chip and feed header. Display-only — no pane state is
  claimed and no radar-owned fields are read; real lifecycle for these
  panes remains an upstream (omp self-reporting) concern.

## [0.2.5] — 2026-09-29

### Added

- **`agent_blocked` rejections carry a public message** — refusing a
  `submit_prompt` while a question or approval owns the pane now maps
  to "Agent is waiting at a question or approval" in `refusal_message`,
  so clients render the real cause instead of the generic fallback.
- **Custom-argv agents claim their pane after detection expiry** —
  when an argv-profile start outlives the detection deadline, the relay
  now claims the pane via `pane.report_agent` (`resume_argv` validated
  against Herdr's rules) instead of leaving it dispatched-unknown.

### Fixed

- **Composer accepted sends while a question owned the pane** — Herdr
  rejects `submit_prompt` with `agent_blocked` while a question or
  approval dialog is pending, and the app surfaced the relay's generic
  fallback ("Herdr rejected the command before it was sent"). Send is
  now disabled while the agent is `blocked` — the same rule the
  terminal's `inputLocked` already applied — with a placeholder that
  points to the terminal for the pending interaction. A defensive
  `sendPrompt` guard preserves the draft on races, and
  `command_result` now carries `data.code` into `CommandException` so
  late rejections map to a real message.
- **Terminal rendered mid-repaint frames after a resize** — the
  phone's terminal lease resizes the real agent TTY; during the
  repaint window the pane grid mixes stale cells from the previous
  geometry ("letters over letters"). `resize_settling` frames now hold
  the last settled frame for display while delta continuity and acks
  continue underneath — per spec, those frames are not committed to
  history.
- **Shadow diff compared run-scoped `health_check` output** — the
  `herdr_status` comparison dropped the always-differing
  `health_check` field from shadow-diff evaluation.


## [0.2.4] — 2026-09-29

### Added

- **Stop session + Close workspace in the session overflow** — both
  destructive actions now sit in the session ⋮ menu under a danger
  divider: *Stop session* runs `agent_stop` (pane close) after
  confirmation, and *Close workspace* opens a sheet that mirrors the
  relay's workspace semantics — single close for lone workspaces,
  group-only close for primaries with linked worktrees, and both paths
  for linked worktrees (close this one, or the whole group via the
  primary). Stale group snapshots escalate into a re-confirmation with
  the relay's authoritative `workspace_ids`; a workspace closed by
  another client dismisses the sheet.

### Fixed

- **Feed rows overlapped during streaming** — `animateItem`'s spring
  placement animation fought the tail-pin's instant `scrollToItem`
  re-asserts: while an agent streamed, placement springs restarted
  faster than they converged and items painted at stale animated
  offsets. Placement now applies atomically (insertion fade kept).
- **Forget left a live credential on the relay** — `removeRelay`
  dropped the registry row and local credential but never told the
  relay, so a forgotten pairing could still authenticate until another
  controller revoked it by hand. `removeRelay` now sends a best-effort
  `revoke_device` with our caller `device_id` while the socket is up
  (5 s bound, failure still unpairs locally).

## [0.2.3] — 2026-09-28

### Fixed

- **`push_subscribe` wedged after a failure during reconnect churn** —
  a refused or lost subscribe marked the endpoint as sent, and only a
  disconnect edge cleared the mark; a request dying on a socket
  mid-reconnect left every later CONNECTED emission seeing a live relay
  with the mark still set, so the subscription never retried until
  process restart (observed live: the phone stayed "relay refused" with
  the keep-alive FGS pinned after a relay restart). Failed sends now
  re-arm on an exponential backoff (5 s doubling, 5 min ceiling);
  success, endpoint rotation, unregistration and unsubscribe paths
  cancel pending retries.
- **Orphaned push state fenced re-paired devices** — an endpoint row
  whose owning credential died outside the wire path (hand-edited
  tombstone, crash between revoke commit and prune hook) kept the
  endpoint bound to a dead device id, and UnifiedPush endpoints are
  per app+distributor so the re-paired device inherited the fenced
  endpoint and ate `push_subscription_device_mismatch`. The relay now
  reconciles device-keyed push state against live credentials at boot
  and rebinds dead-owned endpoints on subscribe; a live owner still
  fences.
- **Hand-edited revoked tombstones crash-looped the relay** — a revoked
  record retaining a secret hard-failed store validation; since the
  tombstone is already dead the correct shape is unambiguous, so load
  now scrubs the secret, warns, and persists instead of refusing to
  boot. Other violations stay fatal.

## [0.2.2] — 2026-09-28

### Fixed

- **Pairing retry wedged after a rejection** — an `AuthRejected` (or
  `Closed`) session stayed in the session map with its dial loop already
  exited, so every later Connect replayed the stale verdict instantly
  (zero wire traffic) until the app was force-stopped. `connect()` now
  recreates a terminal session only while a pending `RelayInvitation`
  exists — re-pairing writes one, a revoked credential keeps its record
  and must not redial forever — and the session factory reads auth
  records straight from the store instead of a lagging mirror. Verified
  on-device: consumed-token rejection, then a re-armed invite redeems on
  plain retry with no restart.

## [0.2.1] — 2026-09-28

### Fixed

- **QR pairing crashed on release builds** — R8 stripped the ML Kit
  `*Registrar` constructors that `ComponentDiscovery` instantiates
  reflectively, so `BarcodeScanning.getClient()` produced a scanner with
  a null delegate and the first analysed frame NPE'd. Keep rules now
  cover `com.google.mlkit.**` + `com.google.firebase.components.**`.

## [0.2.0] — 2026-09-28

### Added

- **Real push delivery via UnifiedPush** — the app registers with any
  installed UnifiedPush distributor (tested with ntfy), sends the
  endpoint + Web Push keys to each relay via `push_subscribe`, and the
  relay delivers RFC 8291-encrypted pushes signed with its VAPID key.
  Pushes render through the same notification ids as socket events (no
  duplicates), deep-link into the app (`lerdr://settings`,
  `lerdr://agent`), and revive a dead process — delivery is verified to
  survive `force-stop`. Settings shows the real subscription state per
  stage and a "Send test" action.

### Changed

- **Keep-alive service is now the fallback, not the primary** — the
  `dataSync` foreground service only pins while push cannot cover
  dead-process delivery (no distributor, registration incomplete, or no
  relay subscription acked). Once a relay acks the current endpoint the
  pin is released, ending the persistent-foreground battery warning and
  the Android 15+ `dataSync` quota exposure. Pin activation/release is
  debounced (1.5s/3s) and a system-restarted service self-stops when
  push already covers. Settings reports the keep-alive state honestly.
- **Relay: push endpoint validation is structural** — any `https` host
  on port 443/default is accepted (no userinfo, no fragment) instead of
  a five-host allowlist, matching the UnifiedPush model where the user
  picks the distributor.

## [0.0.5] — 2026-09-24

### Added

- **Terminal text selection** — long-press-drag marks a cell range and
  the release menu offers "Copy selection" / "Share selection", so part
  of an agent's output can be copied into another session's input. A
  held press without travel still opens the row menu; a tap dismisses
  the selection. Wide graphemes and multi-codepoint clusters select
  atomically. Dragging raises the platform magnifier above the finger,
  and committed selections grow draggable teardrop handles that re-range
  the endpoints — a tap inside the selection reopens the copy menu.
  While a selection is marked the grid freezes its rendered rows
  (copy-mode semantics), so live pane output can no longer slide the
  text out from under the highlight in a truncated scrollback.

### Changed

- **Terminal is edge-to-edge** — the pane card and the cell grid no
  longer pay double horizontal margins, so the negotiated lease tracks
  the real screen width (~8 more columns on a phone).

## [0.0.4] — 2026-09-24

### Added

- **`pane_links` in the terminal** — the long-press menu now hit-tests
  the cell server-side (`pane_link_resolve`) and gains an "Open link on
  desktop" item when herdr reports link regions. This reaches OSC8
  escape-sequence links whose URL never appears in the served text;
  `pane_link_activate` opens on the pane host's browser and reports the
  target, with a local-open fallback when upstream resolves but cannot
  handle. Negotiated via the `pane_links` capability — older relays keep
  the client-side linkified items only.
- **`pane_search` scrollback count** — the terminal find bar annotates
  the local "n of m" with the server's full-scrollback hit count
  ("· N in scrollback") when `pane_search` is negotiated and hits live
  beyond the rendered buffer. Debounced per query; local matching stays
  the source of truth for highlights and navigation.

## [0.0.3] — 2026-09-24

### Added

- **Terminal send + watch cadence** — the terminal input bar's Send
  action now rides `send_input` (text + Enter as one action) instead of
  literal `send_text`; the special-keys bar still exposes literal input.
  `watch_pane` requests `interval_ms=100` (the lowest whitelisted
  cadence) now that `pane_realtime_delta` is advertised and watches arm
  automatically.
- **report_metadata badges** — `agents[]` rows decode herdr
  `pane.report_metadata` projections (`tokens`, `state_labels`) and
  `workspaces[]` rows decode `tokens`. Home renders a watching eye on
  panes carrying `lerdr_watching`, a device-count chip on workspace
  groups carrying `lerdr_devices`, and state-label chips on agent rows.
  Snapshot semantics: absent keys clear (herdr TTL expiry), deltas keep.
- **Capability contract completed** (`docs/03` §4.1 is canonical) — the
  relay now advertises `pane_realtime_delta` (gated on `pane.read`),
  `tab_reorder`, `workspace_reorder_block`, `typed_push`,
  `push_policy`, and `device_management`, so app-side feature gates arm
  on connect.
- **Speech capabilities wired** — `speech_synthesis` /
  `speech_voice_management` advertise from live local speech facts
  (flite/espeak/say system fallbacks or the managed Piper engine), flip
  mid-session via `caps_update`, and populate
  `push_config.speech_languages`.

### Changed

- **inner_codec_binary dropped** (`docs/13` §2.1) — the deferred CBOR
  inner-codec path is removed outright; `frame_zstd` captured the real
  win. `preferred_inner_codec` is gone from `client_caps` (relays
  ignore it as an unknown field either way).

### Removed

- Dead capability constants (`herdr-hybrid-v2` — hybrid transport is
  out of scope per docs/12; a duplicated `agent_response_copy`).

## [0.0.2] — 2026-09-24

### Added

- **Phase-5 Track A** (`docs/13-phase5-wire-spec.md`) — capability
  negotiation + pane-content actions on the frozen v3 envelope:
  - App announces `client_caps` as the first post-handshake frame and
    applies the relay's symmetric `caps_update`; the negotiated live set
    is `server-advertised ∩ app-announced`.
  - New actions: `focus_pane`/`focus_tab`/`focus_workspace`/`focus_agent`,
    `pane_search` (match-position metadata), `pane_selection_read`,
    `pane_link_resolve` (cell regions) / `pane_link_activate`
    (`handled` + `url`), `layout_export`/`layout_apply`.
  - `TargetRef` gains `workspace_id`/`tab_id`; `Inbound` gains the
    structured Phase-5 raw fields (`query`, `cursor` objects,
    `anchor`, `previous`, `row`/`col`, `root`, `tab_label`, `focus`).
- **Phase-5 Track B** (`docs/13-phase5-wire-spec.md` §2) — negotiated
  transport upgrades, all gated on the live capability set:
  - `frame_zstd` — `pane_content` frames may carry
    `encoding:"zstd"` + base64 zstd `payload`; the envelope stays
    plaintext and the inflated `{content}` restores before the
    terminal surface consumes it. Malformed payloads drop safely.
  - `convo_sub` — `subscribe_conversation`/`unsubscribe_conversation`
    per pane; `conversation_update` pushes a `reset` snapshot then
    append-only tails, deduplicated by entry id and dropped when the
    pane `generation` is stale. The Feed subscribes while the screen
    is open; history polling remains the fallback path.
  - `upload_binary` — chunks ride the `0x03` carrier
    `[0x03][upload_id:32][chunk_seq:BE64][bytes]` inside the sealed
    channel when `upload_begin_result.chunk_encoding == "binary"`.
    Acks stay JSON (`upload_chunk_result`, empty `request_id`,
    correlated by send order); JSON and binary carriers share one
    sequence domain per upload.

## [0.0.1] — 2026-09-23

First public release. Working end-to-end, still in development.

### Added

- **Rust relay** — axum WebSocket server, per-pane watchers, bounded send
  buffers, Herdr Unix-socket client, health endpoints (`/health`,
  `/healthz`, `/readyz`), update worker, `speech-voices` binary surface,
  release packaging (`lerdr-relay_<v>_<os>_<arch>.tar.gz`).
- **Android app** (Kotlin / Jetpack Compose, Material 3 Expressive):
  - Pairing via QR / `lerdr://pair` deep link, biometric lock.
  - Mission-control home: agents grouped by workspace, working /
    attention / idle states, needs-you rail with one-tap approvals.
  - Session: Feed (conversation, question and approval cards, composer,
    history), Terminal (full ANSI pane, key bar, find, leases), Files
    (workspace tree, preview, git status/diff).
  - Notifications with deep links, Settings + relay detail, computers,
    activity journal, worktrees, session rename/reorder.
- **Protocol** — `protocol v3` / `herdr-e2ee-v2`: P-256 handshake, HKDF,
  AES-GCM sealed frames; anchored by frozen golden vectors (`fixtures/`).
- **Tailscale transport** — `tailscale serve` integration scripts in
  `plugin/scripts/`; relay binds loopback, tailscaled owns the tailnet
  listener.
- **Determinism harness** — `tools/shadow/` runs two fresh relays against
  one fake Herdr and diffs normalized outbound streams.
- **Herdr plugin packaging** — manifest + install/verify scripts under
  `plugin/`.

### Notes

- The Android APK ships **unsigned** until keystore secrets are
  configured — sideload at your own discretion.
- This is an AI-generated codebase (built with Cognition's SWE-2);
  see `README.md` and `CONTRIBUTING.md` for the disclosure policy.
