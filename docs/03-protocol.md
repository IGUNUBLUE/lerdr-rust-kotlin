# 03 — Protocol specification (extracted)

Normative source: `internal/protocol/protocol.go`, `internal/transport/e2ee.go`,
`internal/transport/ws.go`, `internal/transport/sendbuffer.go`,
`internal/app/pane_watch.go` in `IGUNUBLUE/lerdr` @ v0.26.3. This document
is the contract every implementation must satisfy byte-for-byte.

## Layers

```
WebSocket (subprotocol "herdr-e2ee-v2")
└── E2EE frames (AES-256-GCM, per-direction keys, monotonic sequences)
    └── JSON messages (protocol v3): actions in, events out
```

## 1. E2EE handshake — `herdr-e2ee-v2`

All base64 is `RawURLEncoding` (no padding). All HMAC is HMAC-SHA256.

### Step 1 — client hello (plaintext JSON)

```json
{
  "type": "e2ee_client_hello",
  "version": 2,
  "auth_kind": "credential" | "invitation",
  "auth_id": "<credential-or-invitation id, ≤128 chars>",
  "auth_version": <u64>,
  "locale": "<bcp47, ≤32 chars>",
  "nonce": "<b64, 32 bytes random>",
  "public_key": "<b64, 65 bytes, UNCOMPRESSED P-256 point>",
  "proof": "<b64, 32 bytes>"
}
```

```
binding       = "herdr-e2ee-v2 auth\x00" ‖ kind ‖ 0x00 ‖ auth_id ‖ 0x00
                ‖ decimal(auth_version) ‖ 0x00
client_proof  = HMAC(secret, "herdr-e2ee-v2 client\x00" ‖ binding
                ‖ client_nonce ‖ client_public_bytes)
```

`secret` = the 32-byte pairing credential or invitation secret known to
both sides. Server rejects structurally-invalid or unauthenticated hellos;
failed proofs are recorded (invitation attempt limiting).

### Step 2 — server hello (plaintext JSON)

Server generates ephemeral P-256 keypair + 32-byte nonce:

```json
{ "type": "e2ee_server_hello", "version": 2,
  "nonce": "<b64 32B>", "public_key": "<b64 65B uncompressed>",
  "proof": "<b64 32B>" }
```

```
transcript    = binding ‖ client_nonce ‖ client_public ‖ server_nonce
                ‖ server_public
server_proof  = HMAC(secret, "herdr-e2ee-v2 server\x00" ‖ transcript)
key_salt      = HMAC(secret, "herdr-e2ee-v2 key\x00"    ‖ transcript)
shared        = ECDH(server_private, client_public)     # P-256, raw 32B
c2s_key       = HKDF-SHA256(shared, salt=key_salt, info="herdr-e2ee-v2 c2s", 32)
s2c_key       = HKDF-SHA256(shared, salt=key_salt, info="herdr-e2ee-v2 s2c", 32)
```

### Step 3 — client finish (first encrypted frame)

`{"type":"e2ee_client_finish","version":2}` sealed with c2s key, seq 0.
This both proves the handshake and lets the server commit the credential /
consume the invitation **atomically after** authentication.

### Step 4 — server finish (encrypted)

```json
{ "type":"e2ee_server_finish", "version":2,
  "device_id":"...", "credential_id":"...", "role":"controller|reader",
  "locale":"...", "credential_version":<u64>,
  "credential_secret":"<b64 32B — only when invitation issued a credential>" }
```

Handshake timeout: 10 s.

### Session frames

```
nonce = 4 zero bytes ‖ BE64(sequence)
AAD   = "herdr-e2ee-v2 " ‖ direction ‖ 0x00 ‖ BE64(sequence)
        direction ∈ {"c2s","s2c"} — receiver's perspective differs!
seq   = 0,1,2… per direction; must arrive strictly in order;
        max = 2^53−1
cipher = AES-256-GCM(key, nonce).seal(plaintext, AAD)
```

**Frame codecs** (negotiated via WS message type — text vs binary):

