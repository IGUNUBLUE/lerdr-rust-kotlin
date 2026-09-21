# Pane Delta — `pane_delta` / `pane_content` / `pane_applied` / `pane_resync`

Spec for the pane streaming delta codec and the watch ack chain. Closes
`docs/10-spec-gaps.md` P0-1. All line numbers cite `~/Projects/lerdr` (Go
reference; read-only oracle).

Sources: `internal/panedelta/delta.go` (algorithm),
`internal/app/pane_watch.go` (watch lifecycle, sender policy),
`internal/app/server.go` (fingerprints, dispatch),
`internal/transport/sendbuffer.go`, `internal/transport/ws.go` (coalescing),
`frontend/src/lib/store.ts` (client-side apply — the released contract).

---

## 1. Line model

Both sides model pane content as **lines that retain their `\n`**
(`strings.SplitAfter(content, "\n")`, `delta.go:19-20`).

| Input | Lines (Go `SplitAfter`) | Notes |
|---|---|---|
| `""` | `[""]` | one empty line |
| `"a\nb"` | `["a\n", "b"]` | no trailing newline |
| `"a\nb\n"` | `["a\n", "b\n"]` | **no phantom empty line** |
| `"a\n\n"` | `["a\n", "\n"]` | interior blank line kept |
| `"a\r\nb"` | `["a\r\n", "b"]` | `\r` stays at end of line — CRLF is not normalized |

> CRITICAL PARITY NOTE. The released JS client does **not** use `SplitAfter`
> semantics to *index* the previous buffer. It builds a **boundary table**:
> `boundaries = [0] ++ [i+1 for each '\n' at index i] ++ [len(previous)]`
> (`store.ts:3118-3122`). For `"a\nb\n"` this yields `[0, 2, 4, 4]` — a
> duplicate trailing boundary — so `copy_lines` can legally range up to
> `len(boundaries)-1 = count("\n")+1`. Go `Apply` rejects that same segment
> (`end > len(lines)`, `delta.go:75`). The relay emits such a segment for the
> "content unchanged, metadata changed" case (`pane_watch.go:340-343`), so a
> client implementing strict Go `Apply` would resync on every unchanged-pane
> delta. **Implement the boundary-table semantics (§6), not `SplitAfter`
> indexing.** Flagged as OPEN QUESTION-1 (whether to "fix" the relay or bless
> JS semantics — treat JS as normative until decided).

## 2. Segment codec

```json
Segment = { "copy_start"?: int, "copy_lines"?: int, "text"?: string }
```

Go struct `Segment{CopyStart, CopyLines, Text}` with `omitempty` on all three
fields (`delta.go:10-14`). Wire rules that follow:

- **Copy segment**: `copy_lines` present and `> 0`. `copy_start` omitted ⇔ `0`.
  `{"copy_lines":4}` or `{"copy_start":1,"copy_lines":4}`.
- **Literal segment**: `copy_lines` absent (or `0`, but `omitempty` means the
  encoder never emits `0`), `text` present. `{"text":"six\n"}`.
- `{}` is never emitted (Build only appends a literal when `end > literalStart`,
  `delta.go:31-35`; copy segments always have `CopyLines ≥ 3`).

Client decode rule (matches `store.ts:3128-3139`): **if `copy_lines` is present
(not null/undefined) it is a copy segment**, validated per §6; otherwise `text`
must be a string. A present-but-`0`/non-integer `copy_lines` is malformed →
reject the whole delta.

## 3. Build algorithm (sender)

Constants (`delta.go:5-8`): `minimumCopyLines = 3`, `maxCandidates = 64`.

Pseudocode of `Build(previous, current) []Segment` (`delta.go:18-59`):

