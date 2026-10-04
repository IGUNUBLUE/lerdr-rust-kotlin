# 10 — Spec gaps (strict self-review)

> **Alignment note (2026-09):** the project is self-contained — `docs/` +
> `fixtures/` are the authority. Mentions of "the predecessor" / "the Go
> implementation" in this file are **historical provenance** (where each
> behavior was extracted from), not a standing comparison rule. New gaps
> are resolved against the spec and vectors, not an external codebase.

Honest accounting of what the plan does **not** yet specify. Ordered by
severity. Historical items name the original implementation file each
behavior was extracted from.

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
  implementation-chosen, not predecessor strings.
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

### Wire/semantic

- **Fingerprint scope** — `content_fingerprint` binds content bytes only
  (`sha256(utf8(content))[0..8]` hex), not `lines`/`viewport_rows`/
  `format`. Same content under different budgets chains cleanly; render
  parameters are unpinned. Phase-5 candidate: extend scope or document.
- **Empty-string fingerprint suppresses `watch_pane`** — committed deltas
  store `content_fingerprint=""`, which blocks a `watch_pane` re-issue until
  a real `pane_content` arrives. The relay should maintain this invariant.
- **4 MiB outbound cap vs pane_content** — a full frame exceeding the
  send-buffer byte cap evicts the client. Delta efficiency gating makes
  it rare but not impossible (large near-unchanged frames that fail
  `Efficient` go full).
- **Ack-gate semantics** — implicit delta acks apply while watching;
  `pane_content` acks iff `ack_required && fingerprint!=""`;
  `pane_unchanged` never acks (adopts + re-watches); resync forces an
  unthrottled `read_pane`; the server allows one unacked frame with a 4 s
  timeout; client read coalescing is 35 s. Kotlin enables
  `verifyContentHash` by default.
- **Boundary-table rule** — `copy_lines = count("\n")+1` is legal for
  metadata-only frames. A relay-side verifier must use the boundary-table
  semantics in `docs/specs/pane-delta.md`, not a strict `SplitAfter` count.
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
  rule); malformed `invite` params → hard reject instead of predecessor's
  silent downgrade to bootstrap import. Pairing spec should bless or fix.
- **Bootstrap `setup` token must be exactly 32 UTF-8 bytes** at
  `toPendingInvitation()` (relay-side requirement); link parse stays
  predecessor-loose (16–512 chars).
- **`AndroidKeystoreCipher` untestable on JVM** by design — fakes cover
  the store; needs an instrumented smoke test when emulator/Robolectric
  lands.
- **Draft debounce** intentionally left to the ViewModel
  (`snapshotFlow.debounce(300)`), unlike predecessor's built-in flush.

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

- **Peer-session revocation** — the required behavior is to disconnect every
  session bound to a revoked credential promptly. The initial Rust relay
  disconnected only the session that performed `revoke_device`; the required
  credential→session index is now recorded as resolved in round 8/9/10.
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
  can't be resolved fail `failed` like the predecessor.

## Phase-1 round-7 findings (stations, 2025)

### Subsystem ports (coordinator)

- **Push identity is connection-bound** — `ActionContext.client_id` is a
  connection label, not an authenticated device identity; device-binding
  for `push_test_device`/`push_viewed_pane` uses the session's credential
  device as the stand-in. Thread the enrolled device id through the
  session handshake when multi-device-per-credential matters.
- **Web Push delivery is live** — `push.rs`/`push_delivery.rs` own policy,
  subscription validation, signed refs, snooze, queue bookkeeping, and
  the VAPID + aes128gcm fan-out. The Android app subscribes through a
  UnifiedPush distributor (ntfy & co.) and renders delivered records
  locally; the FGS socket remains the primary channel while the app is
  alive.
- **Upload audit logging** — uploads require secret-aware attempt and result
  audit rows; this was completed in the later durable-audit work.
- **Speech voice updates** — catalog changes must fan out to every active
  session; this was completed through the shared notices broadcast.
- **Questions replay idempotency** — pending/fingerprint checks in the store
  provide the required idempotency behavior.

### Files mode (app)

- **`workspace_file` images decode from base64 in the ViewModel** — fine
  for icons/screenshots; large images will hit the pane-read cap first
  (bounded at the coordinator, contract-faithful).
- **Git diff shown for the selected file only** — `workspace_git_diff`
  returns a repo-scoped diff; the client filters hunks by path and degrades
  to "no diff" silently on parse gaps.

## Phase-1 round-8/9/10 findings (stations + orchestrator, 2025)

### Resolved this round

- **Push identity** now keys on `identity.device_id`: `ActionContext` carries
  both the transport `client_id` and authenticated `device_id`, while policy,
  subscriptions, viewed-pane state, and event references use the device id.
- **Web Push delivery** uses VAPID load-or-generate, RFC 8291 aes128gcm
  payloads, RFC 8292 ES256 JWT, a no-redirect 10 s client, and the documented
  delivery/retry/pruning rules. `queue.json` is durable; deliveries in flight
  at restart are intentionally not resumed.
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
- **App: worktrees** — sheet (list/create/open/remove with force
  escalation on `dirty_worktree_requires_force`) entered from the
  session bar ⋯ menu. The workspace tab strip under the mode switch was
  removed — it pushed the back stack once per pane switch and crowded
  the bar; pane switching happens from the Agents list.
- **Conversation history lands** — `internal/conversation` port:
  provider roots (claude/codex/qoder/pi/omp/omo/opencode/hermes/devin),
  bounded tail/JSONL/sqlite/ATIF-document reads behind strict
  containment (canonicalized root prefix + `O_NOFOLLOW`), `ConversationBrowser`
  behind a thin action adapter with the predecessor's
  `sameConversationTuple` post-read recheck. Devin is the first
  document-format provider: ATIF `steps[]` from
  `<XDG_DATA_HOME>/devin/cli/transcripts/<session>.json`, `step_id` as
  the entry id, tool calls wired from same-step
  `observation.results[]` via `source_call_id`, `system` steps dropped,
  unparseable documents report `source_corrupt`. Fixture-verified: 10
  suites, 39 vectors, 59 steps. Deliberate deltas: raw entry-id
  cursors instead of signed `hb1.` envelopes, no prepare/snapshot
  jobs.
- **Shadow determinism harness** — `lerdr-shadow` provides a scripted WS
  client, normalizer, and differ; `lerdr-fake-herdr` supplies the fixture
  endpoint, and `tools/shadow/shadow_diff.py` runs Rust-vs-Rust self mode.
  Identical repeated traces are the regression gate. Earlier predecessor
  comparisons are retained only as historical migration evidence.

### Still open

- **Idle RSS baseline (release builds, same socket, 2026-09)** —
  `lerdr-relay` measured approximately 10.4 MB at idle. Future load
  measurements, if needed, use the Rust self-determinism harness.
- **Plugin packaging exercised** — `herdr plugin link` registers the
  manifest (5 actions, 5 panes, build/startup/event hooks);
  `plugin-on-event.sh`/`plugin-on-startup.sh` → `lerdr-relay
  event-hook`/`startup-hook` → verified UDP datagrams. Still unproven:
  `plugin-build.sh` end-to-end (needs a published GitHub release —
  Phase-4 release pipeline).
- **App sends a web-push subscription via UnifiedPush** — the UP
  connector (vendored `org.unifiedpush.android:connector`) owns
  endpoint+key generation and RFC8291 decryption; the app sends
  `push_subscribe` on every relay CONNECTED edge and renders the
  decrypted `push.Payload` through the same notifier/reducer slot ids as
  socket-driven cards. Relay endpoint validation is structural (any
  `https:` host, 443-or-default port) so self-hosted distributors work.
  Push coverage gates the keep-alive pin: `shouldPin` (in
  `PushSubscriptionManager`) arms `RelaySyncService` only while a relay
  is connected AND no endpoint can reach a dead process — a subscribed
  distributor leaves the app permanently unpinned, and the service's
  own collect re-evaluates the same predicate so a START_STICKY restart
  can't zombie-pin.
- **`speak_text` on Android plays relay-synthesized WAV** — no on-device
  TTS fallback when the relay lacks `speech_synthesis` (capability-gated
  section hides; contract conformance).
- **Multi-relay settings are per-card** — sections render under each
  relay card; no global rollup (contract conformance: per-connection).

## Phase-1 round-11 findings (watch/target reconciliation, 2025)

The pane-watch and exact-target review closed with a dedicated `watch`
scenario. `lerdr-shadow` now compares repeated Rust runs over the full
surface: `server_session_id`, `session`, `session_name`, `format`,
`truncated`, `viewport_*`, `resize_settling`, `interaction`,
`question_layout`, and `target`.

### Resolved this round

