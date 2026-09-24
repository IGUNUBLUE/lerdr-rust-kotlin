# Phase 5 — wire spec (ratified 2026-09, joint review closed)

Status: **ratified** — app-side verdicts folded in below. Governing rule
(`AGENTS.md`): wire changes land only through a deliberate revision —
never by drift.

Two independent tracks:

- **Track A — new actions** (additive under `protocol: 3`): safe to ship
  piecemeal; unknown actions get `unknown_action` on old servers.
- **Track B — transport upgrades** (codec/compression/chunk encoding):
  negotiated; both ends must agree before bytes change.

## 0. Capability negotiation revision

Today `push_config.capabilities` is a flat `Vec<String>` of feature
names — keep the mechanism, extend the vocabulary, and add a client leg
so negotiation is symmetric:

```
server → push_config { ..., capabilities: [..., "focus", "pane_search",
                 "pane_links", "layout", "convo_sub", "frame_zstd",
                 "upload_binary"] }
client → {"type":"client_caps", "protocol":3,
          "capabilities":[...], "preferred_inner_codec":"json"|"binary-v1"}
```

`client_caps` is optional for the server and **emitted unconditionally**
by the app as the first post-handshake frame; old relays answer
`unknown_action`, which the app ignores. The relay replies with a
`caps_update` carrying the **server's advertised list** — the symmetric
declaration, so the client learns the live set even if it connected
before the latest flip. A capability is live only when present on
**both** lists. Unilateral changes: each side emits
`caps_update {capabilities:[...]}` if support flips mid-session (e.g.,
herdr restarted older). `caps_update` is a new outbound type — app-side
it decodes to `UnknownServerMessage` harmlessly even on old parsers.

**Q1 verdict (app): `client_caps` post-handshake — `e2ee_client_hello`
untouched.** The hello is hand-built JSON in `E2EEHandshake.kt` anchored
by `crypto.handshake.*` golden vectors; any new field invalidates
vectors and strict old parsers. A caps frame inside the encrypted
channel is additive, and nothing capability-gated can arrive before it
anyway.

**Q2 verdict (app): confirmed.** `ServerMessageCodec` already maps
unknown `type` to `UnknownServerMessage(type, fields)`.

## 1. Track A — new actions (app-ranked)

All follow catalog conventions: flat JSON, `protocol:3`, `request_id`,
`target` (`TargetRef` — `pane_id`/`workspace_id`/`agent_session_id` as
applicable), `command_result` for reads, `action_receipt` progression
for mutations. Marks: **[M]** mutating, **[C]** coordinated per-pane,
**[A]** audited.

### 1.1 Focus family — notification tap → desktop jump

```json
{"type":"focus_pane",      "target":{"pane_id":"wE:p1",...}}      [M]
{"type":"focus_tab",       "target":{"pane_id":"wE:p1",
                                     "tab_id":"wE:p1:t2",...}}    [M]
{"type":"focus_workspace", "target":{"workspace_id":"wE",...}}    [M]
{"type":"focus_agent",     "target":{"agent_session_id":"wE:a3"}} [M]
```

Maps to herdr `pane.focus`/`tab.focus`/`workspace.focus`/`agent.focus`.
Reply: `action_receipt` (confirmed when herdr acks). Failure:
`herdr_error` with upstream message. Declared capability `"focus"`.

**`TargetRef` addition:** `workspace_id` does not exist on `TargetRef`
today — `focus_workspace` adds it additively (empty string elsewhere;
old decoders ignore it).

### 1.2 `pane_search` — server-side find over full scrollback

```json
req  {"type":"pane_search","target":{...},"query":"panic",
      "direction":"forward|backward",
      "cursor":{"row":<u32>,"col":<u16>},
      "previous":{"start":{...},"end":{...}}|null}
resp command_result {"matches":[{"start":{"row","col"},
      "end":{"row","col"}}], "content_revision":<u64>,
      "total":<u64>, "current":<u64>, "current_global":<u64>}
```

Maps to herdr `pane.copy_search`. The relay injects the pane's current
upstream `content_revision` as the fence — app never sends it (the
served watch watermark is authoritative relay-side; an unobserved
watermark probes via unfenced `pane.copy_motion`, `stale_content`
refusals re-probe and retry once). Empty `matches` = no hit. The
`total`/`current`/`current_global` fields are upstream match-position
metadata, additive beyond the base shape (S13 implementation).

### 1.3 `pane_selection_read` — read an arbitrary range

```json
req  {"type":"pane_selection_read","target":{...},
      "anchor":{"row","col"},"cursor":{"row","col"}}
resp command_result {"text":"...", "content_revision":<u64>}
```

Capability `"pane_search"` (same family). Read-only.

### 1.4 Pane links — resolve (preview) / activate (open on desktop)

```json
req  {"type":"pane_link_resolve","target":{...},
      "row":<u16>,"col":<u16>}
resp command_result {"regions":[{"start":{"row","col"},
      "end":{"row","col"}}...]}                    # cell bounds

     {"type":"pane_link_activate","target":{...},
      "row":<u16>,"col":<u16>}                     [M,C]
resp command_result {"handled":<bool>, "url":"https://..."|null}
```

