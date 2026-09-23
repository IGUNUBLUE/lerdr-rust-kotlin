# 10 — Spec gaps (strict self-review)

Honest accounting of what the plan does **not** yet specify. Ordered by
severity. Each item names the oracle in the Go repo to extract from —
Phase 0 should burn this list down before any production code.

**Status after the Phase-0 fixture+spec pass**: P0 items 1–4 now have
specs in `docs/specs/` AND executable vectors in `fixtures/` (delta codec,
sendbuffer, questions, lease). P0-5 resolved by decision (fresh store —
spec written). Corrections surfaced by the pass: `pane_delta` carries
`segments[]` (3-line anchors), not `ops[]`; sendbuffer overflow *rejects
the incoming push* — "eviction" is client disconnect, never dropped
queue entries; the replaceable set is exactly 8 types; `Interaction.Kind`
has only two values (`single_select`, `multi_select`) — approval/chat
live in the `Classify` layer; conversation pagination is tail-first;
Claude continuation IDs are inode-dependent.

## P0 — blocks implementation if missing

### 1. `pane_delta` segment codec — byte-exact
Doc 03 says `ops[]` but the real codec is a **3-line-anchor copy segment**
format (`internal/panedelta/delta.go`):
`Segment{copy_start, copy_lines, text}`; anchors keyed on 3 consecutive
lines (`minimumCopyLines=3`), ≤64 candidates per anchor
(`maxCandidates=64`), sender-side efficiency gate
`literalBytes + 64·len(segments) < ¾·len(current)` else full frame.
Kotlin `Apply` must reproduce bounds-check + `SplitAfter("\n")` semantics
exactly, including trailing-newline edges. **Gap**: op table + edge-case
fixtures (empty prev, all-rewrite, single-line pane, CRLF).

### 2. Send-buffer eviction contract — client-visible semantics
4MB-per-client ceiling + coalescing sets (pane_content/unchanged/resync
replaceable; pane_delta/acks never) exist in code, not in a spec the
Kotlin repo can test against. **Gap**: on-evict sequence diagram
(disconnect → reconnect → which resync flows), queue-priority ordering,
and the `>4MB message evicts client` cliff (reachable via 10k-line ANSI
panes — known audit finding, deferred wire change).

### 3. `Interaction` question taxonomy — full field matrix
`internal/question/parser.go`: `Interaction{id,kind,question,options[],
other{label,placeholder,allow_empty,hidden},submit_label,can_chat,
can_go_back,question_index,question_total}`. **Gap**: the `kind`
enumeration, per-kind answer action payloads (`answer_question`,
`clarify_question`, `navigate_question`), stale/dismiss lifecycle, and
how `Focus{kind,index}` maps to pane navigation keys.

### 4. Size-lease arbitration spec
`internal/panesize/manager.go`: `LeaseTTL=120s`, `ReleaseGrace=10s`,
leases keyed per clientID, `Rows=0` → width-only, effective size =
`minimumColumns` across live leases, local-terminal resize while leased
becomes new baseline. **Gap**: the multi-client arbitration table (phone
lease vs local terminal resize vs second controller) and expiry/freeze
semantics — the code comments encode hard-won mobile behavior (hidden-tab
clamping) that must not be lost.

### 5. Pairing + credential store format — ~~migration hinge~~ resolved
QR payload fields, invitation→credential exchange, device-file layout
(`HERDR_PLUGIN_CONFIG_DIR`), `credential_version` monotonicity.
**Decision (reimplementation framing)**: fresh Rust-native store — no
byte-compat with the Go layout. Re-pairing is one QR scan, the same UX
as day one; optionally import device names/roles for continuity. What
still needs speccing: the QR payload + handshake fields (inherited from
doc 03, unchanged) and the new store schema itself.

## P1 — phase-gated, needed before their phase ships

### 6. Conversation `Entry` schema per agent kind
7 agent readers emit `Entry{role,text,tools[]}` — the field matrix per
agent (claude/codex/gemini/…), cursor/paging semantics, live-tail vs
page-back contract. Doc 03 names the action, not the schema.

### 7. Upload/download spec
`internal/upload`: 256KB default chunks (bounds 1KB–1MB), `MaxFiles`,
expiry, path cleaning, resume semantics, base64-in-JSON overhead (Phase-5
fix already noted). **Gap**: request/response tables + error codes.

### 8. Push policy spec
VAPID lifecycle covered; the **policy** isn't — which events push,
debounce/coalescing rules, per-device policy model, quiet behavior when
app is foregrounded.

### 9. Semantic activity projection — the "what's it doing" model
The flagship UX claim needs a data model: which inputs fuse into feed
items (conversation Entries + `agent_status` + `detection` buffer +
`pane.updated`) with precedence rules when sources disagree. Currently a
UX intention, not a projection spec.

### 10. App offline/reconnect state machine
Draft persistence exists; **pending-action queue** doesn't (does a
prompt sent while reconnecting queue or fail?). States, retries, resync
UX, and what the UI shows at each rung.

### 11. Version negotiation policy
`protocol v3` field exists; the app↔relay min-version matrix, capability
downgrade table, and forced-upgrade UX are unspecified. Matters the
moment two binaries are live.

## P2 — before public release

- **Threat model**: pairing grants (role→action matrix), stolen-phone
  story (biometric gate vs stored creds at rest), log-hygiene rules for
  secrets in diagnostics, plugin code-execution trust statement.
- **Accessibility spec**: terminal surface semantics, live-regions for
  feed, screen-reader labels for agent status — M3E gives baseline, the
  terminal needs explicit work.
- **Localization**: `internal/localize` exists; app string catalog +
  language coverage decision.
- **CI matrix for this repo**: fixture validation, interop jobs,
  gradle/cargo caching, release lanes.
- **Observability parity**: diagnostics export contents, journald fields,
  what a support bundle contains.
- **License + app id + store listing** decisions.
- **`min_herdr_version` policy**: which features need which Herdr version
  (SchemaRegistry degradation table vs manifest floor).

## Non-gaps (already covered — don't re-litigate)

E2EE handshake byte-layout (doc 03), Herdr dispatch taxonomy + events_lost
recovery (doc 08), plugin distribution contract (doc 09), actor topology,
module layout, M3E isolation, phase gates.

## Phase-0 implementation findings (stations, 2025)

Recorded by the first Rust/Kotlin harness pass; none block Phase 1.

- **replay vs seq error split** — Go emits one string ("invalid encrypted
  frame sequence"); the fixture suite splits it (`received < expected` →
  replay, otherwise seq). Both implementations encode the split — the spec
  should bless it in the Phase-5 revision.
- **Server-side hello/finish parsing** — `parse_server_hello` /
  `parse_server_finish` validation rules were inferred symmetrically (the
  Go relay never parses them; the JS client does). Error variant names are
  implementation-chosen, not oracle strings.