```
previousLines = SplitAfter(previous, "\n")
currentLines  = SplitAfter(current,  "\n")

# 1. Index every 3-line anchor in previous, first-64-wins per key.
matches = {}                                   # lineKey -> [prevIndex]
for i in 0 ..= len(previousLines)-3:
    key = previousLines[i..i+3]                # [3]string, lines keep "\n"
    if len(matches[key]) < 64: matches[key] += i

# 2. Greedy left-to-right scan of current.
segments   = []
literal    = 0                                 # literalStart index
ci         = 0                                 # currentIndex
while ci < len(currentLines):
    if ci + 3 > len(currentLines): break       # can't anchor last <3 lines
    bestStart, bestLen = 0, 0
    for pi in matches.get(currentLines[ci..ci+3], []):
        m = matchingLines(previousLines, currentLines, pi, ci)
        if m > bestLen: bestStart, bestLen = pi, m   # strict >: first wins ties
    if bestLen < 3: ci += 1; continue
    if ci > literal: segments += {text: join(currentLines[literal..ci])}
    segments += {copy_start: bestStart, copy_lines: bestLen}
    ci += bestLen; literal = ci
if len(currentLines) > literal:
    segments += {text: join(currentLines[literal..])}
return segments
```

`matchingLines` counts forward-equal lines from both positions, bounded by both
arrays (`delta.go:92-99`). Properties an implementer must preserve:

- Anchors are **exact 3-consecutive-line equality**; a single changed line kills
  every anchor that overlaps it (see Example 2 — the whole frame goes literal).
- At most **64 candidate positions** per anchor key; earlier indices win when a
  repeated block exceeds the cap (`delta.go:24-26` — the first 64 appended are
  kept).
- The scan is **greedy and non-overlapping in `current`**, but a `previous`
  region may be copied more than once (a repeated block in `current` re-references
  the same `previous` range).
- Longest match wins; on equal length the **lowest previous index** wins
  (`matched > bestLines` strict, `delta.go:44-46`).

## 4. Efficiency gate

`Efficient(segments, current) bool` (`delta.go:61-67`):

```
literalBytes = Σ len(segment.Text)              # bytes, not runes
return literalBytes + 64*len(segments) < len(current)*3/4
```

- Byte lengths throughout (`len(string)` in Go = UTF-8 bytes).
- Each segment is charged a flat **64 bytes** regardless of its actual JSON size.
- Strict `<` on integer arithmetic: `len(current)*3/4` truncates toward zero.
- Deliberate slack: a delta may carry up to ~3/4 of the full frame in literals
  and still ship, because a full frame costs `content` + the same metadata.

## 5. `Apply` semantics (Go reference — relay-side verifier)

`Apply(previous, segments) (string, bool)` (`delta.go:69-86`):

```
lines = SplitAfter(previous, "\n")
out = ""
for seg in segments:
    if seg.CopyLines > 0:
        end = seg.CopyStart + seg.CopyLines
        if seg.CopyStart < 0 || end < seg.CopyStart || end > len(lines): FAIL
        out += join(lines[seg.CopyStart .. end])
    else:
        out += seg.Text
return out
```

Rejects: negative `copy_start`, integer-overflow `end`, and `end` beyond the
`SplitAfter` line count. `copy_lines ≤ 0` is treated as a **literal** segment
(falls through to `Text`), not an error — differs from the JS rule, see §6.

## 6. Client-side apply (Kotlin contract — normative)

Mirror `store.ts:3113-3142`. Reject the whole delta → forced `read_pane` on any
violation.

```
boundaries = [0]
for each index i where previous[i] == '\n': boundaries += i+1
boundaries += len(previous)            # may duplicate the last boundary
                                       # (trailing '\n') — intentional

chunks = []
for seg in segments:                    # segments must be a JSON array
    if seg has "copy_lines" (any non-null value):
        copy_lines must be an integer > 0            else FAIL
        copy_start = seg.copy_start ?? 0
        copy_start must be an integer >= 0           else FAIL
        copy_end = copy_start + copy_lines
        if copy_end > len(boundaries) - 1: FAIL
        chunks += previous[ boundaries[copy_start] : boundaries[copy_end] ]
    else if seg has "text" of type string:
        chunks += seg.text
    else: FAIL
return concat(chunks)
```

Differences from Go `Apply` (deliberate — the JS client is the deployed contract):

| Case | Go `Apply` | Client apply |
|---|---|---|
| `copy_lines = count("\n")+1` on trailing-`\n` previous | **reject** (end > lines) | **accept** (dup boundary) — the relay emits exactly this |
| `copy_lines: 0` present | literal (writes `text`) | reject |
| `copy_start` out of range vs `len(lines)` | reject at copy time | `boundaries[copy_start]` indexes the table — reject via `copy_end` bound only |

