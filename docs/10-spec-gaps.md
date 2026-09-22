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

### Still open

- **Push queue persistence** — `queue.json` in-memory only (see above).
- **Conversation history** — readers in flight (rs-conversation);
  `get_conversation_history` still returns the oracle's browserless
  failure until the router arm re-points.
- **Shadow-diff harness** — in flight (rs-shadow-diff): `lerdr-shadow`
  scripted client + scenario runner diffing normalized frame streams
  across both relays on one Herdr socket; Phase-3 exit gate.
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
