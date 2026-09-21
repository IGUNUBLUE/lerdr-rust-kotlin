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
- `events_lost` recovery (upstream-specified): connection closes → resubscribe, wait `subscription_started`, pull `session.snapshot`, treat later events as invalidation signals only (serialize refreshes; snapshots/events share no sequence boundary — never replay buffered events).
- Prefer wait primitives over polling: `agent.wait{until:[...]}`, `events.wait{match_event}`, `pane.wait_for_output{pane_id,source,match,strip_ansi,timeout_ms}`.
- `session.snapshot` = one-call full topology reconcile (workspaces+tabs+panes+layouts+agents+focus) — the `events_lost` recovery base.
- High-value methods beyond the Go relay's subset: `layout.apply/export` (templates), `command.invoke` (endpoint-issued ids, revision-validated), `agent.explain` (detection debugging), `notification.show`, `plugin.action.invoke` (drive other plugins), `server.reload_config`, `integration.*` — full table in `docs/11-stack-practices.md`.
- `agent.view.set` with `source:"plugin:lerdr.events"` installs the canonical attention-sorted projection (drives Herdr's mobile Agents list too); reapply after `[[startup]]`/live handoff.
- Capabilities come from `herdr api schema --json` (SchemaRegistry) first, TTL-probe fallback for `pane.read`, `workspace.move_block`, `tab.move`, `workspace.reordered`, `client_shell.endpoint`, `direct_terminal`, `ordinary_json`; refreshed on reconnect.

## Decision Gates

| Need | Path |
|---|---|
| Read pane content | `pane.read` socket op; CLI `herdr` fallback only if capability unknown |
| Mutating call | socket op → propagate dispatch boundary to receipt phase |
| Topology state | never poll `*.list` in hot paths — consume the event-stream projection |

## References

- `docs/08-herdr-boundary.md` — full contract + topology.
- `~/Projects/lerdr/internal/herdr/` — reference implementation.