- **Zeroization** — Go `clear()`s secrets; neither new implementation
  zeroizes yet. Add `zeroize` (Rust) + `Arrays.fill` discipline (Kotlin)
  in the credential-store phase — dep was not in the Phase-0 allowlist.
- **`Interaction.kind` fixture value `"choice"`** — one question vector
  carries a kind outside the spec'd `single_select`/`multi_select`.
  Kotlin keeps `kind` as raw `String` (lossless decode) with a typed
  accessor; classify whether `"choice"` is a legacy alias before Phase 5.
- **`apply` vs `applyStrict` divergence** — released JS client rejects
  `{}` segments that Go `Apply` accepts; Kotlin ships both applies and
  asserts the divergence. Boundary-table semantics (§6 of pane-delta
  spec) remain normative for clients.
- **`advance_seconds` fixture semantics** — maps to `advance` + one
  `sweep_expired` (equivalent to Go's 1s ticker only while no ops
  interleave mid-window); if vectors ever interleave, the sweep needs a
  per-second loop.
- **Relay-side `apply` policy** — if the Rust relay ever verifies deltas
  it must decide between `SplitAfter` line counts and boundary-table
  semantics (OPEN QUESTION-1 in the spec).

## Phase-1 round-3 findings (stations, 2025)

Relay server crate, app shell, terminal surface, and data layer landed;
none block Phase 1 continuation.

### Wire/semantic (protocol-parity relevant)

- **Fingerprint scope** — `content_fingerprint` binds content bytes only
  (`sha256(utf8(content))[0..8]` hex), not `lines`/`viewport_rows`/
  `format`. Same content under different budgets chains cleanly; render
  parameters are unpinned. Phase-5 candidate: extend scope or document.
- **Empty-string fingerprint suppresses watch_pane** — committed deltas
  store `content_fingerprint=""` (JS `typeof` check), which then blocks
  `watch_pane` re-issue until a real `pane_content` lands. Ported
  verbatim; consider a relay-side invariant.
- **4 MiB outbound cap vs pane_content** — a full frame exceeding the
  send-buffer byte cap evicts the client. Delta efficiency gating makes
  it rare but not impossible (large near-unchanged frames that fail
  `Efficient` go full).
- **Ack-gate oracle semantics pinned** — implicit delta acks while
  watching; `pane_content` acks iff `ack_required && fingerprint!=""`;
  `pane_unchanged` never acks (adopts + re-watches); resync → forced
  `read_pane` (never throttled); server gate = one unacked frame, 4s
  timeout; client read coalesce = 35s. `verifyContentHash` hardening
  beyond oracle is on-by-default in Kotlin, flag-off = released-client
  parity.
- **Go `Apply` vs client boundary-table divergence confirmed harmful** —
  Go's `Apply` rejects `copy_lines = count("\n")+1` yet the relay emits
  exactly that shape for metadata-only frames and JS accepts it. If the
  Rust relay ever verifies client acks with Go-style `Apply`, it will
  flag legal frames — use boundary-table semantics relay-side.
- **Inbox-overflow busy response can't echo ids** — request/action ids
  live inside the encrypted frame; Go's "Relay is busy" reply has the
  same constraint. Documented behavior, not a bug.

### App-side

- **`hilt-navigation-compose` absent** — Nav3 entries can't resolve
  `@HiltViewModel`; ViewModels use `viewModel{}` factory initializers
  (single swap point per screen). Revisit if the artifact returns.
- **`androidx.navigation3.runtime.deeplink` doesn't exist in 1.1.7** —
  local URI matcher parses `lerdr://pair?…` (cold + warm via
  `onNewIntent`→Channel). Re-check on Nav3 updates.
- **Setup-link divergences (deliberate)** — `lerdr://pair` requires
  `relay=` and allows `ws` (no page origin to inherit, no mixed-content
  rule); malformed `invite` params → hard reject instead of oracle's
  silent downgrade to bootstrap import. Pairing spec should bless or fix.
- **Bootstrap `setup` token must be exactly 32 UTF-8 bytes** at
  `toPendingInvitation()` (relay-side requirement); link parse stays
  oracle-loose (16–512 chars).
- **`AndroidKeystoreCipher` untestable on JVM** by design — fakes cover
  the store; needs an instrumented smoke test when emulator/Robolectric
  lands.
- **Draft debounce** intentionally left to the ViewModel
  (`snapshotFlow.debounce(300)`), unlike oracle's built-in flush.

### Relay-side

- **`with_session_config` rebuilds shared state** — startup-time only;
  runtime re-tuning would need `&mut self` before serving.
- **`HandshakeError::KeyMaterial` label stretch** — also covers
  `session.seal` on the finish frame; split variants if consumers care.
- **No fixture replay at handshake layer** — `KeySource` seam exists for
  pinning key+nonce; wire a `lerdr-fixture` golden `e2ee_server_hello`
  assertion in a follow-up.
- **Binary frames refused on `/ws`** (requireText parity) — `Codec` seam
  in place for the future DataChannel transport.

## Phase-1 round-6 findings (stations, 2025)

### Device-admin (relay)

- **Peer-session revocation is lazy, not prompt** — the oracle disconnects
  every session bound to a revoked credential; `lerdr-relay` disconnects
  only the session that performed `revoke_device`. Other sessions on the
  same credential are fenced at their next action (store re-read). Needs a
  credential→session index to match oracle promptness.
- **`reset_devices` during fixture replay kills the replay session** —
  sweep-style tests must run destructive admin fixtures on a dedicated
  connection last (the session-test pattern).
- **Admin responses are two frames** — `command_result` then
  `action_receipt` on success, `command_result` alone on failure. Generic
  "one response per request" harness assumptions break.

### Notifications/app

- **`POST_NOTIFICATIONS` is requested at the composition root** —
  first-run prompt on launch; revisit placement if UX review wants it
  tied to the first real relay instead.
- **Roborazzi JVM path defaults to `captureType=Dump`** (semantics
  overlay, ~1px text jitter). All screenshot tests must force
  `CaptureType.Screenshot()` + `@GraphicsMode(NATIVE)` or goldens flake.
- **Foreground suppression is absent by design (v1)** — attention
  notifications post even while the app is foregrounded.

### Action table (coordinator)

- **Ack ledger records but does not yet project** — `acknowledge_pane`
  binds `pane_id → state_change_seq`; the `attention_kind` projection
  that would consume it (agent list "needs you" dimming) doesn't exist.
- **Pane-size leases ride `stty` via process lookup** — `lease_pane_size`
  needs `PaneProcessInfo` from Herdr's `pane.inspect`; panes whose TTY
  can't be resolved fail `failed` like the oracle.