- **Exact-target admission** — `validate_exact_pane_target`
  (`actions/target.rs`) runs between authorization and the audit-attempt
  write. Missing targets, mismatched `target.pane_id`, and stale tuples
  (`server_session_id`, `terminal_id`, `generation`, `agent_session_id`)
  reject with documented `invalid_request` shapes; `unwatch_pane`,
  `release_pane_size`, and `cancel_speech` are exempt.
- **`agents` frames carry the full target tuple** — `server_session_id:
  "primary"`, `generation`, `terminal_id`, `agent_session_id`
  (trimmed `agent_session.value`) are projected, sorted by `pane_id`.
  Without them Kotlin's `wireTarget()` refused to build targets and the
  app could not send any pane-directed frame against the Rust relay.
- **Per-pane generation tracking** — `Topology` keeps generation across
  accepted snapshots; `agent_stop`/`agent_clear` bump it on non-replayed
  effects (scheduler `slot.generation` is a separate counter, as in the
  predecessor).
- **Watch loop parity** — interval ticker (100/250/500/1000 whitelist,
  250 default) + invalidation fast path, so an update arriving inside
  the ack gate is picked up by the next tick instead of being lost
  forever (the review's stale-terminal defect). `watch_pane` re-issue
  replaces the watch. `pane_applied` matches the wire fingerprint
  against pending/acknowledged and foreign fingerprints push
  `pane_resync`; the 4 s ack deadline clears pending+acknowledged and
  forces a fresh full frame. `pane_delta` never carries `ack_required`.
  The ctl channel is bounded (`try_send`; acks coalesce).
- **Two-tier poll** — each tick runs the predecessor's `HandleProbePane`
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
  paths follow the committed wire contract; `capPaneContentLines` tail-caps
  content before fingerprinting.
- **`agent_state` projection** — `session` is `agent_session.value` (the
  predecessor's raw `SessionRaw.Value`), `session_name` is `""` — Rust has no
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
- **Read single-flight** — the predecessor dedupes concurrent `read_pane`
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
  unlike the predecessor's restart-persistent triage records.
- **`acknowledged.classificationAgent` probe leg** — the third
  `paneWatchNeedsFrameRead` trigger has no counterpart until the
  classification projection exists.

## Round 11 — live device test (emulator ↔ Rust relay ↔ real Herdr)

First on-device run surfaced three runtime-only defects no automated
gate caught; all fixed and re-verified live (pairing → connect →
agents/workspaces inventory → `watch_pane` terminal stream → ANSI
render → pane-size lease → interactive key bar):

- **`usesCleartextTraffic=false` blocked every `ws://` relay** —
  direct-LAN relays are the predecessor's normal path and the payload is
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
  Kotlin types `features` as a non-null map (the predecessor always
  allocates it); the Rust relay emitted `null`, so `push_config`/
  `herdr_status` failed decode → `UnknownServerMessage`, inventory
  stayed `starting`, and `acceptsInventorySnapshots` dropped every
  `agents`/`workspaces` frame. The relay now emits `features: {}`
  (no probe-ledger subsystem yet — the predecessor's map is evidence
  gathered by active probing).
- **Agent observation times** — `updated_at`/`last_seen_at` were 0,
  rendering "497253h ago" ages on device. `AgentTimes` now stamps them
  from snapshot-apply observation (change-keyed on `state_change_seq`/
  `revision`, bumped by `bump_generation`); `last_active_at` remains a
  declared gap (the predecessor derives it from the activity journal).

## Round 12 — phone-terminal interaction layer + mobile polish (2025)

Second live pass on emulator + an authorized physical Android device (`adb reverse`
USB tunnel — LAN 8377 is firewalled; `ws://127.0.0.1` over USB works
unchanged because the E2EE handshake is host-agnostic). Everything
below is verified on-device; multi-finger pinch verified by code path
only (no `adb input` equivalent).

### Landed this round

- **Paired-device rows** (`DevicesSection`) — metadata `FlowRow` +
  predecessor's ≤36rem breakpoint (`WIDE_DEVICE_ROW_MIN`): actions drop
  below the row as equal-width buttons. Fixed the timestamp
  char-per-line collapse.
- **Single special-keys bar** — merged the duplicate row; keys send the
  predecessor's exact wire spellings (`Left`/`Escape`/`Ctrl+C` — `send_keys`
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
- **Pinch-to-zoom** — `fontScale` 0.25–2.5× on the surface state; cell
  metrics re-probe → `lease_pane_size` re-leases automatically (native-size
  providers like omp skip the lease). **Fit width** in the ⋯ menu applies
  the scale that draws the widest committed row edge-to-edge — floor 0.25
  exists so ~200-col TUIs can be fully visible on a phone viewport; the
  toggle flips to "Actual size" (1.0×) once fitted.
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
- **Pane-size lease lifecycle** — `TerminalViewModel` renews on a 10 s
  cadence only while visible. Background/hide unwatches the pane and
  releases its size claim under the same ownership mutex. Hidden grid
  callbacks cannot acquire a lease; resume re-arms the latest measured grid
  immediately. The former five-minute hidden renewal grace is removed.
  The renewal loop rides `appScope`; `onCleared` cancels it and closes the
  owner so an obsolete screen cannot release its successor's lease.
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
  (predecessor payloads mirrored): `send_secret` (cap `secret_input`),
  `copy_agent_response` (15 s), `tab_reorder` (cap `tab_reorder`),
  `agent_start` (45 s), `agent_rename`/`restart`/`stop`,
  `agent_clear` (45 s), `workspace_create` (45 s) /`rename`/`close`
  (30 s, `close_group` + `expected_workspace_ids` supported) /
  `reorder` (block form when `workspace_reorder_block`, legacy
  `insert_index` otherwise), `list_directories` (10 s) with a parsed
  `DirectoryListing` model. `deviceRole`/`canControl` expose the
  enrolled credential role for the predecessor's `readOnlyRelayIds` UI gate
  (fail-closed: READER unless proven CONTROLLER).
- **Danger tokens** — `extendedColors.danger/onDanger/dangerContainer/
  onDangerContainer` (muted maroon, not saturated `errorContainer`) for
  deny/stop/destructive actions.

## Round 15 — Wave-1 feature wave (2026)

Five parallel streams closed the audit's UI-reach gaps; all mutations gate
on `canControl` (the predecessor's `readOnlyRelayIds` behavior).

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

## Round 16 — Wave-2 lifecycle seams: viewed pane, acknowledge, self-update (2026)

This wave completed three protocol-v3 lifecycle paths:

- **`push_viewed_pane`** (`SessionRepository.setViewedPane` +
  `setLocked`, fed by `TerminalViewModel` and `LerdrApp` lock state). The
  signature is `relay:pane:terminal:agent_session:generation`; it is non-empty
  only while visible, unlocked, connected, and on the primary session. Changes
  clear the former target and publish the new target; Feed/Files clear it.
- **`acknowledge_pane` on open** — `FeedViewModel` acknowledges opened panes
  for non-reader devices, and the store applies the documented optimistic
  `done`→`idle` presentation transition.
- **Relay self-update** — `checkUpdate`/`installUpdate` are `self_update`
  gated and reconcile completion after reconnect using the expected
  version/revision.
- **Tests** cover exact targets, deduplication, lifecycle clears, lock and
  capability gates, and update payload reconciliation.

Deferred product work included workspace-row reorder, diagnostics, OSC-52,
hardware-keyboard mapping, voice input, attachment ingress, and pane swipe.
`deploy_app_update` is reserved only; it has no emitter.

## Round 17 — Wave-3 remaining surfaces (2026)

This historical review closed the remaining in-repository UI-reach gaps.

- **Feed history diagnostics** provide continuation, oversized/corrupt-record,
  preparing, retry, and reload states; older pages prepend-merge by id.
- **Pull-to-refresh** is armed only at scroll top and drives
  `SessionRepository.inventoryRefresh`.
- **Workspace-row reorder** has no current UI surface.
- **Out of product scope**: voice input, OSC-52 clipboard, hardware-keyboard
  mapping, and pane swipe may return as product work, but are not contract
  debt.
- `deploy_app_update` remains a reserved v3 name with no emitter;
  `app_deploy_status` remains parseable for compatibility.

## Round 14 — relay wave: semantic layer, Herdr boundary, durable push (2026)

Three stations integrated the semantic layer, Herdr boundary, and durable push.
The completed self-mode `core`/`watch`/`semantic` shadow scenarios reported
identical repeated Rust traces.

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
- **Event-vs-poll commit split** — `CommitKind::{Event,Poll}` preserves
  committed blocked details on event updates while polls may replace them.
  The semantic scenario verifies this contract.
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
  event-stream staleness. It stays omitted until evidence exists; transport
  staleness remains on `Topology.stale`, and an unchanged reconnect does not
  republish `herdr_status`.

### Still open (unchanged deltas + deferred waves)

- `agents[*].{pane_revision,tokens,state_labels}`, workspaces'
  `{cwd,tokens,worktree}` — commit-epoch counter + Go-only fields;
  declared deltas, drop-keys remain.
- `action_receipt`/`push_config` — Rust-only v3 dispatch evidence and
  implementation-scoped payloads (census-visible, not compared).
- Startup-burst frame order — unordered pool comparison; the predecessor's
  fixed order is `push_config,agents,workspaces,activity_history,
  inventory_status` vs Rust's `push_config,herdr_status,workspaces,
  agents`.
- Mid-read `ContentRevision` fence — resolved (round 15 / S6); the
  counter is coordinator-side, portable, now enforced on all read paths.
- **Removed upstream, not ported**: `webrtc_*`/`herdr-dc-v1`,
  `lerdr-gateway`, `deploy_app_update`, portmap/UPnP — the predecessor's
  CHANGELOG made Tailscale the only transport and deleted these
  binaries/actions. Wire names remain reserved in the v3 catalog for
  compatibility; `app_deploy_status` stays a parseable frame with no
  emitter (same as post-removal predecessor behavior for the native app).
- **In flight (wave 2)**: release pipeline / CI matrix; `session_name`
  title resolver; `ContentRevision` mid-read fence; `[[link_handlers]]`
  manifest section; `internal/localize` residual audit.

## Round 15 — wave 2: relay leaves, release pipeline, upstream-removal audit (2026)

Three stations + orchestrator. The wave's headline finding was a
scoping correction: the predecessor deleted its entire non-Tailscale
transport surface (`lerdr-gateway`, WebRTC gateways, `herdr-dc-v1`,
portmap/UPnP, `deploy_app_update`/`deploying_app`, `stable-state`) —
"Tailscale is now the only transport" per its CHANGELOG. Those items
are recorded as **removed upstream, not ported** (roadmap annotated);
the v3 wire names stay reserved and `app_deploy_status` remains a
parseable frame with no emitter, matching post-removal predecessor behavior.

**Removal scope clarification** — upstream deleted *transports and the
app-deploy stage*, not the relay's local binary/HTTP surface. `/health`,
`/healthz` JSON, `/readyz`, `RELAY_INSTANCE_ID`, `update-worker`, and
`speech-voices` are transport-independent: `common.sh`'s health-wait
parses the `/healthz` JSON fields, `tailscale-serve.sh` probes it, the
self-update worker health-checks it post-swap, and `install_update` is a
live routed action the predecessor still ships. Those remain planned work
(S9), not part of the skipped web/gateway surface.

**Tailnet exposure** stays script-based: `tailscale-serve.sh` /
`tailscale-service.sh` / `plugin-tailscale-setup.sh` were already ported
(path-only diffs), wired as the `tailscale-setup` manifest command and
listed in release `REQUIRED_FILES`. A native `tailscaled` LocalAPI
serve-config path was considered and declined — it would duplicate the
script path with a new unsocketed API surface and no predecessor to shadow
against.

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
  delta:** the predecessor's title cache is unbounded; the port caps at
  2048 entries (sweep-then-clear) per the bounded-state rule.
- **`ContentRevision` mid-read fence** — the counter is the
  coordinator's own `content_rev` (the earlier "fake Herdr" note
  misattributed it). All three read paths now fence generation +
  revision: watch probe, watch frame read, direct `read_pane`
  (`mid_read_fence` extracted for ordering tests).
- **`[[link_handlers]]`** — manifest section + `plugin-open-link`
  scripts: GitHub issue/PR links in panes open a QR overlay on the
  phone. Spec-only (the predecessor ships none) but verified against
  upstream herdr 0.9.1's real manifest schema/env names.
- **localize residual** — audited: no gap. Wire errors stay
  `{code,args}`; `NormalizeLocale` + push localization already ported.
- **Release pipeline** — `.github/workflows/{relay,app,interop,
  release}.yml` + `scripts/{check-version-sync.sh,release-manifest.py}`
  + `plugin/scripts/{package-release,check-installed-release}.sh` +
  `docs/release.md`. Tag-gated 4-target builds (musl linux ×2, darwin
  ×2), per-target native smoke, APK (signed iff keystore secrets
  configured), version-sync gate, republish guard. Live gates
  (HERDR_LIVE/LERDR_*/live) are `workflow_dispatch`-only.
- **MSRV floor** — raised to 1.88: `icu_*` (via `url→idna`) already
  required it; the CI `msrv` job is a real gate now, not advisory.

### Still open

- **Release-management subcommands** — resolved (S8, `release.rs`):
  all five subcommands (`release-manifest`, `verify-release`,
  `activate-release`, `seal-release`, `prune-releases`) ported with
  predecessor CLI shapes and sync dispatch; `support-state.json` emits
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
- **Live smoke** — partially validated (an authorized physical phone paired to the Rust
  relay over Tailscale, live frames render); watch/lease/question
  flows on device still to exercise.
- **Phase 5** — untouched by design (binary inner codec, zstd,
  conversation subscriptions, binary chunks, capability-negotiation
  revision).

## Round 16 — S9: binary surface completion (2026)

- **HTTP health surface** — `/health` (text `ok` +
  `X-Herdr-Relay-Instance`), `/healthz` JSON (`status`/`readiness`/
  `inventory`/`instance`/`version`/`release_version`/`revision`/
  `protocol`), `/readyz` (200 `ready` vs 503 `unavailable`). Live
  inventory reaches the handlers through `InventoryProbeFn` — a closure
  over the topology `watch::Receiver` installed via `Relay::with_health`,
  same shared-cell idiom as `ResolverSlot`. `serving` is the predecessor's
  one-way `s.ready` latch. Verified live: healthz reports real inventory
  state against the running Herdr socket. Declared omission: `bundle_*`
  keys (no web handler exists to stamp them).
- **`instance_id`** — `Config` reads `RELAY_INSTANCE_ID` via `relay_env`
  (`LERDR_` > `HERDR_`), env-only like the predecessor.
- **`update-worker` subcommand** — detached job runner ported
  (`update_worker.rs`, ~1.6k LOC + 13 tests): reads the persisted
  `update-job-*.json`, downloads/extracts the release archive, verifies
  the manifest, health-checks `job.health_url` post-swap, writes terminal
  state. `UPDATE_WORKER_SUPPORTED` flipped true — `install_update`
  eligibility now reports the worker available. Divergences: archive name
  is `lerdr-relay_*` only (Go asset names would install the wrong binary);
  HTTP via `curl --max-time` + `\n%{http_code}` trailer; extraction via
  `tar` with the predecessor's caps enforced; no in-worker rollback (plugin
  install owns the swap — `failed` is terminal, job file retained).
- **`speech-voices` CLI** — `lerdr-relay speech-voices
  {list|missing|install|reinstall-runtime|remove} [--languages …]`
  sharing the wire actions' `Catalog`/`SystemEngine`; Go-flag binding
  forms and usage-error exit-2 contract pinned by tests. Plugin README's
  stale "no speech subsystem" note corrected.

Verification: 358 coord tests + 4 health integration tests green,
fmt/clippy clean, live smoke of all three endpoints.

## Round 17 — S10: upstream output-revision consumption + metadata projection (2026)

- **Capability audit basis** — `docs/11` now maps all 129 schema methods
  of herdr 0.9.1 (66 used, 63 classified). Headline: upstream Discussion
  #1277 landed — `content_revision` (seqlock: even=stable, odd=mid-write,
  `stale_content` on mismatch), `pane_output_changed{revision}` event,
  `min_revision` wait filters.
- **Consumed this round** (`lerdr-herdr` + `lerdr-coord`): topology
  subscription set attempts `pane_output_changed`; a shared
  `upstream_revs` watermark map lives on the attention ledger (max-merge,
  generation-scoped, swept on pane loss); every `pane.read` goes through
  `pane_read_fresh` — one re-read on `stale_content`, mid-read watermark
  drift, odd revision, or below-watermark result; watches wake on
  revisions newer than the served watermark and skip covered ones.
  **Declared inert path:** herdr 0.9.1's `Subscription` enum lacks the
  `pane_output_changed` variant (it exists in `EventData`/`EventMatch`
  only), so the subscription is refused today and every leg degrades to
  the existing tick — the capability adjudication (`supports_subscription`
  only) lights the path automatically once upstream ships the variant.
- **`tokens`/`state_labels` correction** — previously filed as Go-only
  projection deltas; they are herdr-reported (`pane.report_metadata` /
  `workspace.report_metadata`) and already deserialized upstream-side.
  Now projected onto `agents[]` (`state_labels`, `tokens`) and
  `workspaces[]` (`tokens`), additive-only (absent when unreported).
  Remaining honest delta: workspace `cwd`/`worktree` stay as before
  (`worktree` already projected; `cwd` has no upstream source).

Verification: 366+ coord tests green incl. fake-driven event folds and
watch coalescing, fmt/clippy clean, shadow self-mode IDENTICAL.

## Round 18 — Phase-5 Track A + tier-2 marginal surface (2026)

- **§0 capability negotiation landed** (`fbf432d` + `ae7ca0f`): inbound
  `client_caps` (unconditional emit by app, absorbed + symmetric
  `caps_update` reply carrying the server's advertised list), outbound
  `caps_update` (replaceable/coalescible, emitted on advertised-set
  flips), `NegotiatedCaps` per-session gate (`advertised ∩ announced`,
  `capability_unsupported` before the session-id fence), `TargetRef`
  gained `workspace_id`/`tab_id` additively.
- **Track A complete** — focus family (`fbf432d`: pane/tab/workspace/
  agent focus, session→pane resolution, partial-family per-method
  refutation) and pane-content families (`8334689`: `pane_search`,
  `pane_selection_read`, `pane_link_resolve`/`activate`,
  `layout_export`/`apply`). All wire actions are `protocol:3` additive,
  capability-gated both-lists.
- **Spec corrections folded back** (`docs/13`): `pane.link.resolve`
  returns `{regions}` cell bounds only — the URL surfaces on
  `pane.link.activate` `{handled,url}` (0.9.1 truth); `pane_search`
  adds `total`/`current`/`current_global` match metadata.
- **Tier-2 marginal herdr surface** (`4c7ee84`): `pane/workspace.
  report_metadata` (watch annotations — `lerdr_watching`/`lerdr_devices`
  tokens, wall-clock-floored seqs, 300s TTL server-side expiry),
  `client.window_title` ("lerdr: N device(s)"), `plugin.pane.*`/
  `plugin.*`, `server.reload*`, `integration.*` — 15 client methods,
  all capability-adjudicated; `plugin-pane`/`herdr-reload`/`integration`
  debug subcommands.
- Dropped by joint decision: `inner_codec_binary` (CBOR — removed from
  the plan after `frame_zstd` captured the real win; `docs/13` §2.1);
  `client_shell.surface.set` (thin-client-only); graphics/popup/
  input-set tier-3.

Verification: 730+ workspace tests green post-merge, fmt/clippy clean,
frozen vectors untouched. Track B in flight: `convo_sub` → `frame_zstd`
→ `upload_binary`.

## Round 19 — Phase-5 Track B transport upgrades (2026)

- **`convo_sub` landed** (`eb532ee`): per-pane conversation
  subscriptions — `subscribe_conversation`/`unsubscribe_conversation`
  gate on the capability; `conversation_update` pushes ride
  `reset:true` for initial/rotation/rebuild frames and `reset:false`
  for append-only tails, coalescible like the other snapshot streams.
- **`frame_zstd` landed** (this branch): negotiated zstd compression of
  `pane_content` payloads per §2.2. The envelope stays plaintext —
  `type`/`pane_id`/`target`/`content_fingerprint`/`ack_required` and the
  semantic fields route and coalesce as before — while `content` folds
  into `encoding:"zstd"` + `payload` = base64(zstd(`{"content":"…"}`)).
  `pane_delta` is exempt by spec (deltas already compress well);
  `pane_resync` carries no payload member to compress. Compression is
  applied at encode time inside the session send path — `Actor::enqueue`
  and `ClientSink::try_send` both consult the negotiated gate (an
  `AtomicBool` `NegotiatedCaps` keeps in sync on every `client_caps`,
  inbound `caps_update`, and observed outbound `push_config`/
  `caps_update`), so mid-session flips apply to the very next frame and
  `SendBuffer` byte accounting sees the wire shape. The capability is
  advertised unconditionally — no Herdr method stands behind it — and
  `effective_capabilities` never refutes it. `Outbound::decode` stays
  wire-faithful (compressed members pass through so decode+encode
  round-trips byte-exact); `PaneContent::decompress_payload` is the
  explicit inflate-and-restore for clients and tooling, bounded at the
  outbound byte cap against zip bombs. Measured on a realistic watch
  frame: 6099 B plaintext → 569 B on the wire at zstd level 1.
- **`upload_binary` landed** (this branch): negotiated raw-binary upload
  chunks per §2.4. A decrypted `0x03` payload —
  `[0x03][upload_id:32 ASCII][chunk_seq:BE64][bytes…]` — is dispatched
  before the JSON decode in the session frame path; the header carries
  the begin-issued opaque id verbatim (32-char base64url — the sketch's
  `upload_id:16` became the full string so no relay-side id mapping
  exists). `target`/`file_index` anchor to the staged session and
  `sha256` is measured on receipt; ordering/dedup/size/capacity/digest
  run the shared `chunk_locked` machinery so every outcome (and the
  `attachment_*` public codes) matches the JSON form, including the
  discard-on-failure rule. Acks stay JSON (`upload_chunk_result` with
  an empty `request_id`; `next_sequence` correlates); the
  `recordWriteAudit` attempt row is synthesized as the JSON-equivalent
  message so audit records read identically. The capability gates both
  directions: inbound `0x03` without negotiation answers
  `capability_unsupported` (session kept — a mid-flight retraction is
  not a kill race), a malformed header evicts like non-JSON plaintext,
  and `upload_begin_result` gains `chunk_encoding:"binary"` only while
  the capability is live — the same encode-time gate pair
  (`Actor::enqueue`, `ClientSink::try_send`) `frame_zstd` uses, now a
  shared `Negotiated` flag set. JSON/base64 chunks for non-negotiated
  clients are untouched, and carriers may be mixed within one upload
  (the sequence counter is the shared domain).
- Dropped: `inner_codec_binary` (CBOR — `docs/13` §2.1).

Verification: workspace tests green — 6 new `uploadbinary` unit tests
(header round-trip, edge sequences, every malformed shape, foreign id
rejection, encoding stamp), 5 new upload-manager/handler tests (binary
round-trip + JSON ack, validation parity incl. out-of-order args and
discard, size/capacity rules, mixed JSON↔binary sequence sharing,
audit attempt row), 5 new session tests (capability gate answers
`capability_unsupported` + session survives, negotiated `0x03` routes
through the e2ee pipe to the router, malformed header evicts,
`chunk_encoding` stamp follows announce/retract, error results never
stamped) — fmt/clippy `-D warnings` clean, `shadow_diff.py` core/watch/
semantic all IDENTICAL, frozen vectors untouched.

## Round 20 — Phase-5 E2E epilogue (2026)

- **App-side landed** (`dc7ade1` Track A, `8de761c` Track B — 27 files,
  +947): `client_caps` post-handshake unconditional, `caps_update`
  typed + `applyCapsUpdate` mid-session advertised replacement,
  `capabilityLive` gate in `RelayConnection`, 10 gated repository
  methods, `sendBytes` binary path that bypasses the send buffer but
  refuses while JSON is queued (ordering in the shared seq domain),
  FIFO ack correlation validating `file_index`/`next_sequence`/
  `received_bytes`, zstd-jni 1.5.7-16 (compileOnly + @aar, all ABIs),
  `FeedViewModel` convo_sub with reset-replace/append-dedup/stale-
  generation-drop and polling fallback.
- **Live-verified against `:8377`**: `caps_update{caps=22}`,
  `pane_content` `encoding:zstd` inflate round-trip (1865 chars),
  `conversation_update reset:true` 80 messages, `upload_begin_result`
  `chunk_encoding:binary`, `0x03` chunk ack `request_id:""` +
  `next_sequence:1` + `received_bytes:42` — session healthy throughout.
- **Coexistence fix** (`7febd29`): `agent.view.set` asserts narrowed to
  the documented loss points — first `Synced` (bootstrap) and the
  `[[startup]]` hook. Resubscribe `Synced`s collect capabilities only;
  the mid-session stomp on `hhdebb.herdr-radar`'s view (~4.4h hold,
  observed live) cannot recur.
- Roadmap `docs/05` Phase-5 marked landed; `inner_codec_binary`
  dropped outright — `frame_zstd` captured the compression win and a
  second inner codec is not worth its conformance surface. The relay
  no longer models `preferred_inner_codec` (senders are ignored like
  any unknown field).

Phase-5 is closed: every ratified capability is implemented,
negotiated, and exercised end-to-end on both sides.

## Round 21 — capability contract made canonical (2026)

- **The gap**: `push_config.capabilities` was assembled in code but the full
  list existed nowhere in the specification. Consequence observed live:
  `pane_realtime_delta` was never advertised, so the app's `watch_pane` gate
  never armed and terminals ran on manual reads alone. `tab_reorder`,
  `workspace_reorder_block`, `push_policy`, `typed_push`,
  `device_management`, `agent_response_copy`, and the `speech_*` pair were
  likewise absent.
- **The fix**: `docs/03` §4.1 now declares every advertised name, what it
  unlocks, and its gate. Code follows that table; historical sources explain
  wire provenance only.
- **Wired**: `tab_reorder`/`workspace_reorder_block` refute on their
  single backing methods (`tab.move`, `workspace.move_block`) like the
  other Herdr-gated caps; `typed_push`, `push_policy`, and
  `device_management` are unconditional — the VAPID push worker always
  runs (startup fails on a bad key) and the device-auth store backs the
  session admin actions unconditionally.
- **Deliberately not advertised**: `agent_response_copy` — the relay has
  no host clipboard backend and the action always answers
  clipboard-unavailable; advertising it would light a button that only
  fails. Revisit only if a clipboard backend is ever added.
- **Deferred (tracked, not dropped)** — later wired in this round:
  `speech_synthesis` / `speech_voice_management` / the
  `push_config.speech_languages` field. `Topology::local_speech`
  (`LocalSpeech{languages, management_supported}`) carries the
  relay-local catalog facts through the same commit→publish→`caps_update`
  path as Herdr evidence; `TopologyCommand::SpeechFacts` feeds it from
  the factory's post-construction `Speech::local_facts()` probe and the
  `change_speech_voice` handlers. `speech_synthesis` advertises when ≥1
  speakable language exists, `speech_voice_management` when the catalog
  reports management support, and `speech_languages` fills on the
  handshake (mid-session catalog changes move the caps, not the field).
  Confirmed against the app: `device_management` needs no client gate
  (its five admin actions all exist session-side), `typed_push` is
  unused but harmless, and `herdr-hybrid-v2` is intentionally absent —
  the hybrid/WebRTC transport is out of scope for the Tailscale-only
  deployment.

## Round 22 — live app verification (2026)

- **Warm-start deep links to an already-open `Pairing` key do not
  re-seed.** `MainActivity.onNewIntent` → `deepLinks` channel →
  `navigator.navigate(LerdrKey.Pairing(setupLink))` is wired correctly,
  but pushing a key that is already the back-stack top does not re-run
  the entry — the delivered `setupLink` is silently dropped and the
  screen keeps showing whatever it held. Observed live during the
  v0.2.5 smoke: `am start … lerdr://pair#…` while sitting on Pairing
  never populated the preview card; the cold-start path (seeded back
  stack) works. Applies to `agent`/`agents`/`settings` keys too when
  they match the current top. Fix belongs to the navigator (dedup or
  replace-on-same-key), not to matching.
- **`PairingScreen` never auto-connects a deep-linked `setupLink`.**
  The key's link only *prefills* the confirmation card — `connect`
  fires exclusively from the Connect button. That is the documented
  design (the link is sensitive; a silent auto-pair on an unsolicited
  intent would be worse), recorded here so the distinction is explicit:
  "deep link opens prefilled pairing" is intended; "deep link dropped
  entirely on warm start" (above) is not.

## Round 23 — orchestration cohort visibility (2026)

- **The gap**: the observed omp orchestrator panes had no
  `agent_session_id`; Herdr resolved `default_known_agent_idle_fallback`
  and reported `idle` while the TUI visibly coordinated work. Starting
  outside `agent start` is not itself the cause: an integrated omp
  process reports lifecycle regardless of how it was launched.
- **The derivation** (client-side, display-only): `workspaces` already
  carry `worktree { repo_root, is_linked_worktree }` — the root
  checkout hosts the orchestrator pane and linked worktrees host its
  dispatched children. A hook-less pane in a non-linked workspace whose
  same-`repo_root` linked siblings hold busy agents
  (working/blocked/attention) renders `orchestrating · N` in the
  working section, `orchestrating` on the unleased terminal chip, and
  the working accent in the feed. Hook-bound panes (which report real
  status) and worktree members (children, not orchestrators) never
  derive.
- **Boundary kept**: nothing is asserted into pane state — no
  `pane.report_agent` on foreign panes and no `ws_key` coupling (that
  field is owned by the radar projection, not the wire). Real lifecycle
  comes from Herdr's supported omp integration in the active profile;
  no new upstream self-reporting implementation is required.

## Round 24 — session lifecycle visibility (2026)

- **Terminal header precedence**: a pane snapshot with positive columns
  replaced the agent's status with `lease N×M`, hiding both working and
  blocked transitions. The header now projects lifecycle independently of
  geometry; dimensions remain in the pane metadata row. Working uses the
  working accent, blocked uses the existing waiting variant, and idle
  stays neutral. The obsolete lease-header variant is removed.
- **Profile-scoped omp integration**: the global extension existed but
  the live `sundevs-work` profile had no extension. Installing it in the
  active profile produces native `herdr:omp` session identity and
  working/completion events. Existing omp sessions must `/reload` or
  restart to load a newly installed extension; installation alone does
  not alter a running process. See [plugin setup](../plugin/README.md#omp-lifecycle-status).
- **Cohort fallback limit**: worktree derivation cannot observe subagents
  inside the same omp process. It remains a display-only fallback for
  separate child panes, not a substitute for the lifecycle integration.
- **Relay short-transition loss**: `pane.agent_status_changed` was absent
  from the topology subscription set, so `agent_status` only moved on
  poll commits (15 s reconcile plus wake refreshes), and the plugin's
  `agent_event` datagram discarded its own `status`/`pane_id` payload to
  trigger a sampling `refresh()`. A `working`→`idle` burst that settled
  before the sample never committed: no transition, no `done`+`unseen`,
  no `content_rev`. The relay now subscribes to the event (gated like
  `workspace.reordered`, dropped on handshake refusal) and commits the
  carried status through the full accept pipeline; the UDP datagram
  commits its own payload the same way, keeping real transitions on
  older Herdr versions that refuse the subscription.
- **Lifecycle subscription scope**: Herdr 0.9.3 requires `pane_id` on every
  `pane.agent_status_changed` entry. A global named entry rejected the entire
  subscription, leaving the app idle during observed native working. Discover
  all current panes, subscribe each one, then reconcile an authoritative
  snapshot. Rebuild coverage after membership changes, including changes between
  discovery and bootstrap. Buffered status events only invalidate a fresh read;
  a resync snapshot adopts current lifecycle rather than preserving stale status.
- **Echoed decoder refusals**: the live Herdr 0.9.3 handshake rejected
  `pane.output_changed` with `invalid_request` and an echoed request ID. Treat
  that initial decoder refusal as pre-dispatch within subscription setup so the
  named optional variant is removed; other RPC dispatch semantics stay unchanged.
  The same live topology subscription then succeeded. The unchanged minified
  API35 app showed a genuine Codex working header and live Working row.

## Round 25 — Android audit boundaries (2026)

- **Warm navigation**: same-destination intents do not add duplicate stack
  entries. A changed setup link re-seeds an already-open pairing form;
  pairing still requires the explicit Connect action. On the rebuilt
  minified physical APK, two warm settings intents followed by one Back
  returned to Agents.
- **Warm enrollment ownership**: pairing starts and awaits a fresh transport
  under the repository's session lock. Ordinary registry reconciliation remains
  idempotent. A pending invitation must not reuse the old connection's
  `Connected` verdict while its credential is still being replaced. The real
  owned Reader app showed `Pairing failed` even though the relay redeemed its
  invitation. The stale-verdict regression failed before correction; all six
  pairing tests and full Gradle tests passed afterward, along with debug/release
  builds. A HOT same-endpoint invitation on the signed minified API35 Reader
  then returned to live Home and retained the read-only reply gate. Conversation
  history acceptance is separate: the resumed native Codex had not reported its
  conversation session, and Feed correctly displayed that limitation.
- **Update permission**: only the activity-result callback resumes a
  pending update after the app-specific unknown-source settings page.
  Entering the Allow phase must not simulate returning from that page.
  Physical verification reached Android's audit-labeled `SpaActivity`;
  returning denied restored the Update action without downloading.
- **Update trust boundary**: copy the download into private storage, verify
  package identity, a strictly newer version code, and signing compatibility,
  then revalidate the staged bytes before granting a read-only installer URI.
  Single-signer rotation requires the candidate's history to contain the
  installed current signer; multisigner updates require the exact signer set.
  A process-death completion notification routes to review, not the installer.
  A successful newer, compatible signed update remains a separate live gate;
  an isolated application ID or different smoke signer cannot prove it.
- **Native lifecycle evidence**: the isolated OpenCode 2 TUI loaded the
  installed Herdr integration from its directory entrypoint, not its JS file.
  A real assistant turn then produced working/completion events. Cached
  readiness and API-created forms are not inference or model-question proof.
  Original credentials and global integration configuration were unchanged.
- **Draft authority**: persisted text hydrates each identity once; a local
  edit wins over a delayed initial read or an earlier queued save. Saves use
  application lifetime so leaving the screen does not cancel its last edit.
  Both delayed-read regressions failed before the fix; exact rapid-input
  prompts reached real OpenCode and OMP turns on the minified physical app.
- **Raw request ownership**: correlation also requires the current relay,
  session handle and connected epoch. Disconnect fails pending work as
  `dispatched_unknown`; cancellation removes its pending reply and timer.
- **Native OMP footer spelling**: the installed Ask dialog renders glyph
  Enter hints, not the older ASCII `enter select` / `enter submit` text.
  The classifier accepts both without adding wire fields. A genuine model
  question on the minified physical app accepted phone-selected Beta, wrote
  the native Beta tool result and assistant reply, then returned to done.
- **Native OMP multi-select controls**: current footers advertise Space-toggle
  and Enter-confirm, while older menus use Enter-toggle. A private, non-wire
  control flag is derived from the live footer; custom input returns to the
  question before confirmation. Physical Alpha + Gamma + a custom answer
  reached the exact native result and assistant response.
- **OpenCode free-provider question evidence**: the isolated native Big
  Pickle model generated a real single-select question; phone-selected Beta
  reached its native accepted-answer response, completed tool card and done
  header. This resolves that family's question gate without repeating the
  Fledge-only malformed Chat deltas or Responses HTTP500, changing global
  authentication, or switching to a paid model.
- **Mutable JSONL projection**: a tool-result record updates its earlier call
  entry without changing that entry's content-derived id. Live subscriptions
  compare tool-bearing entry digests as well as SQLite row digests; immutable
  text-only JSONL rows need no second hash. The same physical Feed changed
  the real OMP call from called to completed after its result, without reload.
- **Reader launch admission**: Home exposes agent/workspace creation only
  when at least one paired relay has a proven controller role. Reader-only
  and unknown-role inventories keep navigation and live agent status,
  without a launch menu or empty-state instruction to use it.
- **Reader speech boundary**: voice listing, synthesis and cancellation are
  read-only actions; voice installation/removal are mutations. Readers keep
  the catalog and playback controls, without download/remove actions; the
  ViewModel also rejects stale mutation callbacks before dispatch. Genuine
  Reader enrollment on a separate minified physical APK verified both
  omissions and real Android playback/cancellation without replacing the
  existing controller credential.
- **Draft write order**: application-lifetime saves enter the shared mutation
  queue before yielding. A confirmed send clears only its submitted snapshot;
  later edits and drafts for other identities survive. Cleanup of an expired
  read cannot delete a newer record written while that read was pending.
  The minified physical app restored exact independent unsent drafts after a
  cold signed reinstall; the apparent empty first draft was a Terminal input
  field, not the Feed composer. Owned probes were cleared without submission.
- **Screen lifetime ownership**: pane watches, conversation subscriptions,
  viewport leases and viewed signals use explicit local owner tokens. Acquire
  and teardown serialize through the receipt boundary; old cleanup cannot
  stop a reopened screen. New session handles inherit current visibility.
  These tokens never enter protocol payloads.
  The minified physical app repeatedly reopened Feed and Terminal while a
  genuine OpenCode question remained pending; the phone-selected answer
  still produced a live completed tool card and assistant result in Feed.
  This does not replace the stale-owner interleaving regressions.
- **Cancelled update downloads**: a missing DownloadManager row is terminal,
  like a failed download. Clear pending staging before exposing the failure;
  a cold start must not restore the dead download's poller.
- **Directory reopen**: opening the browser refreshes its selected folder
  unless a load is already pending. The minified physical APK discovered a
  project directory created while the browser was closed, without reconnect.
- **Raw start registration boundary**: a successful `pane.send_input` may
  precede `agent.get` registration. Only that detection loop treats
  `agent_not_found` as pending within its existing deadline, without repeating
  the launch. Other detection or naming failures preserve `dispatched_unknown`
  because the aggregate action already sent the command. The physical failure
  created a real Codex pane despite a refusal receipt; the corrected isolated
  relay and minified API35 app started a differently named owned case with
  corroborated native name and cwd. No protocol fields or global transient
  refusal set changed.
- **Custom raw claim identity**: `pane.report_agent.agent` does not populate
  `AgentInfo.name`; the admission API has no name parameter. Rename the claimed
  pane before returning success, sharing the existing five-second response
  reserve with admission. Preserve the aggregate dispatched boundary on a
  later refusal. The dedicated minified API35 app started a real non-LLM raw
  fixture once; native `agent.get` and the app header both retained its requested
  name and owned cwd. This is lifecycle identity proof, not model acceptance.
- **Initial launch catalog readiness**: profile rows are handshake-only state;
  resolving them after an early client receives `agent_profiles: null` leaves
  that session's picker empty. A live Settings Reconnect is a health ping, not
  necessarily a new handshake. Initialize the bounded existing catalog before
  accepting clients, including its existing unavailable-Herdr local fallback.
  Do not add a mid-session message or repeat `push_config` and its reset effects.
  The dedicated minified API35 app stayed running across the isolated relay
  cutover, auto-connected 3.43 seconds after listen, and offered both owned raw
  profiles without a cold restart or re-enrollment. A stalled-inventory consumer
  regression failed before initialization and passed after it.
- **Explicit launch name ownership**: automatic cwd/profile suggestions apply
  only until a name edit occurs, including an edit equal to the suggestion.
  Late directory results must not overwrite that choice; fresh agent forms,
  relay selection and accepted submit reset the provenance. The updated
  same-signer minified API35 APK retained a chosen name through the real cwd
  browser, after both overwrite regressions failed before and passed after.
- **Observer geometry versus TTY lease**: `stty` changes the process window
  size, not Herdr's native VT grid. A phone-sized read-only observer crops that
  grid rather than reflowing it. The committed native layout must therefore
  determine each known observer dimension; lease dimensions are only a fallback
  when layout geometry is unavailable. The dedicated API35 audit reproduced a
  real `Password:` tail missing from the leased 54×10 observer but present on a
  covering surface; native layout remained 129×42. No secret was submitted
  through the stale generic composer. This changes read geometry, not wire fields.
  The bottom/right prompt regression failed before the geometry correction and
  passed after it. The unchanged minified app then showed the same waiting native
  prompt; one masked audit-only answer was consumed without echo, cleared its
  composer and remained absent from the isolated activity/write-audit logs.
- **Copy-engine zero coordinates**: the frozen point shape requires both `row`
  and `col`, including the origin and first-row/column boundaries. Default-value
  omission produces `{}` or a partial point, which the relay correctly refuses.
  Range endpoints are likewise required even when they equal the origin. The
  API35 audit found no served-search annotation while native read-only search
  returned 111 real marker hits; shared model serialization must retain these
  fields, without weakening native validation or changing the wire contract.
  The origin/axis/range wire regression failed before the model annotations
  and passed after them. A separately signed minified audit package was genuinely
  enrolled without replacing the earlier audit app; its actual Find field showed
  `111 in scrollback` for the same unchanged native history. Actual first-column
  drag/copy selection was also exercised without inspecting clipboard contents.
  That UI reads frozen local rows; `paneSelectionRead` currently has no UI caller.
  A separate real native read-only zero-column selection returned the known marker,
  so no server-selection request from the Android gesture is inferred.
- **Managed pane replacement**: `agent_restart` and `agent_clear` return a new
  raw `data.pane_id`; neither action respawns the old pane in place. Keep the
  management sheet alive while the action is pending even if the old agent
  disappears. Qualify the returned ID with its enrolled relay and replace all
  matching Feed, Terminal and Files Back-stack entries without changing mode.
  Both actions share the existing 45-second replacement deadline. The generic
  15-second Restart timeout can discard a successful late replacement reply.
  A delayed-disappearance regression failed before deadline alignment and
  passed after it. The same-signer minified API35 app followed an actual owned
  raw fixture through Restart in Terminal and Clear in Files; Back returned
  Home rather than either closed pane. Fixture output is not model acceptance.
- **OMP ASCII Ask frames**: native OMP can render `+- Ask` headers with `|`
  edges and `+`/`-` borders. Recognize those headers and discard structural
  border rows when extracting the question and option descriptions. Keep the
  existing live-footer requirement so completed tool output cannot reopen an
  interaction. A genuine GPT-6-Luna Ask reproduced terminal-only fallback;
  its regression failed before the decoder correction. After the correction,
  the unchanged minified API35 app answered that same pending question inline
  with Beta, and both native OMP and Feed showed `OMP_QUESTION_ACCEPTED Beta`
  with the Ask tool completed. All 506 coordinator tests and warning-denying
  all-targets Clippy passed; no protocol fields or frozen vectors changed.
- **Wrapped Codex question footers**: navigation and cancel hints may wrap
  below the live submit row. Accept only those recognized footer controls;
  later response text still rejects a historical question. A genuine native
  Codex two-question form reverted to terminal fallback in the minified API35
  app. After the relay correction, that same pending form accepted Beta,
  Previous/Next navigation retained Beta, and the second answer was Two.
  Native Codex and Feed both showed
  `CODEX_TWO_ACCEPTED — Marker: Beta; Number: Two`. The wrapped-footer
  regression failed before the correction; all 508 coordinator tests and
  warning-denying all-targets Clippy passed afterward.
- **Background viewport release proof**: an owned minified API35 Controller
  held the actual OpenCode TTY at 54 columns × 34 rows. Backgrounding it
  restored the native desktop baseline, 129 × 42, after the relay's release
  grace (10.76 s observed); foreground resume restored 54 × 34. A new real
  terminal prompt then produced `EMU_OPENCODE_TERMINAL_BETA` in native
  OpenCode and the live phone surface. The visibility regression failed
  before correction. Full Gradle tests, debug and release builds, and the
  signed minified audit clone build passed afterward.
- **Unknown Home activity times**: a fresh native inventory can intentionally
  carry `updated_at = 0` and no `last_active_at`. Zero means unobserved, not
  Unix epoch; show the lifecycle/attention label without an invented age.
  Positive last activity still takes precedence over a known update time.
  Cold-launching the old minified audit clone rendered genuine owned Codex
  and OpenCode rows as `497517h ago`; the corrected signed minified clone
  showed `idle` and `done`, while known rows retained their real ages.
  The missing-time regression failed before correction; Home tests, recorded
  and verified Roborazzi coverage, full Gradle tests, and debug/release builds
  passed afterward. The relay timestamps and frozen wire contract are unchanged.
- **Feed editor ownership**: `FeedViewModel.composerValue` is synchronous
  Compose snapshot state containing text, selection and IME composition.
  `FeedUiState` no longer carries an asynchronous copy of the draft. Native
  input updates the editor immediately; only text changes enter the existing
  persistence queue and advance the edit generation. Programmatic replacements
  place the caret at the end. Restoration and successful-send clearing retain
  identity/edit guards, so late reads and completions cannot erase newer input.
  The owned API35 minified app corrupted an uploaded reference during rapid
  ADB typing; Android DocumentsUI retained the same reference with identical
  input. After correction, the same Feed input retained the full reference,
  multiline prose and a verified midtext insertion/deletion. Genuine Codex
  read the uploaded file and returned its first-line marker in native output
  and live Feed; the submitted composer cleared. Existing regression tests
  cover delayed-store composition/selection and newer edits during submission.
  Full Gradle tests, Feed Roborazzi verification and debug/release builds passed.
  This is emulator evidence, not new physical-device or API28 acceptance.
- **Tab insertion boundaries**: `tab_order` is a one-based ordinal, while
  Herdr's `insert_index` selects a zero-based boundary in the original strip
  before the source is removed. A right move uses `position + 1`; a left move
  uses `position - 2`. The real two-tab audit workspace confirmed a no-op for
  the old right boundary `1`, leaving the app waiting indefinitely. The same
  native API moved the source right with boundary `2`. A behavioral regression
  failed before correction and passed afterward; obsolete request-index pins
  were removed rather than re-pinned. After correction, the signed/minified
  API35 app moved right to position 2 and left back to position 1, with real
  host order checked and no added timeout or synthetic topology. Tab-order
  UI verification passed after recording its previously missing refusal
  screenshot baseline. Frozen wire fields and vectors are unchanged.
- **Large-text bottom navigation**: keep destination labels on one line with
  ellipsis; do not reduce the user's font scale. At 200% system text on the
  owned API35 emulator, `Computers` split into `Compute` / `rs` and enlarged
  the whole bar. The signed/minified correction keeps a single-line label,
  retains the full accessible icon name, and opens the actual Computers screen
  through that name. Enlarged navigation targets measured 80dp high at 420dpi.
  A Home text-layout regression failed before the fix; its new Roborazzi
  golden and all existing Home screenshot checks passed afterward. Original
  emulator font scale was restored. This does not claim a TalkBack audit.
- **Invitation clipboard privacy**: one-use setup links carry enrollment
  secrets, so copied links set `ClipDescription.EXTRA_IS_SENSITIVE` before
  reaching the clipboard. This follows Android's
  [sensitive-content guidance](https://developer.android.com/develop/ui/views/touch-and-input/copy-paste#SensitiveContent).
  On the owned API35 emulator, the previous minified APK's system clipboard
  overlay contained the actual invitation URI. The corrected minified APK
  showed only masked dots in that overlay; native paste still populated the
  real invitation and its endpoint preview. Connect was not pressed and the
  preview was cancelled, retaining the original Controller. Authentication
  links are redacted in the generated private UI evidence. Clipboard
  metadata does not replace expiry, single-use redemption, or revocation.
- **Warm push-distributor discovery**: returning from installing a distributor
  rechecks discovery only while the app reports no distributor. Reuse the
  repository's existing visibility flow; do not re-register active endpoints.
  The actual official signed ntfy installation was discoverable by Android,
  but the old minified app stayed Off until a cold restart. The corrected
  minified app changed from Off to Active on warm return with the same PID,
  unchanged enrollment, and no app-data clearing. A later ordinary resume
  retained the active endpoint. The JVM regression covers available choices;
  real SDK registration needs AndroidKeyStore and was exercised on the device.
- **Viewed local notifications and Feed ownership**: Feed publishes its viewed
  pane for its composition lifetime, using the repository's existing owner
  guard so stale disposal cannot clear a newer screen. Local notification
  reduction excludes the visible unlocked pane, retracts its standing card,
  and preserves other panes and summary collapse. The DI binding passes the
  repository's observable view to the notifier after construction; neither
  constructor depends on the other. A genuine pending Codex question produced
  an alert while already open before correction. The corrected minified app
  retracted that same alert, kept a fresh viewed question and completion quiet,
  and still notified an unviewed OMP completion. A new background question
  notified until the actual Feed returned. Another real question arrived
  behind native PIN authentication and was retracted after unlock and entry.
  Original app-lock and owned emulator credential settings were restored.
- **Membership refresh and queued status invalidations**: a pane membership
  event previously restarted the supervisor immediately, discarding later
  bootstrap-gap and already queued live events. Forward the whole gap and a
  fixed count of live queued events before replacing subscriptions; do not
  wait for future traffic or replay gap statuses over a newer snapshot.
  The Unix-socket regression failed before correction and now forwards
  `pane.moved`, `working`, and `idle` before the next bootstrap. A separate
  client using the changed supervisor against real Herdr observed native
  pane movement and tab closure before resync, then genuine OMP working and
  completion. This native trace does not claim both statuses preceded resync.
- **Cold restored Terminal and initial target inventory**: a saved Terminal
  may open before its relay session and agent target exist. The Connected
  resync then cannot encode the read. After an accepted authoritative agent
  snapshot, re-arm only open foreground panes still waiting for first content;
  desired-watch state alone does not prove the initial read was sent.
  The consumer regression failed with a null snapshot and passes after the
  correction; closing the owner before later inventory cannot reopen it.
  On the current minified candidate, an actually STOPPED saved task survived
  exact owned-process SIGKILL and a COLD launcher return. Its original Terminal,
  plain draft and midtext caret returned with genuine native OMP content,
  without mode switching or manual Refresh. Only the unsent probe was cleared,
  and temporary root on the owned emulator was restored. Synthetic crash trials
  that removed the Android task are not saved-task acceptance.
- **Feed pagination anchor and drawing ownership**: stable entry keys cannot
  retain a `load-older` header that disappears after the final older page.
  Capture its first visible real entry and measured offset before loading,
  then request that entry's new index after the page settles. Other prepend
  anchors remain key-driven. The Compose regression failed with the previous
  expanded row absent and now retains its exact offset and expansion state.
  The current signed/minified app repeated the genuine 201-call OMP history:
  native line-16 Input stayed at `[84,825][870,1062]` after actual Load older
  turns exhausted the cursor. Real bitmap inspection also exposed stale recent
  tail graphics over historical rows and fixed tabs. Transcript and blocker
  rows no longer retain exit-animation layers; clip the list viewport. The same
  native history and expanded row then rendered cleanly before and after prepend.
- **Tall final turn and passive updates**: `viewportEnd - itemEnd` is negative
  when the final turn continues below the visible viewport. Only a gap within
  the existing two-pixel rounding slop and 48dp near-bottom threshold can re-pin;
  an arbitrarily negative gap cannot. Initial manual scroll appeared stable,
  but the old retained pin yanked the view on subsequent real output.
  The streaming-growth Compose regression failed before and passes after.
  Genuine native 64-row arithmetic still followed to completion while pinned.
  After actual manual scrollback into a new 64-row final answer, an external
  native arithmetic update completed while every visible row and exact pixel
  bound stayed unchanged and the new reply remained offscreen.
- **Approval choice readability and scope**: Home and Feed keep original
  approval labels in full-width multiline buttons. Rounded rectangular shapes
  prevent long labels from clipping against oversized pill corners. Home's
  general chip-label truncator is not applied to permission choices.
  A genuine Codex `0.160.0` dialog exposed two losses: the app displayed
  `Yes,` / `Yes, and` / `No, and`, and the relay omitted hanging continuation
  lines containing the persistent choice's command prefix. After validating
  the existing menu/header/freshness gates, Codex projection retains those
  established between-row lines with their displayed breaks and the existing
  500-character bound. It does not absorb the final option's tail or relax
  rejection of newer output. The same pending native read showed all three
  policies and its owned-file scope in the signed/minified Home and Feed.
  Only the one-time choice was selected; no persistent permission was granted.
- **Terminal editor restoration**: plain input uses `TextFieldValue.Saver` so
  restored text retains its selection; secret text is still memory-only.
  The component regression previously inserted `beta` at the start after
  restoring `alpha omega`; it now inserts at the saved middle caret. A
  separate regression confirms secret text is absent after restoration.
  The owned minified emulator accepted an exact 358-character native Terminal
  task before real Codex submission. Partial long synthetic input was traced
  to Android `InputDispatcher` stale-event drops, not editor corruption;
  fresh bounded batches retained the full task without added typing delays.
  A controlled native font-scale roundtrip retained the actual `w1T:p2`
  Terminal route and `alpha omega` draft. The existing canvas-to-IME action
  refocused the editor without moving its caret; typing `beta ` then produced
  `alpha beta omega`. The original system scale was restored and the unsent
  probe cleared. An earlier Home observation was not reproduced by this
  controlled sequence; no navigator or minifier-rule change was justified.
  A subsequent actual portrait-to-landscape configuration change on the owned
  emulator retained `w1T:p1` Terminal, the same draft and midtext caret. Native
  insertion produced `alpha beta omega`; the unsent probe and original rotation
  settings were restored. This is separate from the earlier font-scale check.
- **Management editor and dismissal ownership**: rename input is synchronous
  `TextFieldValue` state, following the Feed editor convention. Inventory may
  seed an untouched field, but cannot replace an edited name or its selection
  and composition. Saving reads the immediate value. Handled dismissal and
  replacement results are consumed before closing the sheet, so a retained
  ViewModel does not immediately close its next opening. The owned API35
  minified app corrupted an edited test tab name before correction. After
  correction, fast input
  retained the exact name; a midtext insertion could be deleted without
  corruption. Two successful native renames each allowed management to reopen,
  and the original owned tab label was restored and verified in Herdr. The
  management regression suite and unchanged Roborazzi goldens passed.
  Batched synthetic Left events did not land at the assumed offset; their
  exact repeat count is not accepted by this check. No new physical-device or
  API28 acceptance is claimed.
- **Feed blocker kind headings**: label only actual `approval` and `question`
  kinds as those actions. Other blockers use `ATTENTION NEEDED`, matching
  Home's existing generic attention semantics. A genuine owned OpenCode
  request returned the provider's `Rate limit exceeded` failure; Home correctly
  displayed `attention`, but Feed labeled the same unknown blocker `QUESTION`.
  The corrected signed minified app kept the provider failure and terminal
  inspection path visible under `ATTENTION NEEDED`. This is a renderer fix,
  not a new classifier rule or acceptance of an OpenCode question. Full Gradle
  tests and debug/release builds passed; the provider limit remains external.
- **Find scope labels**: the shared find bar takes its placeholder from the
  owning screen: Feed uses `Find in conversation`; Terminal keeps
  `Find in terminal`. A minified API35 Reader session opened Feed search and
  displayed the corrected field, alongside its read-only reply notice.
  This session had no conversation log, so this proof covers the label and
  role controls, not matching or pagination. Existing genuine conversation
  search acceptance remains separate.
- **Emulator continuation**: after explicit physical-device disconnection,
  remaining audit coverage runs on a dedicated API35 AVD. Keep its proof
  separate from physical acceptance. The historical owned API28 environment
  failed in SystemUI/telephony with a missing WifiManager; its startup was not
  visual acceptance. A new isolated AOSP API28 AVD subsequently cold-started the
  corrected minified cold-restored artifact, exercised real native PIN challenge,
  cancellation and unlock, restored the temporary lock state, and shut down.
- **Conditional runtime acceptance**: manual cwd fallback is reachable only
  without `directory_browser`; the current Rust relay advertises it
  unconditionally. A genuine authenticated peer omitting/revoking that
  capability is required for the missing runtime case; synthetic capability
  frames are not proof. Workspace rename has no production UI caller.
  Document/image attachment selection passed through the native document
  picker; there is no attachment-camera launcher to exercise.
- **Question and Activity limits**: the genuine Codex multi-question form
  exercised next/back and explicit answer submission, but does not expose
  `can_chat`; clarification needs a supporting native provider form.
  Activity refresh and filtering passed against real relay data. It pulls
  at most 500 entries, with no UI paging or clear-history control. Response
  copy is naturally omitted by the current relay; Activity omission still
  requires a genuine peer without that capability.
- **Trusted app upgrade boundary**: the official signed app upgraded from
  0.2.6 to 0.2.7 through canonical download and Android PackageInstaller on
  an empty owned device. Its published permission gate needed the ordinary
  app-specific Settings grant; current source already handles activity-result
  return. No newer matching trusted published artifact exists for the
  unreleased audit package, so current private staging/signing checks have
  no end-to-end runtime acceptance. Changing package identity, signing key
  or version to manufacture eligibility is not verification.
- **Visible Terminal accessibility**: expose the drawn viewport as text, not
  hidden scrollback, and defer scroll reads to semantics. The existing canvas
  and measured rows remain the render path. A genuine enrolled Reader on the
  signed/minified API35 APK focused visible `FRAME_DELTA_NATIVE_TAU 171`;
  scrolling back exposed `FRAME_DELTA_NATIVE_SIGMA 136` and omitted the
  offscreen newer reply. Native TalkBack hardware focus captures were taken
  after stopping hierarchy instrumentation. Audible speech and full-app
  accessibility acceptance are not inferred.
- **Settings notification readability**: place the system-settings action
  below the notification status rather than narrowing its headline with a
  trailing button. The signed/minified guest screen at 200% font on a
  393dp phone displayed the full Notifications heading and action. Its
  unpaired guidance now points to the implemented Computers pairing entry.
  Roborazzi large-font coverage checks the headline remains on one line.
- **Adaptive theme and file-preview targets**: measure theme labels with the
  current typography and density. If equal segments cannot fit, show full-width
  selectable radio rows. The signed/minified 393dp/200% native screen displayed
  complete System, Light and Dark labels, with one checked native parent after
  each transition and System restored. The Files preview Back container is
  explicitly 48dp; its genuine owned-file clickable ancestor measured
  132×132px at 440dpi (48×48dp), and returned to the file list.
- **Relay update status layout**: keep status text full-width with actions below
  it. The same signed/minified 393dp/200% native screen displayed the genuine
  available 0.2.7 version and revision on complete lines. The old peer did not
  retain its worker's failed state on this final screen; this is not native
  failed-state acceptance. Large-font Roborazzi coverage checks the available
  heading and revision without relying on blank paragraph overflow flags.
- **Managed relay upgrade boundaries**: a separately established, genuinely
  installer-owned canonical 0.2.6 service upgraded through the unchanged current
  hook to canonical 0.2.7. Its real systemd invocation/PID changed, health became
  ready, and the nonroot relay retained zero capabilities. A missing standard
  WorkingDirectory first caused a safe refusal and successful real rollback;
  the distinct unit prerequisite was corrected before success. This does not
  accept the original unclaimed configuration or immutable published worker.
  That native worker stopped before activation because the plugin was locally
  linked. A genuine pinned GitHub install then stopped at the published hook's
  obsolete WEB_HASH identity gate. A corrected published hook is required for
  native end-to-end managed-update acceptance; no release or replay was made.
- **Last-controller cleanup guard**: native Forget confirmation alone is not
  revocation evidence. The isolated old peer retained its sole controller;
  a separate genuine credential-authenticated revoke returned
  `cannot revoke the last controller`. Its private retained credential remains
  inactive after owned service/container shutdown. The original native receipt
  was not captured, so the separate rejection is not attributed to that frame.
- **Adaptive session modes**: measure the widest label against Material's inner
  padding and checked-content width, not the theme selector's more conservative
  allowance. An oversized allowance unnecessarily replaced healthy normal-font
  controls; the corrected decision preserved full-screen screenshot goldens.
  At 393dp/200% the genuine owned OMP Terminal label previously split into
  `Termina` / `l`. The final signed/minified APK now shows a single-height mode
  button and complete Feed, Terminal and Files menu labels with exactly one
  checked native option. Native selections reached the real Files listing,
  Feed and live Terminal routes; returning to 100% restored ordinary segments.
  Large-font Roborazzi covers label lines, targets and selection transitions.
- **Contract unchanged**: protocol v3, E2EE v2 and committed vectors remain
  frozen. Audit observations do not authorize new wire fields or fallback
  session identities.
