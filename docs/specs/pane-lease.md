# Pane Size Lease — `lease_pane_size` / `release_pane_size`

Spec for multi-client pane-size arbitration. Closes `docs/10-spec-gaps.md`
P0-4. All line numbers cite `~/Projects/lerdr` (original Go implementation — provenance only).

Sources: `internal/panesize/manager.go` (lease manager — entire file cited),
`internal/app/server.go` (action wiring, disconnect cleanup),
`internal/app/pane_watch.go` (`applyPaneReadLease`, `resize_settling`),
`frontend/src/components/TerminalView.svelte` + `store.ts` (client cadence).

---

## 1. Constants & bounds

| Constant | Value | Source |
|---|---|---|
| `MinColumns` / `MaxColumns` | 40 / 240 | `manager.go:20-21` |
| `MinRows` / `MaxRows` | 10 / 120 | `manager.go:22-23` |
| `LeaseTTL` | **120 s** | `manager.go:32` |
| `ReleaseGrace` | **10 s** | `manager.go:38` |
| `sweepInterval` | 1 s | `manager.go:40` |
| `commandTimeout` | 3 s per sweep / resolve | `manager.go:41` |
| `paneResizeSettleWindow` | 3 s (`resize_settling` frame flag) | `pane_watch.go:29` |
| Client renewal cadence | every **10 s** while the pane is visible | `TerminalView.svelte` (`PANE_SIZE_LEASE_REFRESH_MS`) |

**`LeaseTTL = 120 s` rationale** (`manager.go:24-31` comment): desktop Safari
clamps hidden/occluded-tab timers to a measured ~60–65 s cadence within two
minutes. A 30 s TTL lapsed leases on every glance away; 120 s survives the
clamp for a still-renewing hidden page while bounding a frozen/vanished client
to ~2 min. A *closing* client is still released immediately via `ReleaseClient`.

## 2. Lease model

```go
type Lease struct {                    // manager.go:71-77
    Columns   int                     // 40..240 (validated)
    Rows      int                     // 0 = width-only; else 10..120
    ExpiresAt time.Time               // now + 120s on each acquire/renew
}
```

- Leases are stored per pane: `paneState.leases map[clientID]Lease`
  (`manager.go:86`). **One lease per client per pane**; acquiring again is a
  renewal that overwrites (`manager.go:185-186`).
- **`Rows = 0` means width-only**: the pane keeps its own height and old clients
  stay wire-compatible (`manager.go:73-74`). Older relays ignore `rows`;
  clients send `rows: 0` unless capability `pane_size_lease_rows` is present
  (`TerminalView.svelte:1725`).
- Validation order in `Acquire` (`manager.go:130-143`): clientID/paneID
  required → `ErrInvalidLease`; columns out of range → `ErrInvalidColumns`;
  `rows != 0` and out of range → `ErrInvalidRows`.

## 3. Effective size arbitration

```
effectiveColumns = min(lease.Columns over live leases)          # manager.go:187, 544-552
effectiveRows    = min(lease.Rows   over live leases with Rows>0)
                   or baselineRows if every lease is width-only # manager.go:188-192, 556-567
```

- `minimumColumns`/`minimumRows` iterate the raw lease map — **including
  expired-but-unswept leases** inside `Acquire`/`reconcile`. Expiry is removed
  by `removeExpired` at `Acquire` entry (existing pane, `manager.go:166`) and by
  the 1 s sweeper (`manager.go:365-389`), and filtered in the public
  `ActiveColumns`/`ActiveRows` queries (`manager.go:241-249, 284-299`). The
  in-between states are intentional: a just-expired lease still counts toward
  the target until a sweep reconciles — prevents a resize flicker between a
  lapse and the next tick.

- **stty write suppression**: `Acquire` calls `stty` only when the target differs
  from the *current TTY read* (`manager.go:202-219`). A renewal that keeps the
  same columns is a pure lease extension — no `SIGWINCH`, no agent repaint.
  Rows are only passed to `stty` when a row constraint is active or a lapsed one
  must be undone (`sttyRows`, `manager.go:193-198, 594-597`).

## 4. Baseline & local-resize rule