## Phase-1 round-7 findings (stations, 2025)

### Subsystem ports (coordinator)

- **Push identity is connection-bound** — `ActionContext.client_id` is a
  connection label, not an authenticated device identity; device-binding
  for `push_test_device`/`push_viewed_pane` uses the session's credential
  device as the stand-in. Thread the enrolled device id through the
  session handshake when multi-device-per-credential matters.
- **Web Push delivery is not implemented** — `push.rs` does policy,
  subscription validation, signed refs, snooze, and queue bookkeeping;
  actual webpush/vapid fan-out is absent (the app uses FCM/dataSync,
  making this dormant unless a web client appears).
- **Upload audit logging absent** — uploads record journal activity but
  the oracle's secret-aware write-audit line is not yet emitted; the
  central write-audit slice remains queued.
- **Speech voice updates are requester-only** — the oracle broadcasts
  voice-catalog changes to all sessions; ours replies to the requesting
  client only until a manager→broadcast seam is added (the journal
  forwarder pattern applies).
- **Questions replay idempotency relies on the store, not the ledger** —
  the ack ledger is not consulted for question re-answers; the store's
  own pending/fingerprint checks provide the oracle's semantics.

### Files mode (app)

- **`workspace_file` images decode from base64 in the ViewModel** — fine
  for icons/screenshots; large images will hit the pane-read cap first
  (bounded at the coordinator, oracle-faithful).
- **Git diff shown for the selected file only** — the oracle's
  `workspace_git_diff` is repo-scoped; we filter hunks by path client-
  side and degrade to "no diff" silently on parse gaps.

## Phase-1 round-8/9/10 findings (stations + orchestrator, 2025)

### Resolved this round

- **Push identity now keys on `identity.device_id`** — `ActionContext`
  carries both the transport `client_id` (connection bookkeeping) and the
  authenticated `device_id`; policy/subscriptions/viewed-pane/event-refs
  bind the authenticated device like the oracle. Wire `client_id` stays a
  subscription claim and the `push_unsubscribe` filter only.
- **Web Push delivery lands** — VAPID load-or-generate (fatal on corrupt
  key material, matching `push.NewManager`), RFC 8291 aes128gcm payloads
  (golden-vector pinned), RFC 8292 ES256 JWT, no-redirect 10s client, the
  oracle's due-order/retry/404-410-prune/recoverPruned semantics. The
  queue itself (`queue.json`) remains in-memory — the oracle persists it;
  deliveries in flight at restart are lost, subscriptions survive.
- **Voice-catalog + `update_status` fan out relay-wide** through the
  shared `Notices` broadcast (`spawn_notice_broadcast`), requester
  excluded when it already holds the frame.
- **Write-audit is live** — attempt rows at admission (post validation/
  auth/pane-target, pre-dispatch), result rows per emitted
  `command_result`, hub-admin results audited in the session layer,
  `send_secret` records only `text_bytes`, failures warn-and-continue.
- **Activity journal is durable** — JSONL under `runtime_dir/activity`
  with tombstones, compaction, permission repair, monotonic ids across
  reload, mutate-after-write ordering, live broadcast fanout.
- **Peer-session revocation is prompt** — the credential→session index +
  250ms deferred sweep disconnects revoked peers instead of waiting for
  their next action; version fences cover the registration race.
- **App: biometric lock gates `sessions.start()`** — unlock latch is
  injectable `LockState`; no relay traffic before verification.
- **App: uploads end-to-end** — SAF picker → `upload_begin/chunk/finish`
  staging → `Attachment: <ref>` draft lines → `submit_prompt`.
- **App: settings sections** — push policy (six wire categories, whole-
  map replace), device management (list/rename/revoke/invite+QR/reset),
  speech (enable/language prefs, voice catalog management, MediaPlayer
  WAV playback of `speak_text` chunks with prefetch + `cancel_speech`).
- **App: terminal find-in-buffer** — `terminal-find.ts` port: unicode
  case-fold literal search, 1000-match cap, cross-row fragments, wrap
  navigation, center-row reveal; highlight overlay is a separate pass
  that never touches the fingerprint/delta row reuse.
- **App: worktrees + workspace tabs** — sheet (list/create/open/remove
  with force escalation on `dirty_worktree_requires_force`) entered from
  the session bar; workspace tab strip with pointer-driven reorder
  (`insert_index` pre-move semantics) under the mode switch.
- **Conversation history lands** — `internal/conversation` port:
  provider roots (claude/codex/qoder/pi/omp/omo/opencode/hermes),
  bounded tail/JSONL/sqlite reads behind strict containment
  (canonicalized root prefix + `O_NOFOLLOW`), `ConversationBrowser`
  behind a thin action adapter with the oracle's
  `sameConversationTuple` post-read recheck. Fixture-verified: 9
  suites, 34 vectors, 52 steps. Deliberate deltas: raw entry-id
  cursors instead of signed `hb1.` envelopes, no prepare/snapshot
  jobs.
- **Shadow-diff harness lands** — `lerdr-shadow` scripted WS client +
  normalizer + differ, `lerdr-fake-herdr` fixture endpoint, and
  `tools/shadow/shadow_diff.py` driving Rust-vs-Rust self mode and
  Go-oracle-vs-Rust mode on one Herdr socket. Both gates report
  `IDENTICAL` on the `core` scenario.

### Still open

- **Push queue persistence** — `queue.json` in-memory only (see above).
- **Idle RSS baseline (release builds, same socket, 2026-09)** — Go
  `lerdr` 0.27.1 ≈ 22.3 MB vs Rust `lerdr-relay` ≈ 10.4 MB at idle
  (~2.1x lighter). Load comparison still owed by the shadow harness.
- **Plugin packaging exercised** — `herdr plugin link` registers the
  manifest (5 actions, 5 panes, build/startup/event hooks);
  `plugin-on-event.sh`/`plugin-on-startup.sh` → `lerdr-relay
  event-hook`/`startup-hook` → verified UDP datagrams. Still unproven:
  `plugin-build.sh` end-to-end (needs a published GitHub release —
  Phase-4 release pipeline).
- **App never sends a web-push subscription** — Android notifications
  ride the socket + local notifier; `push_subscribe` UI is intentionally
  absent. The relay path exists for future web/desktop clients.
- **`speak_text` on Android plays relay-synthesized WAV** — no on-device
  TTS fallback when the relay lacks `speech_synthesis` (capability-gated
  section hides; oracle parity).
- **Multi-relay settings are per-card** — sections render under each
  relay card; no global rollup (oracle parity: per-connection).

## Phase-1 round-11 findings (watch/target reconciliation, 2025)

