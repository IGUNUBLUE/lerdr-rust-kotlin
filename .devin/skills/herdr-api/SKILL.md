---
name: herdr-api
description: "Trigger: herdr, unix socket, socket api, pane.read, herdr events, agent.list, capability. The Herdr boundary contract: NDJSON RPC one-request-per-connection, event stream, dispatch-boundary error taxonomy."
license: Apache-2.0
metadata:
  author: "IGUNUBLUE"
  version: "1.0"
---

# Herdr API Boundary

## Activation Contract

Use when writing or reviewing `lerdr-herdr` (Rust) or any code that talks
to the Herdr socket/CLI.

## Hard Rules

- **One request per connection** — Herdr closes the socket after each response. Dial fresh; do not pool or cache connections.
- NDJSON RPC: `{"id":"lerdr-api-N","method","params"}\n` → `{"id",result|error}`. Response id must echo; `id:""` + `invalid_request` = pre-dispatch refusal.
- Preserve the dispatch boundary in every error path:
  - `NotStarted` — no bytes written → safe to retry.
  - `Refused{code,message}` — structured Herdr error → NOT applied; surface code.
  - `DispatchedUnknown` — bytes written, no response → may have applied; only idempotent retries.
- All outbound requests bounded by a semaphore (default 32 in flight) — each request is an fd.
- Singleflight identical read-only requests (same method+canonical params) — fan out one result.
- Event stream: request id `lerdr-events`; canonicalize legacy names via the 26-alias map; on drop → re-subscribe + re-bootstrap before resuming emission.
- Capability flags (`pane.read`, `workspace.move_block`, `tab.move`, `workspace.reordered`, `client_shell.endpoint`, `direct_terminal`, `ordinary_json`) are probed with TTL cache, refreshed on reconnect.

## Decision Gates

| Need | Path |
|---|---|
| Read pane content | `pane.read` socket op; CLI `herdr` fallback only if capability unknown |
| Mutating call | socket op → propagate dispatch boundary to receipt phase |
| Topology state | never poll `*.list` in hot paths — consume the event-stream projection |

## References

- `docs/08-herdr-boundary.md` — full contract + topology.
- `~/Projects/lerdr/internal/herdr/` — reference implementation.