`paneState` tracks per-pane `tty`, `baselineRows/Columns` (restore point),
`appliedRows/Columns` (what stty last set), `resizedAt`, `leases`
(`manager.go:79-87`).

- **First lease** (`newState`): `resolvePane` → foreground PID via herdr
  `PaneProcessInfo` → `ps -o tty=` → `ttyPath` validation → `stty size`; baseline
  = applied = current (`manager.go:424-456, 458-474, 481-500`).
- **Local terminal resize while leased becomes the new baseline — per
  dimension** (`manager.go:172-179`):

```
current = stty size                    # read the real TTY
if current.columns != appliedColumns: baselineColumns = current.columns
if current.rows    != appliedRows:    baselineRows    = current.rows
```

  So a human dragging the host terminal narrower while a phone holds a lease
  does NOT fight the lease — the new width becomes the restore point and the
  lease minimum still applies on top.
- `resizedAt` is stamped only on an actual `stty` write (`manager.go:218`);
  `ResizedWithin(3s)` feeds the `resize_settling` frame flag
  (`server.go:2841-2847`).

## 5. Release / expiry / disconnect

| Path | Semantics | Source |
|---|---|---|
| `release_pane_size` (`Release`) | Lease is **lapsed**, not deleted: `ExpiresAt = now + ReleaseGrace` (10 s). If it already expires sooner it is deleted now + `reconcile`. The kept lease still counts toward the minimum during grace — the pane keeps the released width so a quick return doesn't double-resize. | `manager.go:302-335` |
| Disconnect (`ReleaseClient`) | `delete(leases[clientID])` immediately (no grace), then `reconcile` for panes the client owned **or** that now have no active leases. Called on client disconnect with a 5 s context (`server.go` onDisconnect handler). | `manager.go:337-363` |
| Expiry sweep | `SweepExpired` every 1 s: `removeExpired` then reconcile panes whose lease set or target changed. | `manager.go:365-389, 391-407` |
| `Shutdown` | clears all leases, `restore` every pane to baseline. | `manager.go:409-422` |
| `reconcile` | computes min columns/rows over the (already pruned) map; `active==false` → `restore`; else `stty` if target ≠ applied. | `manager.go:581-605` |
| `restore` | `stty` back to baseline columns (rows only if the height was ever leased away), `applied = baseline`, **deletes paneState**. | `manager.go:607-619` |

## 6. Errors

| Error | Trigger | Source |
|---|---|---|
| `ErrLeaseOwnerGone` = `"Pane size lease owner is disconnected"` | `ctx.Err()` non-nil at Acquire entry or after the TTY read — the client's connection died mid-acquire | `manager.go:49, 150-152, 181-183` |
| `ErrInvalidLease` / `ErrInvalidColumns` / `ErrInvalidRows` | empty ids / columns ∉ [40,240] / rows ∉ {0}∪[10,120] | `manager.go:46-48, 135-143` |
| `ErrClosed` | manager shut down | `manager.go:45, 147-149` |
| `ErrProcessUnavailable` | no `PaneProcessInfo` / no foreground PID | `manager.go:50, 424-435, 458-474` |
| `ErrTTYUnavailable` | `ps -o tty=` gives `?`, `??`, `-`, empty, absolute, or traversal path | `manager.go:51, 436-443, 528-542` |
| `ErrSizeUnavailable` | `stty size` fails or returns garbage / `rows<1 \|\| cols<1` | `manager.go:52, 481-500` |
| `ErrResizeFailed` | `stty cols/rows` write failed | `manager.go:53, 502-515` |
| `ErrUnsupportedOS` | not linux/darwin (`stty -F` vs `-f`) | `manager.go:54, 517-526` |

On `setSize` failure mid-Acquire the client's lease is rolled back (previous
lease restored or deleted) and the new paneState is still registered —
`manager.go:206-217`.

Failed acquires surface to the client as `command_result{ok:false,
phase:"failed", error:<message>}`; success returns `phase:"completed"` with
`data:{columns, rows}` = the **effective applied** size, not the requested size
(`server.go:756-762` and the `lease_pane_size` handler above it).