The pane-watch + exact-target review rejection burned down; the shadow
harness now drives a dedicated `watch` scenario and both `core` and
`watch` report `IDENTICAL` against the Go oracle with the tightened
compare surface (`server_session_id`, `session`, `session_name`,
`format`, `truncated`, `viewport_*`, `resize_settling`, `interaction`,
`question_layout`, `target` all compared).

### Resolved this round

- **Exact-target admission ported** — `validate_exact_pane_target`
  (`actions/target.rs`) runs in session dispatch between authorization
  and the audit-attempt write, matching the oracle's order. Missing
  target / mismatched `target.pane_id` / stale tuple (`server_session_id`,
  `terminal_id`, `generation`, `agent_session_id`) reject with the
  oracle's `invalid_request` shapes; `unwatch_pane`/`release_pane_size`/
  `cancel_speech` are exempt like the oracle.
- **`agents` frames carry the full target tuple** — `server_session_id:
  "primary"`, `generation`, `terminal_id`, `agent_session_id`
  (trimmed `agent_session.value`) are projected, sorted by `pane_id`.
  Without them Kotlin's `wireTarget()` refused to build targets and the
  app could not send any pane-directed frame against the Rust relay.
- **Per-pane generation tracking** — `Topology` keeps generation across
  accepted snapshots; `agent_stop`/`agent_clear` bump it on non-replayed
  effects (scheduler `slot.generation` is a separate counter, as in the
  oracle).
- **Watch loop parity** — interval ticker (100/250/500/1000 whitelist,
  250 default) + invalidation fast path, so an update arriving inside
  the ack gate is picked up by the next tick instead of being lost
  forever (the review's stale-terminal defect). `watch_pane` re-issue
  replaces the watch. `pane_applied` matches the wire fingerprint
  against pending/acknowledged and foreign fingerprints push
  `pane_resync`; the 4 s ack deadline clears pending+acknowledged and
  forces a fresh full frame. `pane_delta` never carries `ack_required`.
  The ctl channel is bounded (`try_send`; acks coalesce).
- **Two-tier poll** — each tick runs the oracle's `HandleProbePane`
  cheap `visible`-source probe (500 lines) and full-reads only when the
  probe fingerprint moved or the committed frame was `resize_settling`.
- **`readPaneForDisplay` source/format matrix** — `format:"ansi"` is
  honored on `read_pane`/`watch_pane`; non-ansi reads stay on `visible`
  so text reads never trigger Herdr's mouse-scroll harvest on the
  operator's pane; ansi reads use `recent` (`recent-unwrapped` for
  Claude when not lease-resized). Frames emit `format` on both
  `pane_content` and `pane_delta`.
- **`pane_content`/`pane_delta` metadata** — frames now carry
  `truncated`, `viewport_only` (always), `viewport_rows` (lease-only),
  `resize_settling` (3 s window — corrected from 4 s),
  `interaction: null`, `question_layout: false`, and the `target` echo.
  The unchanged-skip hashes a frame fingerprint over content+metadata
  so metadata-only flips emit the copy-everything delta.
- **`read_pane` response shape** — failures push `pane_content{content:"",
  format, error, target}` (no receipt); empty-pane and fingerprint-hit
  paths match the oracle; `capPaneContentLines` tail-caps content before
  fingerprinting.
- **`agent_state` projection** — `session` is `agent_session.value` (the
  oracle's raw `SessionRaw.Value`), `session_name` is `""` — Rust has no
  title resolver (resolved in round 15: `conversation/resolver.rs` ports
  `internal/session/resolver.go`; Kotlin merges `session_name` verbatim).
- **`pane_unchanged` always echoes `target`** (`null` when absent), and
  `HandleReadPane`'s `handleAcknowledge` half is ported — every direct
  read and every watch frame read records `pane_id → state_change_seq`
  in the ack ledger.

### Still open (declared deltas)

- **Classification projection** — resolved in round 14 (S1):
  `classify/` ports the question/attention projection; shadow compares
  the full semantic key set for real.
- **Mid-read `ContentRevision` fence** — resolved in round 15: the
  counter is coordinator-side `content_rev` (not Herdr's — earlier note
  misattributed it); all three read paths (probe, watch frame, direct
  `read_pane`) fence both generation and revision.
- **Read single-flight** — the oracle dedupes concurrent `read_pane`
  calls per pane in `d.reads`; Rust always reads. Internal RPC economy,
  not a wire difference.
- **Title resolver** — resolved in round 15 (see `agent_state`
  projection note above).
- **Go-only agent projection keys** — `activity_seq`, `pane_revision`,
  `project`, `raw_pane_id`, `tab_label`/`tab_number`/`tab_order`,
  `cwd`, `tokens`, `state_labels`, `last_active_at` remain declared
  deltas in `type_drop_keys`. `updated_at`/`last_seen_at` are now
  emitted from per-pane `AgentTimes` observation bookkeeping
  (`updated_at` bumps when Herdr's `state_change_seq`/`revision`
  advances; `last_seen_at` refreshes every apply) — memory-only,
  unlike the oracle's restart-persistent triage records.
- **`acknowledged.classificationAgent` probe leg** — the third
  `paneWatchNeedsFrameRead` trigger has no counterpart until the
  classification projection exists.

## Round 11 — live device test (emulator ↔ Rust relay ↔ real Herdr)

First on-device run surfaced three runtime-only defects no automated
gate caught; all fixed and re-verified live (pairing → connect →
agents/workspaces inventory → `watch_pane` terminal stream → ANSI
render → pane-size lease → interactive key bar):

- **`usesCleartextTraffic=false` blocked every `ws://` relay** —
  direct-LAN relays are the oracle's normal path and the payload is
  `herdr-e2ee-v2` sealed either way; the flag is now `true` with the
  rationale recorded in the manifest.
- **`RelaySyncService` FGS-deadline crash** — pairing flaps
  `CONNECTED→CLOSED→CONNECTED` inside milliseconds (the setup socket
  hands off to the enrolled credential); `stopService` landing before
  `onCreate`'s `startForeground` makes Android kill the process with
  `ForegroundServiceDidNotStartInTimeException`. Fix: stops are now
  delivered as a queued `ACTION_STOP` command (ordered after
  `startForeground` by construction) with `stopSelf(startId)` scoping,
  the notifier debounces zero-connection stops (3 s) and only stops a
  service it armed, and the in-service safety net uses the same settle
  window.
- **`herdr_status.features: null` broke the whole inventory** —
  Kotlin types `features` as a non-null map (the oracle always
  allocates it); the Rust relay emitted `null`, so `push_config`/
  `herdr_status` failed decode → `UnknownServerMessage`, inventory
  stayed `starting`, and `acceptsInventorySnapshots` dropped every
  `agents`/`workspaces` frame. The relay now emits `features: {}`
  (no probe-ledger subsystem yet — the oracle's map is evidence
  gathered by active probing).
- **Agent observation times** — `updated_at`/`last_seen_at` were 0,
  rendering "497253h ago" ages on device. `AgentTimes` now stamps them
  from snapshot-apply observation (change-keyed on `state_change_seq`/
  `revision`, bumped by `bump_generation`); `last_active_at` remains a
  declared gap (the oracle derives it from the activity journal).

## Round 12 — phone-terminal interaction layer + mobile polish (2025)

Second live pass on emulator + real device (Moto G85, `adb reverse`
USB tunnel — LAN 8377 is firewalled; `ws://127.0.0.1` over USB works
unchanged because the E2EE handshake is host-agnostic). Everything
below is verified on-device; multi-finger pinch verified by code path
only (no `adb input` equivalent).

### Landed this round

- **Paired-device rows** (`DevicesSection`) — metadata `FlowRow` +
  oracle's ≤36rem breakpoint (`WIDE_DEVICE_ROW_MIN`): actions drop
  below the row as equal-width buttons. Fixed the timestamp
  char-per-line collapse.
- **Single special-keys bar** — merged the duplicate row; keys send the
  oracle's exact wire spellings (`Left`/`Escape`/`Ctrl+C` — `send_keys`
  passes names through and the relay normalizes; the old `ArrowLeft`
  style would have been rejected by Herdr).
