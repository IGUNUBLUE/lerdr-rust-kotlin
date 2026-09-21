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