| Codec | Format |
|---|---|
| `CodecJSON` | `{"type":"e2ee","version":2,"sequence":<u64>,"ciphertext":"<b64>"}` |
| `CodecBinary` | `[0x02, 0x00, BE64 seq, ciphertext…]` — 10-byte header |

Both must be supported; selection rides the WS binary/text distinction.

## 2. Message envelope (inside the encrypted channel)

Client→server (`Inbound`): flat JSON with `type`, `protocol: 3`,
`request_id` (for correlated replies), optional `target` (`TargetRef`:
`server_session_id`, `pane_id`, `terminal_id`, `generation`,
`agent_session_id`), plus action-specific fields (see catalog).

Responses: `{"type":"ok"|"error"|"action_receipt"|"command_result"…}`
correlated by `request_id`. Mutations additionally emit `action_receipt`
with `phase` progression `prepared → awaiting_evidence → confirmed`
(`failed_before_dispatch`, `dispatched_unknown` on failure).

## 3. Action catalog (authoritative, 70 actions)

Read-only unless marked **[mutating]**; **[coordinated]** = serialized per
pane through the coordinator; **[audited]** = journaled.

```
get_activity, device_list, get_conversation_history, workspace_file,
workspace_git_diff, workspace_git_status, workspace_tree, worktree_list,
read_pane, watch_pane, unwatch_pane, pane_applied, refresh_agents,
list_directories, list_slash_commands, qr_code, push_open_ref,
push_policy_get, cancel_speech, speak_text, speech_voices_list,
check_update, webrtc_offer, webrtc_ice, webrtc_close,

acknowledge_pane [M,C],   agent_clear [M,C,A],     agent_rename [M,C,A],
agent_restart [M,C,A],    agent_start [M,C,A],     agent_stop [M,C,A],
answer_question [M,C,A],  clarify_question [M,C,A],navigate_question [M,C,A],
lease_pane_size [M,C],    release_pane_size [M,C], respond [M,C,A],
send_keys [M,C,A],        send_input [M,C,A],      send_secret [M,C,A],
send_text [M,C,A],        submit_prompt [M,C,A],   tab_reorder [M,C,A],
workspace_close [M,C,A],  workspace_create [M,C,A],workspace_rename [M,C,A],
workspace_reorder [M,C,A],worktree_create [M,C,A], worktree_open [M,C,A],
worktree_remove [M,C,A],

clear_activities [M],     copy_agent_response [M], deploy_app_update [M],
create_device_invitation [M,A], install_update [M — no protocol check],
push_policy_set [M],      push_snooze [M],         push_subscribe [M],
push_test_device [M,A],   push_unsubscribe [M],    push_viewed_pane [M],
register_app_origin [M],  rename_device [M,A],     reset_devices [M,A],
revoke_device [M,A],      speech_voice_install [M,A],
speech_voice_remove [M,A],
upload_begin [M,A],       upload_cancel [M,A],     upload_chunk [M,A],
upload_finish [M,A],
```

Unknown actions → `unknown_action` error. Mutations require
`protocol: 3` except `install_update`. Role `reader` may only send
read-only actions — enforced server-side.

## 4. Outbound events (server→client)

| Type | Payload | Notes |
|---|---|---|
| `push_config` | VAPID key, host, versions, capabilities, `herdr_status`, inventory, agent profiles, `hybrid` descriptor | first message after handshake |
| `agents` / `agent_list` / `agent_update` | merged inventory entries | replaceable (coalesce) |
| `workspaces` / `workspace_list` / `tab_list` / `pane_list` | topology snapshots | replaceable |
| `session_snapshot` / `inventory_status` | bootstrap + refresh state | |
| `pane_content` | `{target, lines[], columns, rows, fingerprint, ack_required}` | full frame; replaceable |
| `pane_delta` | `{target, base_fingerprint, frame_fingerprint, segments[], ack_required}` | **never** coalesced — chained |
| `pane_resync` | full frame after drift | replaceable; drops pending coalescables |
| `pane_probe` | cheap probe result | |
| `question` / `attention` | `QuestionInteraction` / `attention_kind` | structured input requests |
| `activity` / `activity_history` | journal entries | |
| `command_result` | correlated `{request_id, data}` | unary actions |
| `action_receipt` | `receipt{action_id, phase, error?}` | mutation lifecycle |
| `error` | `ApiError{code, args}` | bounded: ≤8 args, ≤32-char keys, ≤256-char strings |
| `update_status` / `app_deploy_status` | update/deploy progress | |
| `speech_voices` / `speech_voice_*` | TTS catalog/results | |
| `webrtc_offer/answer/ice/closed` | DataChannel signaling | |
| `upload_*` progress | chunk acks | |