- **Latching Ctrl + combos sheet** — Ctrl arms as a modifier
  (highlighted), next typed letter emits `Ctrl+X` without touching the
  draft; long-press Ctrl opens the `C-c C-d C-z C-l C-r` sheet.
- **"show keyboard" pill** — now focuses the inject field + opens IME;
  tap on the terminal surface does the same (spec §Terminal).
- **"scroll to live" pill** — appears when follow-live releases on
  scroll-up; tap snaps to the live edge and re-arms follow.
- **Long-press context menu** — row hit-test by Y (blank rows omit
  "Copy line"); copy line / copy transcript / share transcript; per-URL
  open-link / copy-link from linkified `href` spans.
- **Pinch-to-zoom** — `fontScale` 0.6–2.5× on the surface state; cell
  metrics re-probe → `lease_pane_size` re-leases automatically.
- **Real RTT** — `refresh_agents` keepalive round-trip measures `rttMs`
  (`-1` = unmeasured, reset on disconnect); relay chips render
  `sd · direct · 9ms`, settings detail `… · protocol 3 · 9ms`.
- **`RelayConnection.terminate()` ordering** — `disconnect.complete`
  resumed collectors before `_state = Closed` was written; reordered
  behind a CAS guard (fixed the `serverCloseEndsIncoming…` flake).
- **Launcher icon** — iguana-head medallion (project-provided artwork)
  as the color foreground over the artwork's own jungle-leaf field as
  the background layer; the whole badge sits inside the adaptive safe
  zone so the medallion edge never clips. Monochrome layer is a lizard
  silhouette glyph. `LockGate` badge uses the same mark. The "show
  keyboard" pill was dropped — tapping the terminal surface already
  opens the IME.

### Candidate features — not yet spec'd

Adopted from mobile-terminal UX patterns; each needs a spec entry
before implementation:

- **Image/file attach + annotate in the composer** — attach sheet,
  thumbnail preview, attach→paste path or caption into the prompt.
  Needs a wire shape (relay has no file-ingress message today) —
  largest spec gap in this list.
- **Customizable shortcut panel** — user-defined key strips above the
  bar (persisted per relay? per pane class?). Key bar is already
  data-driven; this is a settings + persistence item.
- **OSC-52 remote clipboard** — pane OSC52 sequences → Android
  clipboard, with a confirmation affordance. Parser hook exists
  (`AnsiSpans`); needs the clipboard seam + privacy copy in settings.
- **Hardware-keyboard map** — Ctrl/Alt/Esc chords on physical
  keyboards; map through the same `send_keys` vocabulary.
- **Swipe-to-switch-pane / recent directories** — horizontal swipe on
  the terminal or a jump-list in the composer for recent cwds.
- **Relay version string** — settings shows `relay 0.0.0`; the Rust
  relay doesn't emit a real version yet.
- **True multi-touch pinch test** — needs a Compose/Robolectric or
  instrumentation test; `adb input` can't synthesize two pointers.

## Round 13 — Computers tab + provider avatars (2026)

- **Computers tab**: the relays carousel at the bottom of Home became a
  real bottom-nav destination (`LerdrKey.Computers`, `ComputersScreen`) —
  Agents · Computers · Activity · Settings. One row per connected relay
  (label, transport, live status/RTT, agent count); the single "+"
  FAB pairs a new device. Fine management stays in Settings → Devices;
  the redundant pair affordances on Home (FAB + trailing add-card) were
  removed since both did the same thing.
- **Provider-logo avatars**: `AgentListItemUi`/`AttentionCardUi` and the
  three session UiStates (`Feed`/`Terminal`/`Files`) gained a `provider`
  field (normalized wire `agent`); the shared `ProviderBadge` renders:
  the official mark for CLIs with a public vector asset
  (claude/claudecode, codex/openaicodex, gemini/geminicli, opencode,
  copilot/githubcopilot, cursor via simple-icons CC0; devin via the
  devin.ai mark); a `π` glyph for pi/picodingagent/
  omp/ohmypi; and a deterministic provider-hued monogram tile for known
  CLIs without a public mark (qoder, omo/ohmyopencode, hermes,
  and any future wire identity). Plain shells/absent metadata keep the
  neutral letter monogram. The badge appears in Home rows, attention
  cards, and the shared `SessionTopBar` title slot.

## Round 14 — Wave-0 lifecycle/safety fixes (2026)

Critical correctness/security pass ahead of the feature wave:

- **Credential storage** — `AppModule` now uses `KeystoreCredentialStore.create`,
  which lands the sealed credential blob under `noBackupFilesDir`
  (`pairing/credentials.dat`) instead of backup-eligible `filesDir`. Auto
  Backup must not restore device-bound identity material onto another
  device. (Pre-existing installs re-pair once.)
- **`paneSnapshot` observability** — was a one-shot cold flow that
  resolved the pane runtime once: a collector racing `openPane` saw a
  single `null` forever, and reconnect-side runtime replacement went
  unseen. Now a `paneGeneration` counter (bumped on every `panes`
  insert/remove) drives `flatMapLatest` re-resolution.
- **Pane-size lease lifecycle** — `TerminalViewModel` renews the lease on
  the oracle's 10 s cadence (`PANE_SIZE_LEASE_REFRESH_MS`), gates on the
  oracle's 5 min hidden grace (`paneLeaseRenewalAllowed` /
  `PANE_LEASE_HIDDEN_GRACE_MS`), re-leases instantly on the
  resume edge (`sessions.hidden` collector), and releases before unwatch
  in `onCleared`. The renewal loop rides `appScope` — a repeating `delay`
  on `viewModelScope`/Dispatchers.Main spins `runTest`'s scheduler
  forever.
