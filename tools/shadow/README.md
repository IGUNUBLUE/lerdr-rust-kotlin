# Shadow-diff parity harness (Phase-3 exit gate)

Runs the Go oracle relay and the Rust relay side-by-side against the same
fake Herdr, drives identical scripted `herdr-e2ee-v2` client traffic through
each, and diffs the normalized outbound frame streams.

## Layout

```
tools/shadow/
  shadow_diff.py        orchestrator: builds binaries, runs both relays, diffs
  scenarios/core.json   client script + normalization/compare policy
  herdr/state.json      fake-Herdr seed (Go CLI-fake Scenario + `socket` ext)
relay/crates/lerdr-shadow/
  src/scenario.rs       scenario/compare config parsing
  src/client.rs         encrypted WS client (handshake + sealed steps)
  src/normalize.rs      volatile-value erasure (drop/map/type-scoped keys)
  src/compare.rs        bucket attribution + unified diff
  src/fake.rs           lerdr-fake-herdr: stateful Herdr socket API
  src/trace.rs          JSONL trace records
  src/bin/lerdr-shadow  `run` (execute scenario → trace) + `diff` (a vs b)
  src/bin/lerdr-fake-herdr.rs
```

## Usage

```bash
# self-parity: rust vs rust — the harness's own determinism gate
python3 tools/shadow/shadow_diff.py --mode self

# parity gate: oracle go relay vs rust relay
python3 tools/shadow/shadow_diff.py --mode go

# keep the run dir (logs/, traces/, report.txt)
python3 tools/shadow/shadow_diff.py --mode go --keep --run-dir /tmp/shadow
```

Exit codes: `0` identical, `1` normalized streams differ, `2` infra failure.
`--run-dir`/`--keep` preserves `logs/` (per-process stdout), `traces/*.jsonl`
(every decrypted frame), `herdr-ops.jsonl` (fake's method log) and
`report.txt`.

`LERDR_ORACLE` overrides the oracle checkout (default `~/Projects/lerdr`);
`SHADOW_RELAY_LOG` sets the Rust relay log level (default `info`).

## What the driver does

1. `cargo build` `lerdr-relay`, `lerdr-shadow`, `lerdr-fake-herdr`; in `--mode
   go` also `go build ./cmd/lerdr` + `./cmd/fake-herdr` from the oracle tree
   (read-only — the oracle is never modified).
2. One `lerdr-fake-herdr` serves `herdr.sock` for **both** relays from
   `herdr/state.json`; `HERDR_BIN` points the Go relay at the oracle's CLI
   fake sharing the same state file (`responses` keys are argv joined with
   `\x00`, hence the `\u0000` escapes in `state.json`).
3. Each relay gets an isolated side dir (`XDG_CONFIG_HOME`/`XDG_DATA_HOME`/
   `XDG_CACHE_HOME`, runtime dir, device-auth store) plus a **shared** `$HOME`
   so `list_directories` output is byte-identical.
4. `lerdr-shadow run` performs the real E2EE handshake (bootstrap token =
   `LERDR_RELAY_TOKEN`, 32 bytes) against `/ws`, then executes `steps`.
5. `lerdr-shadow diff` attributes frames to step buckets or the async pool,
   normalizes, and renders a unified diff + type census + verdict.

## Compare policy (`compare` block)

| knob | effect |
|---|---|
| `drop_types` | whole frame types excluded (one-sided: `action_receipt`, `inventory_status`; transport: `e2ee_server_hello`) |
| `drop_matches` | predicate drops — a `{type, contains}` clause; kills Go's empty startup `activity_history` only when `activities` is null |
| `unordered_types` | scheduler-owned types pool as a sorted multiset, **preempting** `request_id` attribution (a `capture` list on the step wins) |
| `drop_keys` | keys removed at any depth (timestamps, generated `id`s, echoed `target`) |
| `map_keys` | keys kept but value → `"<mapped>"` (device/credential ids, keys, versions) |
| `type_drop_keys` | per-type recursive key drops — declared field-level deltas (`push_config`, `herdr_status`, `agents`, `workspaces`, `pane_content`, `activity*`) |
| `async_dedupe` | collapse identical pool repeats (default true) |
| `notes` | free text echoed into the report header — every normalization should be justified here |

Step buckets stay ordered (the response to a `send` is positional); the async
pool is a sorted multiset (startup burst + unsolicited publishes interleave
freely). `request_id` attribution works across step windows.

## Scenario `steps` ops

- `expect {match}` — assert a frame arrives (type + optional `request_id` +
  deep-subset `contains`) within `timeout_ms`.
- `send {frame, request_id?, until?, capture?, timeout_ms?, quiesce_ms?}` —
  seal+send, collect until `until` matches then quiet for `quiesce_ms`.
- `collect` — drain without sending (post-burst quiescence).
- `settle {ms}`, `fence` — pacing / ordering markers.

`{name}` placeholders resolve from step scope (`request_id`) then `vars`.
Pane-targeted actions carry `vars.target` because Go validates
`target.{pane_id,terminal_id,generation,server_session_id,agent_session_id}`
against live agent state whenever `pane_id` is present; Rust ignores it for
these actions.

## Current result

`--mode go` → **IDENTICAL** (30 vs 23 raw frames; all step buckets and the
async pool match after declared normalization). Documented deltas:

- `action_receipt` — Rust-only v3 dispatch evidence (dropped, still censused).
- `inventory_status` — Go-only poller frame (dropped).
- `push_config`/`herdr_status`/`agents`/`workspaces`/`pane_content`/`activity*`
  — per-type field deltas via `type_drop_keys` (see notes in `core.json`).
- Go emits an empty `activity_history` at connect (predicate-dropped).
- Startup burst order differs (push_config first vs unordered set) — the pool.

## Caveats

- `lerdr-relay` must build cleanly; if a sibling workstream is mid-edit on
  `lerdr-coord`, `--skip-build` reuses the last good `target/debug` binaries.
- The fake socket is shared between relays — state reads are identical and no
  scenario step mutates inventory, so cross-talk is impossible for `core`.
  Scenarios that mutate (workspace_create etc.) should give each side its own
  fake or accept the shared mutations.