## 7. Arbitration table (multi-client)

Let baseline be the TTY size at first lease (updated by local resizes, §4).
"applied" = what stty wrote last.

| Scenario | Applied result | Why |
|---|---|---|
| One phone lease `cols=120` on an 200-col pane | `cols=120, rows=baseline` | min over single lease; rows unconstrained |
| Local terminal resize to 160 while phone lease holds 120 | baseline→160, applied stays 120 | drift capture `manager.go:172-179`; lease still wins until released |
| Second controller acquires `cols=90` | applied → 90 | `min(120, 90)` — narrower wins |
| Second controller acquires `cols=180` | applied stays 120 | `min(120, 180)` — existing narrow lease wins |
| Phone `cols=120,rows=30` + controller width-only `cols=100,rows=0` | `cols=100, rows=30` | min columns over all; rows = min over constrained only |
| All leases width-only | `cols=min, rows=baselineRows` | `minimumRows==0` → baseline (`manager.go:190-192`) |
| Lease expires (no renewal for 120 s) | sweeper removes it; restore if last | `manager.go:569-579, 583-584` |
| `release_pane_size` | lease lapses at now+10 s; pane keeps leased width until then | `manager.go:326-332` |
| Disconnect | lease deleted now; reconcile immediately | `manager.go:337-363` |
| Hidden-but-renewing tab | lease stays (renewal every 10 s ≪ 120 s TTL) | TTL rationale `manager.go:24-31` |
| Frozen/vanished client (no renewal, no disconnect) | expires at TTL — restore within ~2 min | sweep `manager.go:365-407` |
| `Shutdown` | all panes restored, manager closed | `manager.go:409-422` |

## 8. Interaction with pane reads/watch

`applyPaneReadLease` (`pane_watch.go:310-323`): before every pane read the
server overrides `terminal_columns`/`terminal_rows` with `ActiveColumns`/
`ActiveRows` — so **all clients read the pane at the leased size**, and the
lease effectively drives what every watcher sees. This is also why
`resize_settling` matters (§4): right after a lease-driven `stty`, frames are
flagged for 3 s.

## 9. Client contract (Kotlin)

- Acquire `lease_pane_size{pane_id, columns, rows}` when the terminal view is
  visible and focused; renew every 10 s; release on stop (but expect the 10 s
  grace, not an instant snap-back).
- `rows: 0` unless the connection advertised `pane_size_lease_rows`
  (`TerminalView.svelte:1725`).
- Hidden tab: stop watch traffic, keep renewing while a bounded grace allows —
  the 120 s TTL exists to tolerate Safari's ~60–65 s hidden clamp; a *locked*
  app must not count as visible (client-side policy, `TerminalView.svelte`).
- On reconnect: leases were dropped by `ReleaseClient` — re-acquire after the
  pane watch is re-established (see `sendbuffer.md` §6).
- A `command_result{ok:false}` from `lease_pane_size` carries one of the §6
  error strings — treat `ErrLeaseOwnerGone`/`ErrProcessUnavailable`/`ErrTTY…`
  as "pane doesn't support leasing right now", not as retryable transport
  failures.

## 10. OPEN QUESTIONS

1. **`ActiveColumns`/`ActiveRows` vs reconcile divergence**: public queries skip
   expired leases; `reconcile`/`Acquire` count them until swept. Deliberate
   (anti-flicker), but a Kotlin-side model of "effective size" should mirror the
   *query* semantics (skip expired) — confirm that is the intent for
   `lease_pane_size` responses too (`Acquire` returns the raw-map minimum, which
   can include a just-expired co-tenant for one sweep tick).
2. **Width-only vs row-constrained mixing**: `Rows=0` means "no opinion", not
   "unbounded". A row-constrained lease on a pane whose baseline height is
   taller shrinks it; there is no way to say "rows must stay ≥ baseline" —
   product decision whether that's ever needed.
3. **`release_pane_size` grace vs explicit "shrink now"**: a client that wants
   the pane *immediately* restored (e.g. switching to full-width view) cannot —
   release always keeps the width for 10 s. Confirm the Kotlin UI doesn't need
   an immediate-release variant.