- **Hidden watch parity** — `SessionRepository.setHidden` now unwatches
  open panes on background and re-arms read+watch on resume
  (`visibilitychange` parity); `resyncPanes` skips watch traffic while
  hidden so a reconnect doesn't leak watches. The `openPanes` intent set
  is untouched.
- **Bounded inbound queues** — `RelayConnection.frames` and
  `RelaySession.incomingChannel` moved off `Channel.UNLIMITED` to
  `ReconnectPolicy.FRAME_BUFFER_CAPACITY`/`INCOMING_BUFFER_CAPACITY`
  (256). Overflow aborts the socket: pane/command frames can't be
  skipped mid-stream, so redial + resync replays a consistent snapshot
  instead of growing heap without bound.
- **Feed auto-scroll** — was keyed on entry count and always yanked to
  the bottom. Now follows the tail only while the user is pinned to the
  bottom (derived `totalItemsCount` check), keyed on the last entry's
  id + text length so streaming growth still follows and "Load older"
  prepends keep the anchor via stable keys.
- **`LerdrApp` scope** — uses the injected `@AppScope` CoroutineScope
  instead of a private unowned one.
- **`@Immutable`** on `FeedUiState`/`TerminalUiState`/`FilesUiState`/
  `FilesBreadcrumb`.
- **Repository wrappers** for the previously uncalled catalog actions
  (oracle payloads mirrored): `send_secret` (cap `secret_input`),
  `copy_agent_response` (15 s), `tab_reorder` (cap `tab_reorder`),
  `agent_start` (45 s), `agent_rename`/`restart`/`stop`,
  `agent_clear` (45 s), `workspace_create` (45 s) /`rename`/`close`
  (30 s, `close_group` + `expected_workspace_ids` supported) /
  `reorder` (block form when `workspace_reorder_block`, legacy
  `insert_index` otherwise), `list_directories` (10 s) with a parsed
  `DirectoryListing` model. `deviceRole`/`canControl` expose the
  enrolled credential role for the oracle's `readOnlyRelayIds` UI gate
  (fail-closed: READER unless proven CONTROLLER).
- **Danger tokens** — `extendedColors.danger/onDanger/dangerContainer/
  onDangerContainer` (muted maroon, not saturated `errorContainer`) for
  deny/stop/destructive actions.

## Round 15 — Wave-1 feature wave (2026)

