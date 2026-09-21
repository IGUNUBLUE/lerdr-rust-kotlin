# 04 — App design: a new experience, not a port

Design intent: **sit at the computer from the phone**. Two renderers over
one stream — a semantic feed that answers *what is the agent doing / what
does it need*, and a full-fidelity terminal that answers *let me drive*.
Neither is a fallback for the other; both are first-class.

## Navigation model

```
┌────────────────────────────────────────────┐
│  Home (mission control)                    │
│  ├─ Needs-you rail  (attention inbox)      │
│  ├─ Agents          (grouped by workspace) │
│  └─ Relays strip    (computers carousel)   │
├────────────────────────────────────────────┤
│  Agent session                             │
│  ├─ Feed     (semantic timeline — default) │
│  ├─ Terminal (full interactive)            │
│  └─ Details  (workspace, git, files, info) │
├────────────────────────────────────────────┤
│  Activity (cross-agent journal)            │
├────────────────────────────────────────────┤
│  Settings (relays, devices, push, voices)  │
└────────────────────────────────────────────┘
```

Bottom nav (M3E `ShortNavigationBar`): **Agents · Activity · Settings**.
Everything else stacks on top with predictive back.

## Screen: Home

- **Needs-you rail** (top, only when non-empty): horizontally scrolling
  attention cards — approval/question/chat kinds get distinct colors and
  iconography; card shows agent, workspace, the question preview, and an
  inline `ButtonGroup` with the top two choices. Answer without leaving
  home. This is the PagerDuty muscle: urgency you can triage.
- **Agent list**: grouped by `relay ▸ workspace`, pinned working agents
  with `WavyProgressIndicator` (the M3E signature), idle agents with quiet
  status chips. Swipe actions: right = open, left = stop (destructive, with
  confirmation haptic + undo snackbar).
- **Relays carousel**: each computer a card — connectivity state, agent
  count, transport in use (`tailscale`/`gateway`/`direct`), subtle
  morphing-shape loader while connecting.
- **FAB menu** (`FloatingActionButtonMenu`): New agent · New workspace ·
  Pair device (QR) — replaces stacked FABs per M3E guidance.
- Empty state: expressive illustration + "pair your first computer" CTA
  → QR scanner.

## Screen: Agent session — the core redesign

Top bar: agent name (editable), workspace breadcrumb, status chip,
connection dot. **Segmented control** in the app bar switches modes:
`Feed | Terminal | Files`.

### Feed mode (default) — *what the agent is doing*

Data: `get_conversation_history` pages + `question`/attention events +
`activity` — NOT raw ANSI.

- **Timeline** of entries: user prompts (outgoing bubble, surface-variant),
  assistant text (markdown-rendered, code blocks with copy), tool calls as
  compact cards — tool icon, title (`Edit src/foo.ts`), expandable to
  input/output/diff. `Error` tools tinted error.
- **Working indicator**: when pane activity advances but no entry commits,
  show a live "thinking" row — morphing `LoadingIndicator` + latest tool
  name + elapsed.
- **The blocker card**: when `question`/attention arrives, a large
  expressive card pins above the composer — full `QuestionInteraction`
  form: single/multi-select as `ButtonGroup`s / choice chips, other-text
  field, back/next navigation, submit. Dismissible only by acting.
  This replaces "read the CLI prompt" entirely.
- **Jump-to-live**: `Terminal` segment or a "live output" affordance on
  the working indicator opens terminal mode.

### Terminal mode — *the machine itself*

- `LazyColumn` of ANSI-parsed `AnnotatedString` rows (monospace, bundled
  font), keyed virtualization, delta-applied — 60 fps scrolling.
- **Special-keys bar** (collapsible): `Esc Tab ↑ ↓ ← → | Ctrl` — Ctrl is a
  latching modifier (tap then letter = `C-x`). Long-press Ctrl opens the
  combos sheet (`C-c C-d C-z C-l C-r`).
- **IME behavior**: tap screen → keyboard up, typing sends `send_text`;
  suggestion bar hidden (terminal context); Enter = `send_keys [Enter]`.
- **Size lease**: measure grid → `lease_pane_size`; on keyboard-open,
  `adjustResize` shrinks the grid and re-leases rows — pane reflows like a
  real terminal resize. Release on background/hide.
- Pinch-to-zoom adjusts font → re-leases columns.
- Scrollback stays readable while live: pause-follow button ("scroll to
  live" pill, Telegram-style) when scrolled up; deltas still apply.
- Long-press a row → copy line / copy screen / share transcript.

### Details mode

- Workspace info, cwd chip (copiable), git status card (branch, ahead/
  behind, dirty files → tap for diff viewer), file tree (lazy), agent
  metadata (session id, started, kind), actions: rename / restart /
  clear / stop — the `ManageDialog` surface, redesigned as a sheet.

## Composer (shared across modes)

Autosize field, per-agent drafts, attach button (camera/gallery/files →
chunked `upload_*` with progress cards), slash-command autocomplete
(bottom-sheet picker with search — `list_slash_commands`), voice input
(STT → text). Send = `submit_prompt` for feed, `send_text`+Enter for
terminal. Reader role hides mutating affordances entirely — not disabled,
gone.

## Notifications → deep links

- Channels per urgency: `agent_attention` (high), `agent_activity` (low),
  `service` (foreground, low).
- Tapping a notification resolves `push_open_ref` → navigate straight into
  the agent's blocking question — one tap from shade to answer.
- Swipe-to-snooze maps to `push_snooze`; "viewed" suppression via
  `push_viewed_pane` when the pane is foreground.

## Settings

- **Relays**: card per computer — URL/transport, latency, version,
  reconnect; add via QR or paste link; per-relay push policy editor.
- **Devices**: this device + paired list (rename/revoke), create
  invitation QR for a new phone (controller or reader).
- **Speech**: voice catalog, install/remove, language chips.
- **App**: biometric lock toggle, theme (dynamic color on/off),
  update check, diagnostics export.

## Motion & theming (M3 Expressive)

- `MaterialExpressiveTheme` + `motionScheme()` — spring physics defaults.
- Dynamic color (wallpaper) default-on; brand fallback for sideloaded
  contexts without wallpaper colors.
- Morphing shapes on status indicators (working = wavy, waiting = pulsing
  "cookie" shape, error = sharp).
- Predictive back; shared-axis transitions list→detail; attention cards
  animate in with `animateItem` + spring.

## Feature parity checklist (non-negotiable)

Every catalog action must have UI reach: agents CRUD/reorder, workspaces +
tabs + worktrees CRUD, send text/keys/secret/prompt/respond, questions
(answer/navigate/clarify), uploads batch, conversation history + search,
workspace tree/file/git, activity journal + copy response, push subscribe/
policy/snooze/test/viewed, device list/rename/revoke/invite/reset,
speech voices + speak/cancel, update check/install, app-deploy,
slash commands, QR pairing, pane lease, webrtc signaling (phase 2).

## Accessibility & quality bars

- All interactive elements ≥48 dp, content descriptions on icon-only
  controls, live regions on attention cards.
- Terminal: minimum 10sp effective font; zoom ceiling per lease caps.
- Reader mode is fully usable — not a disabled afterthought.
- Battery: single multiplexed WS per relay, lifecycle-aware watch
  (unwatch on background, re-watch + re-lease on resume), WorkManager for
  nothing — the socket *is* the realtime channel.
