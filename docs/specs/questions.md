# Questions — `Interaction` schema, `answer_question` / `clarify_question` / `navigate_question`

Spec for the structured-question contract between relay and client. Closes
`docs/10-spec-gaps.md` P0-3. All line numbers cite `~/Projects/lerdr`
(read-only oracle).

Sources: `internal/question/parser.go` (Interaction/Option/Other schema, id,
parse dispatch), `internal/question/input.go` (Focus → key planning),
`internal/question/attention.go` (attention kinds),
`internal/coordinator/approval.go` (action handlers, watcher, phases),
`internal/coordinator/dispatch.go` (dispatch + acknowledge),
`internal/app/server.go` (carriers: `blocked`, pane frames, snapshots),
`frontend/src/lib/types.ts` + `store.ts` (client contract).

---

## 1. `Interaction` wire schema

Serialized whenever a question is classified (`parser.go:41-56`). All fields
below are emitted unconditionally unless marked `omitempty`:

| Field | Type | Notes |
|---|---|---|
| `id` | string | 20 lowercase hex chars — `hex(sha256(canonical_json))[0:20]`; see §2 |
| `kind` | string | `"single_select"` or `"multi_select"` — only values ever emitted (all parse sites: `parser.go:471,575,823,903,1006,1138,1229,1382,1522,1588`) |
| `question` | string | the question prompt (compacted to ≤500 chars) |
| `options` | `Option[]` | may be empty |
| `other` | `Other` | always present as an object |
| `submit_label` | string | e.g. `"Submit"`, `"Next"`, `"Continue"` — rendered on the confirm affordance |
| `can_chat` | bool | terminal offers a "Chat about this" row (Claude only — `parser.go:908`; clarify is additionally gated on `Agent=="claude"` at `approval.go:373`) |
| `can_go_back` | bool | `QuestionIndex > 1` (`parser.go:537` et al.) |
| `question_index` | int, `omitempty` | 1-based position in a multi-question flow; 0/absent when unknown |
| `question_total` | int, `omitempty` | total questions; 0/absent when unknown |

**Not serialized** (`json:"-"`, `parser.go:53-56`): `Focus`, `AllOptionCount`,
`Agent`, `NotesActive`. These are relay-internal planning state — the wire never
carries cursor/focus position; the relay replans keys from a fresh parse on each
action (§5). Kotlin must not model a focus field.

### `Option` (`parser.go:12-20`)

| Field | Type | Notes |
|---|---|---|
| `index` | int | position in the on-screen list (0-based as serialized) |
| `label` | string | |
| `description` | string | may be `""` |
| `selected` | bool | currently checked (multi_select) / highlighted state |
| `summary` | `SummaryEntry[]`, `omitempty` | review-screen answers: `{q, a}` pairs (`parser.go:22-25`); free-text answers the terminal doesn't echo are recorded as the literal string `"custom answer"` and re-injected server-side via `FillCustomAnswers` (`parser.go:1989-2021`, `server.go:2835-2840`) |

### `Other` (`parser.go:27-34`)

| Field | Type | Notes |
|---|---|---|
| `selected` | bool | free-text row is the current choice |
| `text` | string | current typed content |
| `label` | string, `omitempty` | e.g. "Type something." |
| `placeholder` | string, `omitempty` | |
| `allow_empty` | bool, `omitempty` | empty free-text counts as a valid choice |
| `hidden` | bool, `omitempty` | other exists structurally but the UI must not offer it (submit-time rejection: `approval.go:385-386`) |

## 2. `id` computation (client does not compute — opaque token)

`interactionID` (`parser.go:1939-1960`) = first 20 hex chars of
`sha256(json.Marshal({kind, question, options:[labels], submit_label, position?}))`
where `position` is `[index, total]` only when both are > 0. Treated as an
opaque stable id by clients: the same rendered question re-parses to the same
id; any visible change (label, prompt, position) changes it — which is what
makes it a staleness token.

`Parse` is gated by `LayoutHint` and dispatched per agent family
(`parser.go:103-137`): `claude`, `codex`, `omp`/`pi`/`oh-my-pi`, `opencode`,
`qoder`, `hermes`. Unsupported/ungated content → `nil` (no interaction).

## 3. Attention kinds (separate axis)

`Classification.Kind` (`attention.go:12-22`) rides on `blocked`/`agents`/pane
frames as `attention_kind`:

| Value | Meaning |
|---|---|
| `"approval"` | yes/no-ish tool approval — answered by `approval` action (not this doc's actions) |
| `"question"` | structured `interaction` present |
| `"chat"` | agent is waiting at a free-text/chat prompt — no options, no interaction (`attention_test.go:424`) |
| `"unknown"` | blocked but nothing classified |

An agent exposing a question has `status:"blocked"` **and**
`attention_kind:"question"` **and** a non-null `interaction`.

## 4. Action payloads (client → server)

All are mutating, coordinated actions; each carries `type`, `protocol:3`,
`request_id`, `pane_id`, and a `target` (TargetRef) like other pane mutations.
`interaction_id` binds the action to a specific rendered question.

| `type` | Payload | Validation (`approval.go`) |
|---|---|---|
| `answer_question` | `interaction_id`, `selected_indices: int[]`, `other_selected: bool`, `other_text: string` | `decodeQuestionPayload` (`approval.go:237-274`): indices must be non-negative ints, deduped+sorted; `other_text` ≤ 100 000 runes; `other_text` requires `other_selected`; at least one of selected/other required. Then `validateQuestionPayload` (`approval.go:366-392`) against a **fresh parse**: indices in range; `other.hidden`+`other_selected` → reject; `single_select` requires exactly one of (one index \| other-as-choice); other-as-choice needs non-empty text or `allow_empty` |
| `clarify_question` | `interaction_id` only | `can_chat` and `interaction.Agent == "claude"` (`approval.go:373`) |
| `navigate_question` | `interaction_id`, `direction: "previous" \| "next"` | direction must be one of the two literals (`approval.go:226-229`); `previous` needs `can_go_back`, `next` needs `0 < question_index < question_total` (`approval.go:368-372`) |

`protocol.go:275-279` maps the three types to `handleQuestion` /
`handleNavigateQuestion` / `handleClarifyQuestion`.

## 5. Server execution model (what the client can rely on)

`submitQuestion` (`approval.go:276-364`):

1. Agent must have `status ∈ {"blocked","done"}` else fail
   `"Agent is no longer waiting for a question"` (`approval.go:296-304`) —
   unless a ledger replay matches (§6).
2. Effect step re-reads the pane (`ReadPane(80, "ansi")`) and re-parses; if the
   fresh `interaction.ID != interaction_id` → fail `"The question changed
   before the answer was applied"` (`approval.go:323-329`). **Staleness is
   enforced by re-parse, never by trusting the client's id.**
3. `PlanInput` translates the semantic intent into terminal keystrokes
   (`input.go:23-52`); steps run with **150 ms** between them
   (`questionKeyDelay`, `approval.go:17,453-457`). Partial application is
   tracked and reported (`partiallyApplied`, `approval.go:415-432`).
4. Immediate `command_result`: `phase:"accepted"`, `ok:true`.
5. `watchQuestion` polls the pane every **350 ms** for up to **5 s**
   (`approvalPollInterval/Timeout`, `approval.go:14-16, 504-563`), then emits a
   final `command_result` broadcast.

### Final phases (`finishQuestionWatch`, `approval.go:573-628`)

| Phase | `ok` | When | `data` |
|---|---|---|---|
| `confirmed` | true | question disappeared/changed appropriately (answer consumed; clarify entered chat; agent unblocked) | — |
| `advanced` | true | `answer_question` and new `question_index == original+1` | `interaction` (the next question) |
| `navigated` | true | `navigate_question` and new index == expected ±1 | `interaction` |
| `failed` | false | new interaction at an unexpected index (`"The agent opened an unexpected question; the screen was refreshed"`) | `interaction` when one exists |
| `unconfirmed` | false | 5 s elapsed with the same question still rendered (`"The agent still shows the same question; try again"`) | — |
| `failed` | false | synchronous validation failures (stale id, out-of-range index, hidden other, …) — immediate, no watcher | — |

A generation change (pane restarted) silently abandons the watcher — the client
keeps the `accepted` result and learns the new state from `agents`/`blocked`
traffic (`approval.go:528-530`).

`command_result` is **broadcast to all clients**, not just the requester
(`broadcastResult`, `approval.go:673-692`) — clients match on `request_id`.

## 6. Idempotency / ledger

Ledger keys embed `request_id` (`approval.go:634-643`):
`"question-answer\0" + pane + \0 + interaction_id + \0 + request_id` (analogous
for navigate/clarify). So:

- Retrying the **same `request_id` + same payload** replays the stored result —
  keys are sent once (`idempotency_test.go:330-381`).
- Same key, **different payload** → `"A different response was already
  submitted"` (`ErrConflict`, `approval.go:291-293`).
- A **different `request_id`** on the same `interaction_id` is a *new*
  operation — both can submit; the loser fails on re-parse once the winner
  advances the question.

Client rule: **reuse `request_id` when retrying a timed-out answer**; never
reuse it for a different answer.

## 7. Lifecycle — asked → answered → stale

**Asked**: `blocked` broadcast carries `interaction`, `interaction_id`,
`attention_kind:"question"`, `question_layout` (`server.go:1939-1969`). The
same fields ride `agents`/`agent_update` snapshots and `pane_content`/
`pane_delta` frames (`server.go:2833`; `store.ts:1607` merges them into the
terminal frame).

**Answered**: client sends `answer_question` → `accepted` → watcher resolves to
`confirmed`/`advanced`/`navigated`/`failed`/`unconfirmed` (§5). Agent unblocks
→ `agent_update`/`agents` with a non-blocked status; the `interaction` field
disappears.

**Stale** — there is **no explicit "question_stale" event**. A client-side
interaction is stale when any of:

- a new `blocked`/`agents`/frame payload arrives with a different
  `interaction.id` (or `interaction: null`) for that pane;
- `status` leaves `blocked`/`done`;
- submit-time rejection with `"The question changed before the answer was
  applied"`, `"Agent is no longer waiting for a question"`, or `"A different
  response was already submitted"`.

Correct UI rule: key question UI by `interaction_id`; discard/dismiss when the
pane's current interaction id no longer matches, and show the terminal's live
frame instead.

Related: `acknowledge_pane` (`dispatch.go:287,619-635`) clears the *attention
badge* on an agent (displayed-status bookkeeping), emitting `agent_update` when
it changes — it does not answer the question.

## 8. Focus → keys (relay-internal, documented for the Rust port)

`Focus{Kind, Index}` is server-side cursor bookkeeping (`parser.go:36-39`):
kinds `"option"`, `"other"`, `"submit"`, `"chat"`. `PlanInput` maps intents:

| Intent | Keys |
|---|---|
| `navigate previous` | `Left` (`Shift+Tab` for opencode) (`input.go:25-29`) |
| `navigate next` | `Right` (`Tab` for opencode) (`input.go:30-34`) |
| `clarify` | navigate to `chat` focus + `Enter` (`input.go:36-37`) |
| select option i | Up/Down by `position(target) − position(current)` + `Enter` (`input.go:345-374`) |
| other (claude) | navigate to last option + `Ctrl+U` (clear) then text + `Enter` (`input.go:295-343`) |
| multi-select | per-option Enter toggles; `submit` focus sits at `AllOptionCount` (`input.go:351-352`) |

Per-agent planners (`codex`, `qoder`, `opencode`, `omp`, `claude` fallback —
`input.go:40-51`) differ in where `other`/`submit` sit; the Rust port must
re-derive these from the per-agent terminal layouts. Position models are
`navigationKeys`/`qoderNavigationKeys`/`openCodeNavigationKeys`
(`input.go:345-423`).

## 9. Edge cases

- `question_index == question_total` → `next` is invalid; answer submits the
  whole flow (`submit_label` shows `"Submit"` on the last codex step,
  `parser.go:1336`).
- `done` status also accepts answers (`approval.go:296`) — the agent may have
  finished but still renders the question; the re-parse decides.
- `other_text` alone (no indices) is valid when `other_selected:true`.
- `selected_indices` may be empty for multi_select if `other_selected` carries
  the answer — but at least one of the two is required (`approval.go:267-269`).
- Answer while a **different** pane generation is live: `paneSessionCurrent`
  fails the effect (`approval.go:335-336`).
- `clarify_question` on non-claude or a question without `can_chat` →
  `"this question can no longer be discussed"`.
- Duplicate `command_result` ordering: `accepted` arrives from the effect, the
  terminal phase arrives later — clients must key on `request_id` and expect
  exactly two messages (receipt then resolution).
- Review screens are themselves `single_select` interactions ("Review answers"
  option) with `summary[]` entries — not a distinct kind.

## 10. OPEN QUESTIONS

1. **Stale signaling is implicit**: clients infer staleness from a changed
   `interaction_id`/status or a submit-time failure — there is no dedicated
   event. Should protocol v4 add `question_stale`/`interaction_closed`, or is
   the implicit contract sufficient for the Kotlin UI?
2. **Silent watcher abort on generation change** (`approval.go:528-530`):
   the client holds `phase:"accepted"` forever for that `request_id`. Should
   the relay commit `failed` instead? (Go chooses silence — the pane's new
   generation makes the question meaningless, and a fresh `blocked` broadcast
   covers the new state.)
3. **`done` answers**: answering a question on a `done` agent is permitted by
   `submitQuestion` but its product meaning is fuzzy — flag for the Kotlin UI
   whether to surface the question card on `done` agents at all.
4. **`clarify_question` result shape**: it reuses `confirmed`/`unconfirmed`
   phases with no dedicated phase or returned data — confirmed by code, but
   whether a Kotlin client should distinguish "entered chat" from "answered" in
   its timeline is a product decision.