`copy_start` beyond `len(boundaries)-1` always fails through `copy_end`, so no
separate lower bound is needed past `>= 0` (`store.ts:3132-3134`).

### 6.1 Metadata-only delta fast path (`store.ts:1577-1585`)

If `stored_fingerprint == msg.base_fingerprint` **and**
`base_fingerprint == content_fingerprint` **and** `segments` is `null` or `[]`,
keep the existing content verbatim (do not apply). This accepts "released
relays" that encode metadata-only deltas as `segments: null` (comment at
`pane_watch.go:338-339`, client code at `store.ts:1578-1582`). A `[]`/`null`
segment list under a real Go relay never appears with equal fingerprints via
Build — the tiny-delta path emits `[{copy_lines: N+1}]` instead.

## 7. Wire messages

### 7.1 `pane_delta` (server → client)

Produced by `paneDeltaResponse` (`pane_watch.go:353-363`): **every field of the
pane response except `content`**, plus:

| Field | Value |
|---|---|
| `type` | `"pane_delta"` |
| `pane_id`, `target` | pane id + `TargetRef` echo |
| `base_fingerprint` | `content_fingerprint` of the client's acknowledged base frame |
| `content_fingerprint` | fingerprint of the NEW full frame (the post-apply target) |
| `segments` | `Segment[]` per §3 |
| `format`, `truncated`, `viewport_only`, `viewport_rows`, `no_echo`, `no_echo_prompt`, `resize_settling`, `attention_kind`, `prompt`, `command`, `options`, `interaction`, `question_layout` | same semantics as `pane_content` — **metadata may change without content changing** |

`pane_delta` **never carries `ack_required`** and **never coalesces** in the
send buffer (`ws.go:582-584` omits it from the replaceable list — deltas are
chained on a specific base).

### 7.2 `pane_content` (server → client)

Full frame: `content` + `content_fingerprint` + all metadata fields. Carries
`ack_required: true` whenever the watch is (re)building its base
(`pane_watch.go:334, 349`). `read_pane` responses are `pane_content` too and do
**not** carry `ack_required` (`server.go:766-774`; `unchangedPaneResponse` may
answer `pane_unchanged` instead, `server.go:2933-2946`).

### 7.3 `pane_applied` (client → server)

```json
{"type":"pane_applied", "pane_id":"...", "content_fingerprint":"<the fingerprint of the frame just applied>", ...target}
```

Client sends it (`store.ts:1645-1654`):
- for **every `pane_delta`** that applied successfully (unconditional — deltas
  are implicitly gated even without `ack_required`);
- for `pane_content` **only when `ack_required: true`**.

Server handling (`pane_watch.go:267-289`):
- `fingerprint == pending.contentFingerprint` → pending promotes to
  `acknowledged`; watch resumes.
- `fingerprint == acknowledged.contentFingerprint` → ignore (dup/late ack).
- Otherwise → send `pane_resync` nudge `{type, pane_id, target}` and keep the
  watch alive (`pane_watch.go:288`).

### 7.4 `pane_resync` (server → client)

Bare `{type:"pane_resync", pane_id, target}` — no content. Client must issue a
forced `read_pane` (`content_fingerprint: ""`) to get a fresh `pane_content`
(`store.ts:1565-1570, 2462`). Note `read_pane` **cancels the watch**
(`server.go:765`); the client re-issues `watch_pane` after the `pane_content`
arrives (`store.ts:1641`).

### 7.5 `pane_unchanged` (server → client)

Answer to `read_pane` when the supplied `content_fingerprint` equals the current
frame fingerprint: `{type:"pane_unchanged", pane_id, content_fingerprint, target}`
— no content, no metadata (`server.go:2933-2946`). Client keeps its stored
frame and starts/restarts the watch (`store.ts:1555-1563`).

## 8. Watch lifecycle (sender state machine)

One `watch_pane` **per client connection** — a new `watch_pane` cancels the
previous watch for that client (`pane_watch.go:88-93`). `paneWatches` is keyed
by `client.ID()`, not by pane.

`watch_pane` request fields (`pane_watch.go:56-97`):

