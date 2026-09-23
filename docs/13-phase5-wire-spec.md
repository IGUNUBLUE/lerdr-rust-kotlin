# Phase 5 — wire spec (DRAFT for joint review)

Status: **proposal, not ratified**. Nothing in this file lands until both
relay and app sign off. Governing rule (`AGENTS.md`): wire changes land
only through a deliberate revision — never by drift.

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

`client_caps` is optional and sent once as the first post-handshake
frame. A capability is live only when present on **both** lists.
Unilateral changes: each side emits `caps_update {capabilities:[...]}`
if support flips mid-session (e.g., herdr restarted older).

Open Q1 — app: is a separate `client_caps` frame acceptable, or do you
prefer folding client capabilities into `e2ee_client_hello` (that is an
e2ee v3 handshake change — bigger blast radius)?

Open Q2 — app: unknown `type` values from server→client must already be
tolerated (you run `ignoreUnknownKeys`)? `caps_update` relies on it.

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

### 1.2 `pane_search` — server-side find over full scrollback

```json
req  {"type":"pane_search","target":{...},"query":"panic",
      "direction":"forward|backward",
      "cursor":{"row":<u32>,"col":<u16>},
      "previous":{"start":{...},"end":{...}}|null}
resp command_result {"matches":[{"start":{"row","col"},
      "end":{"row","col"}}], "content_revision":<u64>}
```

Maps to herdr `pane.copy_search`. The relay injects the pane's current
upstream `content_revision` as the fence — app never sends it (the
served watch watermark is authoritative relay-side). Empty `matches` =
no hit. Capability `"pane_search"`. Read-only.

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
resp command_result {"url":"https://..."}          # or {"url":null}

     {"type":"pane_link_activate","target":{...},
      "row":<u16>,"col":<u16>}                     [M,C]
```

`row`/`col` are **viewport coordinates** of the last served frame —
relay translates to herdr's `{viewport_row, col, content_revision,
offset_from_bottom}`. Resolve is read-only (app can show a preview/copy/
QR affordance); activate opens it in the desktop browser. Capability
`"pane_links"`.

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

### 2.1 Binary inner codec (`inner_codec_binary`)

E2EE already rides `CodecBinary` on WS binary frames; the *inner*
payload stays JSON today (~37% overhead win available). Proposal:
`binary-v1` = length-prefixed CBOR/MsgPack carrying the same document
model — field-for-field identical semantics, JSON stays the reference
encoding for fixtures and docs. Negotiated via `preferred_inner_codec`
in `client_caps`; server echoes the chosen codec in `caps_update`.
Falls back to JSON whenever either side declines.

Open Q4 — app: MsgPack (kmp-friendly libs) or CBOR? Or defer — this is
the largest single item and benefits least if watch deltas already
compress well.

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

Open Q5 — app: per-pane subscribe or a single `subscribe_conversations`
covering all watched panes (fewer frames)?

### 2.4 Upload binary chunks (`upload_binary`)

`upload_chunk` today is base64-in-JSON (+33%). Proposal: when
negotiated, `upload_begin` returns `"chunk_encoding":"binary"` and
chunks travel as **raw binary WS frames**:
`[0x03][upload_id:16][BE64 seq][bytes…]` — a new E2EE frame type `0x03`
inside the encrypted channel (same seq discipline). `upload_finish`
unchanged.

## 3. Implementation order (proposal)

1. Caps revision (§0) — foundation, both tracks key off it.
2. Track A actions in app-rank order: focus → search/selection → links
   → layout. Each ships independently behind its capability.
3. Track B by ROI: `convo_sub` (kills polling) → `frame_zstd` (cheap,
   big frames) → `upload_binary` (narrow path) → `inner_codec_binary`
   (largest, defer if 2+3 land the wins).

Relay-side mapping: each action routes through the coordinator like
existing `read_pane`/`send_text`; read fences reuse `pane_read_fresh`'s
upstream `content_revision` watermark (S10).