`row`/`col` are **viewport coordinates** of the last served frame —
relay translates to herdr's `{viewport_row, col, content_revision,
offset_from_bottom}`. Coordinates are resolvable only inside the live
viewport; a scrolled-back `row`/`col` refers to scrollback space and
resolve returns empty `regions`.

**Implementation correction (S13):** herdr 0.9.1's `pane.link.resolve`
exposes **cell regions only** — it does not return the URL. The URL
surfaces on `pane.link.activate` (`{handled, url}`). So resolve = "is
there a link here / where does it span" (highlight affordance), and
activate = open it on the desktop browser and report what it was. The
real value of the pair is **OSC8 hidden links** — terminals render
clickable text whose URL is invisible in the plain text, so the app
cannot regex it out of served lines. Resolve is read-only; activate is
mutating. Capability `"pane_links"`.

### 1.5 Layout — export/apply

```json
req  {"type":"layout_export","target":{"tab_id":"..."}|{"pane_id":"..."}}
resp command_result {"root":<LayoutNode>}

     {"type":"layout_apply","root":<LayoutNode>,
      "workspace_id":null,"tab_id":null,"tab_label":null,
      "focus":false}                                [M,C,A]
```

`LayoutNode` = herdr's shape verbatim:
`{"type":"pane","pane_id"?,"command"?,"cwd"?,"env"?,"label"?}` |
`{"type":"split","direction":"right|down|up|left","ratio":<f32>,
"first":<node>,"second":<node>}`. Capability `"layout"`.

Open Q3 — app: export shape verbatim from herdr, or do you want a
simplified app-oriented tree (flatter, no pane internals)?

## 2. Track B — transport upgrades (negotiated)

### 2.1 Binary inner codec — **dropped**

The Phase-5 draft proposed a CBOR inner payload (`binary-v1`, ~37%
theoretical overhead win) negotiated via `preferred_inner_codec` in
`client_caps`. **Removed from the plan**: `frame_zstd` captured the
real win on the bulky path (~10× on realistic `pane_content`), and the
remaining frames are small control chatter where a second codec would
buy marginal bytes against a doubled fixture/conformance surface.
`preferred_inner_codec` is no longer modeled — a client that still
sends it is ignored like any unknown field. JSON remains the single
inner encoding.

### 2.2 zstd frame compression (`frame_zstd`)

Applied to `pane_content`/`pane_resync` payloads only (deltas already
compress well). New optional field on those messages:
`"encoding":"zstd"` + base64(zstd(payload-json)). Only emitted when the
capability negotiated both ways; otherwise plaintext JSON. Keeps the
schema — `encoding` absent ⇒ JSON.

### 2.3 Conversation subscriptions (`convo_sub`)

Push instead of `get_conversation_history` polling:

```json
{"type":"subscribe_conversation","target":{"pane_id"|"agent_session_id"}}
{"type":"unsubscribe_conversation","target":{...}}
server → {"type":"conversation_update","target":{...},
          "generation":<u64>,"messages":[...],"reset":false}
```

Coalescible (like `agents`/`workspaces` snapshots); `reset:true` when
the history was rebuilt and the client should drop its cache. Honors
the existing generation/ref-staleness rules.

**Q5 verdict (app): per-pane subscribe/unsubscribe.** The app consumes
one pane's conversation at a time (Feed lifecycle); bulk subscription
wastes bandwidth on feeds never opened.

### 2.4 Upload binary chunks (`upload_binary`)

`upload_chunk` is base64-in-JSON (+33%). While `upload_binary` is live
on both capability lists (§0), `upload_begin` reports
`"chunk_encoding":"binary"` in its result payload and chunks travel as
**raw binary plaintext inside the E2EE channel** — the outer envelope
(AES-GCM, per-direction BE64 sequence discipline, the negotiated outer
codec) is unchanged. The *decrypted* chunk payload is:

```text
[0x03][upload_id: 32 ASCII bytes][chunk_seq: BE64 i64][raw bytes…]
```

- The inner type byte `0x03` distinguishes a binary chunk from JSON
  plaintext (always `{`, 0x7B). It is not an outer-codec revision —
  `0x02` remains the outer binary codec header.
- `upload_id` carries the 32-char base64url opaque id (192 bits,
  `upload_begin`'s minted form) **verbatim as ASCII** — the begin result
  hands the client that exact string, so the header is fixed-width and
  self-describing with no relay-side id mapping. (The original sketch
  said `upload_id:16`; truncating or hashing would have needed a second
  lookup table to save 16 bytes per ≤256 KiB chunk.)
- `chunk_seq` is the same global counter domain as JSON
  `upload_chunk.sequence`.
- Fields the JSON form carries and the carrier omits are anchored
  server-side: `target` and `file_index` come from the staged session
  (the client cannot claim either), and `sha256` is measured on receipt
  — the AES-GCM envelope already authenticates the bytes. Ordering,
  dedup, size, capacity, and digest verification run the identical
  machinery as JSON chunks; mixed JSON/binary carriers within one
  upload share the sequence counter and interleave freely while order
  holds.
- Acks stay JSON: `upload_chunk_result`/`upload_*_result` reply shapes
  are unchanged, with an omitted/empty `request_id` on binary-chunk acks
  (the carrier has no correlation field; `next_sequence` correlates).
  `upload_begin`, `upload_finish`, `upload_cancel` remain JSON-only.
- A `0x03` frame from a client that never announced the capability (or
  after a mid-flight `caps_update` retraction) answers
  `capability_unsupported` like a gated action — the session is kept.
  A malformed `0x03` (truncated header, non-base64url id) closes the
  connection exactly like non-JSON plaintext does.
- Non-negotiated clients keep the base64 `upload_chunk` form unchanged.

## 3. Implementation order (ratified)

1. Caps revision (§0) — foundation, both tracks key off it.
2. Track A actions in app-rank order: focus → search/selection → links
   → layout. Each ships independently behind its capability.
3. Track B: `convo_sub` → `frame_zstd` → `upload_binary`.
   `inner_codec_binary` **deferred** pending measured wins from
   `convo_sub` + `frame_zstd` (app recommendation).

Relay-side mapping: each action routes through the coordinator like
existing `read_pane`/`send_text`; read fences reuse `pane_read_fresh`'s
upstream `content_revision` watermark (S10).