| Field | Handling |
|---|---|
| `pane_id` | required; empty → ignored |
| `lines` | default 30, clamped `[1, 10000]` (`history.MaxLines`, `history.go:17`) |
| `format` | `"ansi"` keeps ANSI; anything else → `"text"` |
| `interval_ms` | one of `{100, 250, 500, 1000}` else `250` (`pane_watch.go:291-299`) |
| `content_fingerprint` | client's retained base hint |
| `target` | optional `TargetRef`, echoed back |

Per-tick decision tree (`pane_watch.go:165-223`, `325-351`):

```
if watch.pending exists:
    if age < 4s:  skip tick entirely (ack gate — never pipeline)
    else:         pending = acknowledged = probe = nil   # chain reset

probe = dispatcher.HandleProbePane(...)          # cheap pane snapshot
if probe fails: skip tick
if probeFingerprint == storedProbe
   AND !(acknowledged.resizeSettling || classificationAgent changed):
    skip tick                                     # content-probe gate
# else full frame read:
frame = readPaneWatchFrame()                     # leased-size read + classify

# paneWatchUpdate():
if acknowledged && acknowledged.frameFingerprint == frame.frameFingerprint:
    send NOTHING; acknowledged = frame            # silent base advance
elif acknowledged == nil:
    send pane_content + ack_required              # first frame / post-timeout
elif acknowledged.contentFingerprint == frame.contentFingerprint:
    send pane_delta [{copy_lines: count("\n")+1}] # metadata-only tiny delta
elif Efficient(Build(acknowledged.content, frame.content)):
    send pane_delta segments
else:
    send pane_content + ack_required              # delta too big → full frame

if a message was sent: pending = frame (sentAt = now)   # every frame is ack-gated
```

Ack-timeout reset is the only chain-recovery: at ≥ `paneWatchAckTimeout = 4s`
(`pane_watch.go:23`) the base is dropped so the next frame is a full
`ack_required` `pane_content`, not a delta the client cannot chain
(`pane_watch.go:170-181`).

`unwatch_pane` / disconnect / a newer `watch_pane` cancel the watch
(`pane_watch.go:99-107`, `server.go:777-779`).

### 8.1 Fingerprints

- `content_fingerprint = hex(sha256(content))[0:16]` — 16 lowercase hex chars
  (`server.go:2877-2880`). Content-only.
- `frameFingerprint` = hex(sha256 over a typed-field encoding of
  `[content, format, truncated, viewport_only, viewport_rows, resize_settling,
  attention_kind, prompt, command, options, interaction, question_layout]`)[0:16]
  — **server-internal only**, never on the wire (`server.go:2882-2931`). It is
  what lets the relay skip sending when only nothing-visible changed.
- `resize_settling` is set on frames while a pane-size lease resized the TTY
  within the last `paneResizeSettleWindow = 3s` (`pane_watch.go:29`,
  `server.go:2841-2847`) — clients must not commit `content` from such frames
  into history (the agent may still be repainting).

## 9. Worked examples

Assume `content_fingerprint` values `f(prev)`, `f(cur)`; `S` = segments.

### Example 1 — append (the hot path)

```
prev = "one\ntwo\nthree\nfour\n"        (4 lines)
cur  = "one\ntwo\nthree\nfour\nfive\n"  (5 lines)
Build: current[0..3] anchor matches prev[0..3] fully (match run stops at
prev len) → copy{0,4}; literal "five\n".
segments = [ {"copy_lines":4}, {"text":"five\n"} ]      # copy_start 0 omitted
Efficient: literalBytes=5 + 64*2=133 < 30*3/4=22?  No → FULL FRAME.
```
Note: short frames usually fail `Efficient`; deltas pay off on large scrollback.

### Example 2 — single-line rewrite (anchor collapse)

```
prev = "one\ntwo\nthree\nfour\nfive\n"
cur  = "one\ntwo\nTHREE\nfour\nfive\n"
Every 3-line anchor overlaps the changed line → no candidate matches at any
position → single literal covering all of cur.
segments = [ {"text":"one\ntwo\nTHREE\nfour\nfive\n"} ]   (fails Efficient → full frame)
```