Five parallel streams closed the audit's UI-reach gaps; all mutations gate
on `canControl` (the oracle's `readOnlyRelayIds` behavior).

- **Home triage** — needs-you cards answer inline via `respond` /
  `answer_question` (the rail no longer navigates away to triage);
  expanding "New" FAB opens agent-launch (`agent_start`) and
  workspace-create sheets (with a `list_directories` browser and
  plain-cwd fallback when `directory_browser` is absent); swipe
  end-to-start on an agent row requests `agent_stop`; latency bands and
  a real empty state on Agents; Computers rows carry transport/RTT.
- **Feed** — `session/feed/`: markdown rendering (headings, lists, code
  fences with copy), per-tool icon tiles + expandable payloads (error
  results no longer show a checkmark), full question forms
  (multi-select, Other text, navigate/clarify), in-feed find with
  n-of-m navigation, slash-command menu fed by `list_slash_commands`,
  snackbar error surface, color-coded triage (Allow green / Always
  slate / Deny maroon) with the warning header, periwinkle user
  bubbles. `copy_agent_response` reachable from Manage.
  Fixed a real production bug: `FeedViewModel`'s slash catalog never
  fired — cache fields were declared after `init`, so the combine
  collector read a null backing field during construction and died on
  an NPE (SupervisorJob swallowed it silently).
- **Terminal** — `send_secret` input path (password transform,
  non-saveable draft, never routed to `send_text`), `no_echo` prompt
  banner with capability gate + unsupported-relay guidance, reader
  lock-down (chips/Ctrl/IME disabled, hint chip), amber lease chip,
  pane meta row (leased cols×rows, truncation, no-echo marker), wired
  find bar, 48dp targets, Ctrl latch semantics.
- **Session chrome** — `session/manage/` sheet (rename/restart/clear/
  stop/copy-response, confirm-replaces-list, reader = metadata only),
  inline title editor → `agent_rename`, statusVariant morphing chip
  (lease→amber, attention→pulsing cookie, error→danger corner),
  workspace tab long-press menu (`tab_reorder`, `workspace_rename`,
  `workspace_close` with `action_id`-correlated group escalation +
  live drift cancel), workspace create sheet, copiable cwd chip in
  Files.
- **Cross-cutting** — `enableOnBackInvokedCallback` + `lerdr://agent(s)`
  deep links; shared-axis-X pushes with directional session-mode slide
  and fade-through tab switches (MDC recipe, emphasized easing);
  `LerdrStatus`/`LerdrStatusDot`/`LerdrStatusChip` morphing status
  (attention pulses); segmented pill selected = primary/onPrimary;
  live-region helpers on activity rows / lock gate / settings errors;
  row-owned switch semantics; notification channel descriptions +
  CATEGORY_STATUS; @Immutable audit on nav keys and VM aggregates.

Still open (next wave): voice input, attachment ingress UX, update
check, diagnostics screen, OSC-52 clipboard, hardware-keyboard map,
swipe-to-switch-pane, workspace-row reorder surface.

## Round 16 — Wave-2 oracle-parity seams: viewed pane, acknowledge, self-update (2026)

Audit found three oracle behaviors the app never sent; all are
repository seams + lifecycle wiring rather than protocol changes
(protocol v3 already covers every frame).

- **`push_viewed_pane`** (`SessionRepository.setViewedPane` +
  `setLocked`, fed by `TerminalViewModel` init/cleared and
  `LerdrApp`'s `lockState.locked` collector). Matches the oracle's
  App-level `$effect` exactly: signature =
  `relay:pane:terminal:agent_session:generation`, non-empty only while
  visible + unlocked + `server_session_id == "primary"` + relay
  `connected`; a change pushes `visible:false, unlocked:!locked` to the
  previous relay then `visible:true, unlocked:true, target` to the new
  one. Reactive like the oracle — agent regeneration, reconnect, lock,
  and hide re-derive via `agents`/`connections` collectors under
  `start()`. Tab switches to Feed/Files clear it (the oracle gates on
  `view === 'terminal'`).
- **`acknowledge_pane` on open** — `FeedViewModel.init` calls
  `acknowledgePane` for non-reader devices (the oracle's `openAgent`
  gate); `AgentStore.acknowledgeDone` adds the oracle's optimistic
  `done`→`idle` flip before the command lands.
- **Relay self-update** — `checkUpdate`/`installUpdate` on
  `SessionRepository`: `self_update`-gated, 30 s timeout, `data.update`
  folds into the connection row on success **and** refusal —
  `CommandException` grew a `data` payload for that. `installUpdate`
  mirrors `installRelayUpdate`: reads the expected version/revision
  from `connection.update` (`available && can_install &&
  target_revision` else `CommandException(reason)`), remembers a
  pending install, and `reconcilePendingUpdates` declares completion
  when a reconnect reports `releaseVersion` + `-dirty`-stripped
  `revision` matching the target (the restart dropped the
  `command_result`). The oracle's auto-check effect is ported:
  `check_update` fires once per `relay:version:revision:appVersion`
  identity on connect (needs `buildConfig = true` for VERSION_NAME).
  UI lands on the Devices card: `updateStatus`'s full state vocabulary
  (checking/available/blocked/scheduled/preparing/installing/
  restarting/succeeded/rolled_back/failed + up-to-date fallback),
  warning/danger tints, Check + controller-gated Update actions,
  `shortRevision` port, live-region announcements.
- **Tests** — `SessionRepositoryTest` +11: exact target frame, dedup,
  clear-on-leave, lock clear/republish, non-primary gate, hide-clear,
  regeneration repush, capability refusal, `check_update` payload fold,
  install expected-fields, refusal payload application. DevicesSection
  goldens +3 (available/failed/manual-bootstrap).

Deferred: workspace-row reorder (oracle `WorkspaceManager`), feed
diagnostics surface, OSC-52 clipboard, hardware-keyboard map, voice
input, attachment ingress, swipe-to-switch-pane. (`deploy_app_update`
was listed here at the time; later confirmed removed upstream — see
round 14's tail section.)

## Round 17 — Wave-3 remaining parity surfaces (2026)

Bounded every "still open" item against the oracle; the ones it
actually implements landed here.

- **Feed history diagnostics** — `FeedHistoryWarnings.kt` +
  `FeedViewModel` demand loop port the `ConversationHistory.svelte`
  warning block the feed dropped: `continuation_incomplete` with
  reason-specific copy + Reload, `oversized_records` /
  `omitted_tools`/`omitted_payloads` / `corrupt_records`/`plan_corrupt`
  rows, the preparing-page poll (1 s cadence, progress-key reset,
  30 identical snapshots → retryable `preparation_stalled` +
  Continue), Cancel/Continue/Reload, error codes → Continue vs Retry,
  `source_changed` reload. Older pages prepend-merge deduped by id.
- **Pull-to-refresh** — `PullToRefreshBox` on the Agents list armed
  only at scroll top (oracle `AgentList` touch tracking), LongPress
  haptic, 900 ms re-arm window, driving the new
  `SessionRepository.inventoryRefresh` (`refresh_agents` to connected
  relays + re-dial `disconnected` endpoints — the oracle's
  `requestInventoryRefresh`).
- **Workspace-row reorder** — the app has no `WorkspaceManager`
  surface, so move up/down actions landed in the tab strip's overflow
  menu. `workspaceTrees()` ports `relayWorkspaceTrees` (linked
  worktrees nest under the `repo_key` primary); the block-form
  payload moves a whole linked group, legacy `insert_index` fallback
  kept, optimistic `pendingWorkspaceOrder` invalidated on snapshot
  confirm or membership drift.
- **Dropped as non-oracle**: voice input (no SpeechRecognition/mic
  invoke in the oracle — `speech/` is relay→phone playback), OSC-52
  clipboard (xterm.js never processes it), hardware-keyboard map
  (oracle only wires Ctrl/Cmd+F → find), swipe-to-switch-pane (the
  only list gesture is pull-refresh). These may return as
  product-level enhancements but are not parity debt.
- Housekeeping: `HomeScreenScreenshotTest` stray NUL bytes →
  `\u0000` escapes (production group keys use NUL separators).

Genuinely remaining at the time: `deploy_app_update` — wire-catalog
command with no frontend consumer even in the oracle. Since confirmed
removed upstream (Tailscale-only transport; see the round-14 tail
section): the name stays reserved in the v3 catalog and
`app_deploy_status` remains a parseable frame with no emitter, matching
post-removal oracle behavior. Plus product-level items the oracle never
shipped (voice, OSC-52, hardware keys, pane-swipe).

## Round 14 — relay wave: semantic layer, Herdr boundary, durable push (2026)

Three stations, orchestrator-integrated. Post-merge: `cargo test
--workspace` all green (coord 314), fmt/clippy clean, shadow
`core`/`watch`/`semantic` × self/go = 6/6 IDENTICAL.

### Resolved this round

- **Classification projection (the round-12 "still open" headliner)** —
  `lerdr-coord/src/classify/` ports `internal/question`: attention
  kinds, matchers, no-echo prompts, parse, projector, store. Shadow
  `semantic.json` compares `attention_kind`/`prompt`/`command`/
  `options`/`approval_fingerprint`/`interaction`/`interaction_id`/
  `question_layout` on `agents`+`blocked`+`pane_content` for real
  (event_id/pane_revision/transition_at stay per-commit volatile).
  `history.rs` ports the read-merge the classifier consumes. The ack
  ledger has its consumer; drop-keys for the now-real fields are gone
  from all scenarios.
- **Event-vs-poll commit split** — `CommitKind::{Event,Poll}` mirrors
  `commitTopologyLocked`'s preserve-committed-status rule on the event
  path (the emit-blocked finding): pane/tab/workspace events never let
  a sampled status overwrite committed blocked details; polls do.
  Verified against the oracle by the semantic scenario.
- **`inventory_status` full projection** — six keys emit for real
  (`state`/`error_code`/`message`/`stale` + both timestamps);
  `mark_inventory_failure` ports `MarkInventoryFailure`. Removed the
  blanket `inventory_status` drop; only wall-clock timestamps are
  key-dropped.
- **Runtime `SchemaRegistry` + capability ledger** — `lerdr-herdr`
  gains `capabilities.rs`/`schema.rs`/`cli.rs`/`view.rs`:
  `herdr api schema --json` introspection, epoch-tagged probe notes
  (untracked probes adjudicated by the refresh, not the previous
  server identity), `RunCapabilityRefresh(30s)` equivalent, and the
  full `herdrStatusPayload` projected field-for-field into
  `herdr_status` (`Topology::herdr_status` + `set_herdr_status`
  whole-struct dedup).
- **Canonical `agent.view.set`** — installed post-bootstrap and
  re-asserted on `[[startup]]` hook / live handoff with bounded
  retries; `KnownUnsupported` is a quiet skip. Wire shape pinned by
  test.
- **Durable push queue** — `actions/push_queue.rs` persists
  `queue.json` (0600, atomic temp+rename, indent+newline, 1024
  entries/4 MiB caps). Recovered entries gate behind
  `reconcile_recovered_push` (server.go:1467-1516 port) — first
  authoritative inventory after restart opens delivery; finished keys
  survive only while their completion is current. **Deliberate delta:**
  a corrupt queue file is salvaged member-wise and quarantined as
  `queue.invalid-<nanos>.json` instead of failing manager
  construction — the repo's existing durable-file convention; Go
  hard-fails.
- **Release version** — `lerdr_core::release_version()` owns the
  `LERDR_VERSION` → `CARGO_PKG_VERSION` → `0.0.0-dev` chain; every
  surface shares it, including `lerdr-relay`'s fallback snapshot (moved
  to `-core` to break the dependency direction).

### Deliberate semantic decisions

- **`health_check` is the server-advertised value**, not derived from
  event-stream staleness (matches the oracle's `*bool` — omitted until
  evidence exists). Transport staleness stays on `Topology.stale`; a
  reconnect with no capability change republishes no `herdr_status`.

### Still open (unchanged deltas + deferred waves)

- `agents[*].{pane_revision,tokens,state_labels}`, workspaces'
  `{cwd,tokens,worktree}` — commit-epoch counter + Go-only fields;
  declared deltas, drop-keys remain.
- `action_receipt`/`push_config` — Rust-only v3 dispatch evidence and
  implementation-scoped payloads (census-visible, not compared).
- Startup-burst frame order — unordered pool comparison; the oracle's
  fixed order is `push_config,agents,workspaces,activity_history,
  inventory_status` vs Rust's `push_config,herdr_status,workspaces,
  agents`.
- Mid-read `ContentRevision` fence — resolved (round 15 / S6); the
  counter is coordinator-side, portable, now enforced on all read paths.
- **Removed upstream, not ported**: `webrtc_*`/`herdr-dc-v1`,
  `lerdr-gateway`, `deploy_app_update`, portmap/UPnP — the oracle's
  CHANGELOG made Tailscale the only transport and deleted these
  binaries/actions. Wire names remain reserved in the v3 catalog for
  compatibility; `app_deploy_status` stays a parseable frame with no
  emitter (same as post-removal oracle behavior for the native app).
- **In flight (wave 2)**: release pipeline / CI matrix; `session_name`
  title resolver; `ContentRevision` mid-read fence; `[[link_handlers]]`
  manifest section; `internal/localize` residual audit.

## Round 15 — wave 2: relay leaves, release pipeline, upstream-removal audit (2026)

Three stations + orchestrator. The wave's headline finding was a
scoping correction: the oracle deleted its entire non-Tailscale
transport surface (`lerdr-gateway`, WebRTC gateways, `herdr-dc-v1`,
portmap/UPnP, `deploy_app_update`/`deploying_app`, `stable-state`) —
"Tailscale is now the only transport" per its CHANGELOG. Those items
are recorded as **removed upstream, not ported** (roadmap annotated);
the v3 wire names stay reserved and `app_deploy_status` remains a
parseable frame with no emitter, matching post-removal oracle behavior.

### Resolved this round

- **`session_name` title resolver** (`conversation/resolver.rs`) —
  full `internal/session/resolver.go` port: normalized
  `(agent, cwd, foreground_cwd, session_id)` cache key, 60 s TTL
  re-validated against the freshly-resolved `Location`, all provider
  grammars (OMP `title`/`title_change`/`session.title`; Pi
  `session_info.name`; Hermes `location.title`; Claude/Qoder
  `custom-title`>`ai-title`>`summary`; Codex `session_index.jsonl`
  first-`id` `thread_name`), scanner caps mirrored. Wired via
  `ResolverSlot` into both commit kinds; committed title lives on the
  shared `AttentionCell` so published clones project it. **Declared
  delta:** the oracle's title cache is unbounded; the port caps at
  2048 entries (sweep-then-clear) per the bounded-state rule.
- **`ContentRevision` mid-read fence** — the counter is the
  coordinator's own `content_rev` (the earlier "fake Herdr" note
  misattributed it). All three read paths now fence generation +
  revision: watch probe, watch frame read, direct `read_pane`
  (`mid_read_fence` extracted for ordering tests).
- **`[[link_handlers]]`** — manifest section + `plugin-open-link`
  scripts: GitHub issue/PR links in panes open a QR overlay on the
  phone. Spec-only (the oracle ships none) but verified against
  upstream herdr 0.9.1's real manifest schema/env names.
- **localize residual** — audited: no gap. Wire errors stay
  `{code,args}`; `NormalizeLocale` + push localization already ported.
- **Release pipeline** — `.github/workflows/{relay,app,interop,
  release}.yml` + `scripts/{check-version-sync.sh,release-manifest.py}`
  + `plugin/scripts/{package-release,check-installed-release}.sh` +
  `docs/release.md`. Tag-gated 4-target builds (musl linux ×2, darwin
  ×2), per-target native smoke, APK (signed iff keystore secrets
  configured), version-sync gate, republish guard. Live gates
  (HERDR_LIVE/LERDR_*/shadow-go) are `workflow_dispatch`-only.
- **MSRV floor** — raised to 1.88: `icu_*` (via `url→idna`) already
  required it; the CI `msrv` job is a real gate now, not advisory.

### Still open

- **Release-management subcommands** — resolved (S8, `release.rs`):
  all five subcommands (`release-manifest`, `verify-release`,
  `activate-release`, `seal-release`, `prune-releases`) ported with
  oracle CLI shapes and sync dispatch; `support-state.json` emits
  `release_directory`; `version --json` matches `{version, revision,
  target}`. **Declared divergence:** Go's `release.Verify` requires a
  non-empty `web_hash` (web-bundle builds); the Rust verifier enforces
  `web_hash` only when `web/` entries exist — matching
  `scripts/release-manifest.py`, since Rust tarballs ship no web
  bundle. Cross-verified: Python manifests verify under Rust and vice
  versa.
- **Read single-flight** — unchanged declared delta (internal RPC
  economy, not a wire difference).
- **Go-only projection keys** — `pane_revision`/`tokens`/`state_labels`/
  workspace `{cwd,tokens,worktree}` — declared deltas; the app already
  parses `pane_revision` 0-normalized, so emitting it later is free.
- **Startup-burst frame order** — unordered pool; both relays race.
- **Live smoke** — partially validated (Moto G85 paired to the Rust
  relay over Tailscale, live frames render); watch/lease/question
  flows on device still to exercise.
- **Phase 5** — untouched by design (binary inner codec, zstd,
  conversation subscriptions, binary chunks, capability-negotiation
  revision).
