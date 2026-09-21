# Send Buffer — per-client outbound queue, coalescing, eviction, resync

Spec for the relay's per-client outbound queue and what a slow client observes.
Closes `docs/10-spec-gaps.md` P0-2. All line numbers cite `~/Projects/lerdr`
(read-only oracle).

Sources: `internal/transport/sendbuffer.go` (queue),
`internal/transport/ws.go` (hub send paths, eviction, metrics),
`internal/app/server.go` (bootstrap snapshot), `internal/app/pane_watch.go`
(pane ack gate — a *separate* mechanism), `frontend/src/lib/store.ts`
(reconnect behavior).

---

## 1. Capacity constants

| Constant | Value | Source |
|---|---|---|
| `clientOutboundMaxItems` | **64** messages | `sendbuffer.go:9` |
| `MaxOutboundMessageBytes` | **4 MiB** = `4*1024*1024` | `sendbuffer.go:13` |
| `clientOutboundMaxBytes` | = `MaxOutboundMessageBytes` (per-client queue byte budget) | `sendbuffer.go:14` |
| `wsSendTimeout` | **5 s** per `WriteFrame` | `ws.go:22` |
| `wsMaxReadBytes` | 21 MiB **inbound** read limit (unrelated to the queue) | `ws.go:21` |
| `orderedIngressCapacity` | 128 (inbound command queue; separate mechanism) | `ws.go:25` |
| `handlerCapacity` | 32 (inbound handler slots) | `ws.go:24` |

Accounting is over **serialized plaintext JSON bytes** (before E2EE sealing):
`b.bytes += len(item.data)` (`sendbuffer.go:82`).

## 2. Queue semantics (`sendbuffer.go`)

```go
type bufferedMessage struct {        // sendbuffer.go:17-21
    data        []byte               // serialized JSON
    messageType string               // envelope "type"
    replaceable bool
}
```

`pushTyped(data, kind, replaceable)` (`sendbuffer.go:60-85`):

```
if closed:                    → pushRejected
if replaceable && items > 0:
    tail = items.last()
    if tail.replaceable && tail.messageType == kind:
        nextBytes = bytes - len(tail.data) + len(data)
        if nextBytes > maxBytes:  → pushRejected      # replacement too big
        bytes = nextBytes; tail.data = data
                              → pushCoalesced         # tail replaced in place
if len(items) >= 64 || bytes + len(data) > 4MiB:
                              → pushRejected
items += msg; bytes += len(data); signal consumer
                              → pushQueued
```

Hard rules:

- **Coalescing is tail-only**: the incoming message may replace *the queue tail*
  only — never an arbitrary earlier slot (`sendbuffer.go:66-76`). A queue of
  `[pane_content, agents, pane_content]` does NOT merge the two pane frames.
- **Same type required**: tail `messageType` must equal the incoming `kind`.
- **Both must be replaceable**: a non-replaceable tail is never touched, and a
  non-replaceable incoming message never coalesces.
- A coalesced replacement is still subject to the **byte budget** — an oversized
  replacement is rejected (and therefore evicts, §4) even though the slot exists.
- **FIFO pop** (`sendbuffer.go:87-102`): `Pop` blocks on a condvar while empty
  and not closed; after `Close`, queued items still drain, then `Pop` returns
  `false` (`sendbuffer.go:104-111`).
- **No drop-oldest eviction inside the buffer.** The buffer never discards a
  queued message to make room; it only *rejects* the push. Eviction is the
  caller's reaction (§4).

## 3. Replaceable message types

`encodeMessage` marks the type (`ws.go:581-585`):

| Type | Replaceable | Why |
|---|---|---|
| `agents` | ✅ | full snapshot — only latest matters |
| `inventory_status` | ✅ | full snapshot |
| `update_status` | ✅ | full snapshot |
| `app_deploy_status` | ✅ | full snapshot |
| `herdr_status` | ✅ | full snapshot |
| `pane_content` | ✅ | full frame — stale content is useless |
| `pane_unchanged` | ✅ | no-op notice — latest suffices |
| `pane_resync` | ✅ | resync nudge — one pending is enough |
| `pane_delta` | ❌ | **chained on a specific `base_fingerprint`** — dropping or replacing one corrupts the chain (`pane-delta.md` §7) |
| `agent_update` | ❌ | incremental event; test asserts it never coalesces (`sendbuffer_test.go:92-107`) |
| `command_result`, `action_receipt`, `error`, `blocked`, `push_config`, `workspaces`, `activity_history`, `webrtc_*`, `upload_*`, `speech_*`, `push_*`, `question` events, everything else | ❌ | event results / one-shot payloads |

Note the gap against `docs/03-protocol.md`: `workspaces` is **not** replaceable
in the shipped list (only `agents` is, among the big snapshot messages).

## 4. What happens on overflow — eviction is caller-side

`pushTyped` returning `pushRejected` triggers `Hub.Send`/`Broadcast` to evict
the client (`ws.go:380-385, 413-416`):

```
push failed → slowClientEvictions += 1 → removeClient(client)
removeClient: delete(clients, id); cancel(ctx); buf.Close(); onDisconnect(client)   # ws.go:667-679
   └─ ctx cancel → readPump exits → conn.Close(CloseNormal, "")   (code 1000)
```

Same eviction on write failure: `writePump` seals (E2EE) each popped message and
writes with a 5 s `WriteFrame` timeout; seal error or write error → `removeClient`
(`ws.go:348-372`).

**What the client observes:**

- A WS **close 1000 (normal)** — indistinguishable from a clean disconnect. The
  buffer itself never sends a "you were too slow" signal.