### Example 3 — scroll (drop head, append tail)

```
prev = "one\ntwo\nthree\nfour\nfive\n"
cur  = "two\nthree\nfour\nfive\nsix\n"
current[0..2]=(two,three,four) matches prev index 1, run extends to 4 lines.
segments = [ {"copy_start":1,"copy_lines":4}, {"text":"six\n"} ]
```

### Example 4 — unchanged content, changed metadata (synthetic, not via Build)

```
acknowledged.contentFingerprint == current.contentFingerprint
segments = [ {"copy_lines": count(prev,"\n") + 1} ]
e.g. prev="a\nb\n" → [{"copy_lines":3}] — VALID under §6 boundary semantics
(boundaries [0,2,4,4], copy_end=3 ≤ 3 → copies bytes 0..4 = whole string);
INVALID under Go Apply. Never produced by Build itself.
```

### Example 5 — empty current

`Build(prev, "")`: currentLines = `[""]`, loop breaks immediately, final
`flushLiteral` emits nothing (`end <= literalStart`) → `segments = []`.
Client: `base == content fingerprint` fast-path keeps content; otherwise applies
to `""`.

## 10. Edge cases

- **Empty previous** (`""` → `[""]`, 1 line): no anchor exists; Build emits a
  single literal → almost always fails `Efficient` → full frame.
- **Trailing newline**: the `count("\n")+1` copy segment is the whole point of
  the boundary-table apply — do not "fix" it to `SplitAfter` line counts.
- **Missing final newline**: `"a\nb"` → `["a\n","b"]`; the last line is a
  partial line and participates in anchors normally.
- **CRLF**: `\r` stays inside the line; both sides treat `\r\n` content
  byte-exactly — no normalization anywhere in the pipeline.
- **Unicode**: lines are compared as UTF-8 byte strings; `len()` in `Efficient`
  is bytes. Kotlin must use UTF-8 byte offsets (Kotlin `String` is UTF-16 —
  slice by **byte boundary table computed over UTF-8 bytes**, or keep the pane
  content as `ByteArray` and decode on render).
- **Same content, different `lines` budget**: `content_fingerprint` covers only
  content; a watch with a different `lines` value still re-reads — fingerprints
  are only comparable for identical read parameters. The relay does not guard
  against a fingerprint from a different `lines`/`format` watch (OPEN QUESTION-2).
- **`pane_delta` arriving while not watched** (`watched == nil`): client still
  applies it and stores the frame, just doesn't ack (`store.ts:1586-1608`).
- **Base mismatch / apply failure / missing `content_fingerprint`**: client
  sends forced `read_pane` (`content_fingerprint:""`), which also stops the
  watch server-side; client re-watches after the `pane_content` lands
  (`store.ts:1587-1589, 1641`; `server.go:765`).
- **Dup/late `pane_applied`** matching `acknowledged` (not `pending`) is a no-op;
  matching neither → `pane_resync`.
- **`segments: null`** with `base == content == stored` → metadata-only keep
  (§6.1). `segments: null` with `base == stored != content` → JS returns
  `applyPaneDelta(...) = null` (not an array) → forced resync.

## 11. OPEN QUESTIONS

1. **Apply semantics divergence**: Go `Apply` rejects `copy_lines = count("\n")+1`
   on newline-terminated content; the released JS client accepts it via the
   boundary table, and the relay emits such segments for metadata-only frames.
   Kotlin must implement the JS semantics for wire compatibility; decide whether
   the Rust relay's verifier (if any) keeps `SplitAfter` semantics or aligns.
2. **Fingerprint scope**: `content_fingerprint` covers content only, not
   `lines`/`format`. A client could supply a fingerprint computed at a different
   `lines` budget and (with matching content) receive `pane_unchanged`/tiny-delta
   that silently proceeds. Harmless today because the same client supplies both;
   decide if the Rust relay should bind the fingerprint to read parameters.
3. **Max `pane_content` size vs 4 MiB send cap**: a pane read capped at
   `lines=10000` of very long lines could exceed `MaxOutboundMessageBytes` and
   evict the client (see `sendbuffer.md` §4). Whether pane reads need a byte cap
   before encoding is a product decision — Go has none.