## 5. Realtime pane watch — the correctness boundary

```
client → {"type":"watch_pane", target:{…}}
server → pane_content (fingerprint F0, ack_required)         # base frame
client → {"type":"pane_applied", fingerprint:F0}             # ack
server → pane_delta {base_fingerprint:F0, frame_fingerprint:F1,
                     segments:[{copy_start,copy_lines}|{text}…], ack_required}
client → pane_applied F1                                     # ack
…repeat per change tick (≤4 Hz)…
```

- **Fingerprint**: 16-hex-char digest over typed field stream (v0.26.3+:
  sha256 streaming; earlier: marshaled array — opaque to clients, equality
  only).
- **Ack gate**: server holds at most one unacked `ack_required` frame;
  pending expires at ~4 s → full `pane_content` resync with `ack_required`.
- **Delta codec**: `segments[]` is a 3-line-anchor copy format, NOT generic
  ops — `{copy_start,copy_lines}` copies lines from the base frame, `{text}`
  inserts literal. `copy_lines` alone = continue-from-last; `text` alone =
  insert. Byte-exact semantics in `docs/specs/pane-delta.md`; vectors in
  `fixtures/pane/pane.delta.json` (incl. the SplitAfter trailing-empty-line
  trap).
- **Client apply**: apply segments to its copy of the frame, recompute
  nothing — the server sends the new fingerprint; store it as next base.
- **Coalescing set** (8 replaceable types, verified in `ws.go`):
  `agents`, `inventory_status`, `update_status`, `app_deploy_status`,
  `herdr_status`, `pane_content`, `pane_unchanged`, `pane_resync`.
  `pane_title`, `pane_delta`, `action_receipt` are NOT replaceable. Buffer
  overflow
  *rejects* the incoming push (`queueFull`); queued entries are never
  dropped — eviction means disconnecting the client, not dropping data.
  `pane_content`→`pane_unchanged` at coalesce time; `pane_resync` drops
  anything pending.
- **Size lease**: while watching, the client may hold a `lease_pane_size`
  (cols 40–240, rows 10–120, TTL ~120 s, renewed ~30 s); release on hide.
  The shared pane physically resizes — a lease means "I am the viewport".

## 6. Transport availability matrix

| Path | How | E2EE |
|---|---|---|
| Direct WSS | Tailscale Serve / LAN / Cloudflare tunnel URL in relay config | yes, in-band |
| Gateway | `wss://gateway` registers under `relay_id` derived from relay key; gateway copies opaque frames | yes — gateway sees ciphertext only |
| WebRTC direct | `herdr-dc-v1` DataChannel negotiated inside a gateway session; takes over after first real message; gateway dropped 10 s later | its own handshake inside DC |

Reconnect: exponential backoff 1 s→60 s, reset on wake, keepalive pings as
health check, E2EE handshake on every new socket (sessions are per-conn).

## 7. Compatibility notes for implementers

- `protocol` field is an **integer** `3`, not a string.
- `TargetRef` fields may be empty strings; `generation` guards staleness —
  compare full refs, never pane_id alone.
- Error `args` values are bounded ints/strings/bools only (2^53 range).
- `pane_applied`/`acknowledge_pane`/`unwatch_pane`/`refresh_agents` are
  read-class actions — readers can drive a watch.
- Binary codec exists on the **E2EE frame** layer today; inner payloads
  stay JSON. A binary inner codec was evaluated in Phase-5 and dropped
  (`docs/13` §2.1).