- On gateway/WebRTC transports the pipe's `Close` maps to its own close path —
  e.g. stalled gateway frame assembly closes with `CloseGoingAway` + reason
  `"slow"` (`gateway.go:952-960`). Treat any close as "evicted": always
  re-handshake and re-sync.
- Metric: `SlowClientEvictions` (`ws.go:382, 652`).

**The oversized-message cliff:** a *single* serialized message larger than
4 MiB can never be admitted (`sendbuffer.go:78`), so producing one evicts the
client outright — the P0-2 "evicts client" cliff lives in `Hub.Send`, not in
the buffer. Producers are expected to bound message size before enqueue
(`sendbuffer.go:10-12`); nothing in `panedelta`/pane reads enforces it (see
`pane-delta.md` OPEN QUESTION-3).

## 5. Ack semantics — none at buffer level

The send buffer has **no per-message ack, nack, or resend** — it is not a
journal. The only delivery acknowledgement in the protocol is the pane-watch
gate (`pane-delta.md` §7.3): `pane_applied` promotes the pending frame to the
acknowledged base; ≥4 s without it → full `ack_required` resync
(`pane_watch.go:17-23, 170-181`). Do not conflate the two.

## 6. Disconnect → reconnect → resync (client contract)

The connection is the session. On any close:

```
1. Back off: 1 s → 60 s exponential (docs/03-protocol.md §reconnect);
   repeat the full e2ee handshake — auth state survives, channels do not.
2. After handshake the server pushes the bootstrap snapshot, in order
   (sendConnectionSnapshot, server.go:2612-2656):
     1. push_config       (protocol version, capabilities incl.
                           "pane_realtime_delta", "pane_size_lease(_rows)",
                           inventory status, herdr status, hybrid descriptor)
     2. agents
     3. workspaces
     4. activity_history  (last 500)
     5. inventory_status
   Treat every new socket as a fresh session: rebuild all state from this.
3. Pane watches do NOT survive disconnect (watch ctx is bound to client ctx,
   pane_watch.go:76). For each previously watched pane the client sends:
     read_pane{pane_id, lines, format:"ansi",
               content_fingerprint: <stored or "">}
   → pane_content / pane_unchanged (fingerprint hit) → then
     watch_pane{pane_id, lines, interval_ms, format:"ansi",
                content_fingerprint}
4. Pane-size leases do NOT survive disconnect either — ReleaseClient fires on
   disconnect (pane-lease.md §7); re-acquire leases after reconnect.
5. In-flight command_results are lost: request_ids die with the connection.
   Mutations should carry ledger semantics (retry with the same request_id for
   answer_question idempotency — questions.md §6) rather than assuming delivery.
```

Coalescing in-flight during a reconnect is moot — the buffer is destroyed with
the client (`removeClient` → `buf.Close`, `ws.go:673`).

## 7. Server-side delivery details worth mirroring

- **E2EE seal happens at pop time**, not at push time (`ws.go:355-363`) — the
  queue stores plaintext JSON; a message queued before an E2EE session swap
  would seal under the *new* keys. (Reconnects always create a fresh buffer, so
  this is only an invariant to note, not a behavior to emulate blindly.)
- **Broadcast snapshot under a registration barrier**: `Broadcast` /
  `BroadcastPrepared` encode once and fan out to a snapshot of clients taken
  under `register` + `mu` locks (`ws.go:399-441`), guaranteeing a client sees a
  state change either in its handshake snapshot or as a live delta — never
  both, never neither (`ws.go:420-423` comment). The Rust relay needs an
  equivalent ordering guarantee between `sendConnectionSnapshot` and broadcast.
- `SendByID` / `Send` on an unknown client id is a silent no-op (`ws.go:389-397`).
- **Ingress overflow is answered, not silent**: when `orderedIngressCapacity`
  (128) is exceeded the client gets `command_result{ok:false,
  phase:"not_started", error:"Relay is busy; command was not sent"}` —
  keep this distinction from outbound eviction (see `ws.go` ingress path).

## 8. Client requirements (normative)

- Implement reconnect with capped exponential backoff and full re-handshake.
- Treat close 1000 (WS) or going_away (gateway) after a stall as eviction:
  rebuild from bootstrap, don't blame the credential.
- Buffer `command_result`s by `request_id` only for the life of a connection;
  after reconnect resubmit unanswered mutations (same `request_id` for
  idempotent ones).
- Re-issue `read_pane` + `watch_pane` + `lease_pane_size` after reconnect; none
  of them survive.
- Keep inbound processing fast: ack `pane_delta`/`pane_content(ack_required)`
  promptly — the 4 s pane ack gate is the *only* throttling signal the relay
  honors before evicting.

## 9. OPEN QUESTIONS

1. **Slow-client close visibility**: WS eviction surfaces as bare 1000 (no
   reason). Gateway stalls surface `going_away/"slow"`. Decide whether the
   Rust relay should send an explicit close reason/code (e.g. a private 4xxx)
   so the Kotlin client can surface "dropped for being slow" vs "server gone".
2. **`pane_delta` under a full buffer**: a burst of deltas to a stalled client
   fills 64 slots quickly (each delta is tiny but non-coalescing); eviction is
   correct behavior, but note there is no "collapse watch to a resync nudge"
   fallback — the client is simply dropped. Deliberate? (Go's answer: yes —
   slow consumers are disconnected rather than degraded.)
3. **`wsMaxReadBytes` (21 MiB) vs 4 MiB outbound**: inbound messages may be
   ~5× larger than anything outbound — asymmetric by design (uploads). Confirm
   the Kotlin side keeps its own inbound limit consistent with
   `MaxOutboundMessageBytes` for relay-bound commands.
