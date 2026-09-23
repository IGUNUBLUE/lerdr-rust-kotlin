# Pairing Store — device credentials, invitations, and the Rust-native schema

Two parts: **§A** extracts the inherited protocol/store behavior from the Go
reference (the wire contract is frozen); **§B** proposes the new Rust-native
store schema — a fresh design, no byte compatibility, no migration.

All line numbers cite `~/Projects/lerdr` (original Go implementation — provenance only). Sources:
`internal/deviceauth/store.go`, `internal/deviceauth/resolver.go`,
`internal/transport/e2ee.go` (hello/finish fields), `internal/app/server.go`
(actions, disconnect-on-revoke), `internal/setuphelper/setuphelper.go` (QR),
`internal/config/config.go` (runtime dir), `frontend/src/lib/store.ts`
(invite-link construction), `frontend/src/lib/config.ts` (link parsing).

---

# §A. Inherited behavior (what the wire and semantics require)

## A.1 Constants

| Constant | Value | Source |
|---|---|---|
| `storeSchemaVersion` | 1 | `store.go:20` |
| `storeFilename` | `devices.json` | `store.go:21` |
| `secretBytes` | **32** (raw); 43-char base64url-no-pad encoded | `store.go:22`, `resolver.go:277-284` |
| `identifierBytes` | **18** raw → 24-char b64url id (device_id, credential_id, invitation_id) | `store.go:23, 371-377` |
| `invitationLifetime` | **10 min** | `store.go:24` |
| `bootstrapInvitationID` | `"bootstrap"` | `store.go:25` |
| `maxInviteAttempts` | **5** failed proofs → invitation burned | `store.go:26` |
| `maxNameBytes` / `maxLocaleBytes` | 80 / 32 | `store.go:27-28` |
| `e2eeVersion` | 2 (subprotocol `herdr-e2ee-v2`) | `e2ee.go` |
| `qr_code` input cap | **512 bytes** (`MaxQRBytes`) | `setuphelper.go:107` |

Secrets and ids encode as `base64.RawURLEncoding` — raw URL-safe, **no padding**
(`store.go:233, 376`, `e2ee.go:335`).

## A.2 Setup link / QR payload

A pairing link is `<appOrigin>/#<query>`; the fragment is `url.Values`-encoded.
Two shapes exist:

**Bootstrap/legacy** (`SetupFragment`, `setuphelper.go:14-22`): `setup=<token>`
(the raw 32-byte relay key as a string), `label=<host>`, optional
`relay=<ws(s)://…>`.

**Invitation** (built client-side after `create_device_invitation`,
`store.ts:1884-1928`):

| Param | Content |
|---|---|
| `setup` | the **invitation secret** (43-char b64url, validated `^[A-Za-z0-9_-]{43}$`, `store.ts:1906`) |
| `invite` | `invitation_id` (`^[A-Za-z0-9_-]{16,128}$`) |
| `invite_version` | integer ≥ 1 (currently always `1` — invitations never bump version) |
| `invite_expires` | `expires_at` as unix **ms** (`Date.parse` of the ISO timestamp) |
| `label` | relay display name |
| `relay` | direct `ws(s)://` origin — when the relay has a URL |
| `gateways` | comma-separated `ws/wss` gateway origins — when reachable only via gateways |
| `relay_id` + `rendezvous` | rendezvous identity + key — required with `gateways` (`store.ts:1917-1924`); the relay key itself never travels in the link |

Parsing/validation lives in `config.ts` (`parseSetupLink`); `setup` alone (no
`invite`) means the relay-token bootstrap path. QR rendering is relay-side:
`qr_code{text}` → `{size, modules}` where `modules` is `StdEncoding` base64 of
row-major packed bits, no quiet zone (`server.go:1067-1079`,
`setuphelper.go:111-139`); terminal rendering uses half-block glyphs
(`TerminalQR`, `setuphelper.go:65-103`).

## A.3 Invitation → credential exchange (field level)

E2EE handshake auth fields (`e2ee.go:145-155, 178-187`):

| Direction | Message | Fields |
|---|---|---|
| client → relay | `e2ee_client_hello` | `version:2`, `auth_kind: "invitation"\|"credential"`, `auth_id`, `auth_version`, `locale`, `nonce`(43 b64url), `public_key`(P-256 uncompressed, b64url), `proof`(sha256, b64url) — proof binds `kind\0id\0version\0` (`e2eeAuthBinding`, `e2ee.go:404-415`) |
| relay → client | `e2ee_server_hello` | `version`, `nonce`, `public_key`, `proof` |
| client → relay | `e2ee_client_finish` | `version` only — *inside the encrypted channel*; auth committed only after this authenticates (`e2ee.go:301-309`) |
| relay → client | `e2ee_server_finish` | `device_id`, `credential_id`, `role`, `locale`, `credential_version`, `credential_secret` (**only on invitation redemption**, `e2ee.go:330-337`) |

Redemption (`redeemInvitationLocked`, `resolver.go:138-199`):

1. `auth_id`/`auth_version` must match the live invitation record exactly, else
   `ErrAuthentication`; expired → consumed + `ErrInvitationExpired`.
2. `PendingCredentialID` set → return that credential again (**idempotent
   retry**: a client that lost the finish re-presents the same invitation and
   gets the same credential back, `resolver.go:153-158`).
3. Otherwise: mint `device_id` (18 B), `credential_id` (18 B), `secret` (32 B);
   new credential `version=1`, `paired_at=last_seen_at=now`, `name/role/locale`
   inherited from the invitation (locale normalized through `localize`).
4. `record.PendingCredentialID = credential_id` persists so step 2 works; the
   invitation is **cleared** when that credential next completes authentication
   (`completeCredentialLocked`, `resolver.go:215-218`) or when the credential
   is revoked (`store.go:308-311`).

So an invitation is *redeemed* at server-finish issue but *consumed* only when
the credential authenticates — a crashed client between the two does not burn
the invitation.

## A.4 `credential_version` semantics

- Issued at **1** (`resolver.go:180`).
- `RevokeCredential`: `revoked=true`, **`version++`**, `secret=""` (tombstone —
  `store.go:303-307`; validation rejects a revoked record that still has a
  secret, `store.go:512-519`).
- `ResolveE2EESecret`/`AuthorizeCredential` require `selector.version ==
  record.Version` **and** `!revoked` (`resolver.go:63-70`, `store.go:256-268`).
  A revoked credential at old version fails on the version check; at the bumped
  version it hits `ErrRevoked`. Either way `IsE2EEAuthRejected` → close 4401
  (`resolver.go:17-22`, `conn.go` unauthorized close).
- Revocation disconnects live sessions: `revoke_device` calls
  `DisconnectCredential(credentialID, credential.Version)` after 250 ms — every
  conn with `CredentialVersion <= throughVersion` gets `CloseGoingAway
  "device credential revoked"` (`server.go:822-849`, `ws.go:610-635`). The hub
  `blocked[credentialID]` fence also rejects *future* handshakes at ≤ that
  version in-memory (`ws.go:620-622`) — the store's version bump is the durable
  half of the same fence.
- `completeCredentialLocked` refreshes `last_seen_at` and `locale` on every
  successful credential auth (`resolver.go:210-224`).

## A.5 Roles

`controller` | `reader` (`store.go:33-36`; a third `bootstrap` literal exists in
`protocol.go` DeviceRole but is never a stored role). `reader` is denied every
mutating action except `revoke_device` on **its own** `device_id`
(`server.go:296-321`, `server_test.go:77-78`). `ErrLastController` blocks
revoking the last active controller (`store.go:41, 298-300`).

## A.6 Invitation rate limiting / burning (`resolver.go:95-136`)

- Wrong proof: `FailedAttempts++`; at 5 → invitation deleted, `ErrInvitationBurned`.
- Backoff `next_attempt_at = now + 1s << (failedAttempts-1)` (1 s, 2 s, 4 s, 8 s);
  attempts before then → `ErrRateLimited` (does not count toward burn).
- **Bootstrap exemption**: `bootstrap` invitations never count failed attempts —
  the full-entropy secret can't be brute-forced and counting would be a DoS on
  the first device (`resolver.go:100-104`).
- Expired non-bootstrap invitations are deleted on touch; expired bootstrap
  re-arms (+10 min) when there are no credentials or `rearmBootstrap` is set
  (`resolver.go:34-49`).
- `CreateInvitation` **replaces** the single invitation slot — only one live
  invitation exists at a time (`store.go:182-183`).

## A.7 Go store file (for contrast, not for porting)

`<RuntimeDir>/device-auth/devices.json` (`server.go:209`), `RuntimeDir` =
`dirname($LERDR_RELAY_ENV)` → `$HERDR_PLUGIN_CONFIG_DIR` → `~/.config/lerdr`
with legacy adoption (`config.go:174-185`). Permissions: dir `0700`, file
`0600`, atomic tmp+rename+dirsync (`store.go:420-454, 456-480`). Load: `Lstat`
must be regular file, `LimitReader` 4 MiB, `DisallowUnknownFields`, single JSON
value, `schema_version==1`, full `validateState` (`store.go:379-418, 493-540`).
All secrets stored **plaintext** inside the `0600` file.

`diskState` = `{schema_version, invitation?, credentials[]}`; records per
`credentialRecord`/`invitationRecord` (`store.go:74-96`) — fields as in §A.3-5
plus internal `failed_attempts`, `next_attempt_at`, `pending_credential_id`.

Bootstrap arming: `armBootstrap` on startup (`server.go:268-277`); SIGUSR1 →
`ArmBootstrapInvitation` re-arms before each printed setup link
(`server.go:2736-2756`). `reset_devices` wipes all credentials and installs a
fresh bootstrap (`ResetWithBootstrap`, `store.go:320-349`), then disconnects
every enrolled credential (`server.go:850-879`).

---

# §B. Rust-native store schema (proposal — fresh design)

Reimplementation, not migration: no byte compatibility with `devices.json`, no
migration path required. The invariants that must survive are **semantic**
(marked ⓘ). Everything else is open to redesign.

## B.1 Layout

```
$HERDR_PLUGIN_CONFIG_DIR/            # RuntimeDir equivalent (config.go:174-185)
└── pairing/
    ├── devices.toml        # metadata + tombstones + invitation — plaintext, 0600
    └── devices.lock        # advisory flock for multi-process safety
```

One file, one lock. Keep the name `devices.*` out of deference to operators
used to finding it there; `.toml` chosen over JSON only because the Rust relay
already parses TOML config — JSON is equally acceptable. OPEN QUESTION-1
(format choice) — the wire never sees this file.

## B.2 Schema

```toml
schema_version = 2

[invitation]                      # absent when no live invitation (ⓘ one slot)
id          = "…"                 # 24-char b64url (18 random bytes) or "bootstrap"
version     = 1
secret      = "…"                 # 43-char b64url (32 bytes) — see §B.3
expires_at  = "2026-05-26T12:00:00Z"   # RFC 3339, UTC
name        = "…"                 # ≤80 bytes, UTF-8, no control chars
role        = "controller"        # or "reader"
locale      = "en"                # ≤32 bytes, no whitespace/"\\/"
failed_attempts   = 0             # 0..4 (≥5 means burned → record dropped)
next_attempt_at   = "…"           # optional RFC 3339
pending_credential_id = "…"       # optional — idempotent redemption (ⓘ)

[[credentials]]
device_id        = "…"            # 24-char b64url — unique
credential_id    = "…"            # 24-char b64url — unique
name             = "…"
role             = "controller" | "reader"
locale           = "en"
paired_at        = "…"            # RFC 3339
last_seen_at     = "…"            # optional; refreshed on each auth (ⓘ)
version          = 1              # ⓘ monotonic; ++ on revoke
revoked          = false
secret           = "…"            # 43-char b64url — required iff !revoked (ⓘ)
```

ⓘ invariants carried over from `validateState` (`store.go:493-540`):

- unique `device_id` and `credential_id`; `version ≥ 1`.
- `revoked ⇒ secret absent`; `!revoked ⇒ secret present and 32-byte-decodable`.
- `failed_attempts ∈ [0,5)`; `pending_credential_id` must reference an
  **active** credential.
- exactly one `[invitation]` table at most.
- Unknown fields rejected on load (serde `deny_unknown_fields`), file capped
  at 4 MiB, must be a regular non-symlink file.

## B.3 Plaintext vs sealed

Go stores secrets plaintext at `0600` (`store.go:390, 432`). Proposal:

| Field | At rest | Rationale |
|---|---|---|
| `credential.secret`, `invitation.secret` | **plaintext, file `0600`** — parity with Go's threat model (the relay process needs them on every handshake; an attacker reading `0600` already owns the relay's OS account) | `store.go:569-572` |
| everything else | plaintext `0600` | operational inspectability |

Do **not** introduce a separate sealing key unless the product decides
OS-account compromise is in scope — a key stored next to the file buys nothing
and complicates recovery. OPEN QUESTION-2: if the threat model later requires
sealing (e.g. Android keystore-backed envelope on mobile-relay deployments),
version the `secret` field as `{sealed: {scheme, nonce, ciphertext}}` rather
than a bare string — design the enum now so the format doesn't churn.

## B.4 Persistence discipline

- Write via `NamedTempFile` in the same dir → `chmod 0600` → `sync` → `rename`
  → `fsync` the directory (parity with `store.go:420-454`).
- `MkdirAll(dir, 0700)` + `chmod 0700` on open; reject if dir is a symlink
  (`store.go:456-471`).
- Hold the in-memory `RwLock` over load/mutate/persist; all mutating ops do
  read-modify-write-persist with rollback on persist failure (Go pattern:
  `store.go:184-187, 312-315`).

## B.5 Revocation list representation

Inline tombstones — keep revoked credentials in `[[credentials]]` with
`revoked=true`, bumped `version`, no `secret` (Go model, `store.go:303-307`).

Why not a separate `[[revoked]]` list: the tombstone's `credential_id` +
`version` must be queryable by the auth path to distinguish "unknown id" from
"revoked" for `ErrRevoked` vs `ErrAuthentication` (`resolver.go:63-70`), and
the device list UI needs `paired_at`/name history anyway. Tombstone GC is a
product decision (Go never GCs) — OPEN QUESTION-3.

## B.6 Versioning & monotonicity rules

- `credential.version`: starts 1; `++` on revoke only; `auth_version` must
  match exactly (`resolver.go:67-68`). No other mutation path touches it.
- `invitation.version`: always 1 today; keep the field + the exact-match check
  (`resolver.go:31`) so a future "re-issue invitation" flow has a fence.
- `schema_version = 2` in the new file (1 is the Go file; we are not it).
- The relay's in-memory `blocked[credentialID]` fence (`ws.go:610-635`) becomes
  a per-connection check at handshake time; the durable fence is the bumped
  `version` + `revoked` bit.

## B.7 Invitation lifecycle (unchanged semantics)

- `invitationLifetime = 10 min`, `maxInviteAttempts = 5`, backoff
  `1s << (n-1)`, bootstrap id `"bootstrap"`, bootstrap never counts failures
  (`resolver.go:34-56, 100-104`).
- Redemption is two-phase via `pending_credential_id` (§A.3) — the Rust store
  must keep this; it's the only thing that makes invite-redemption retry-safe.
- `EnsureBootstrapInvitation` / `ArmBootstrapInvitation` / `ResetWithBootstrap`
  / `rearmBootstrap` semantics carry over verbatim (`store.go:191-244, 320-349`).

## B.8 Operations the store must support (API surface)

Mirror `Store`: `open`, `create_invitation`, `ensure_bootstrap`,
`arm_bootstrap`, `list_credentials(current_credential_id)` (sets `current`),
`authorize_credential(id, version)`, `rename_credential`,
`revoke_credential` (with `ErrLastController` guard), `reset_with_bootstrap`,
`resolve_e2ee_secret`, `complete_e2ee_auth(selector, authenticated)`,
`is_e2ee_auth_rejected(err)`. Selector `{kind: invitation|credential, id,
version, locale}` matches `E2EEAuthSelector` (`e2ee.go:110-115`).

## B.9 OPEN QUESTIONS

1. **File format**: TOML vs JSON — TOML proposed for consistency with relay
   config; no functional difference. Decide before first write (the file is
   append-only-once; changing format later *is* a migration).
2. **Sealed secrets**: §B.3 proposes plaintext-at-0600 parity. If mobile-relay
   deployments need keystore-backed sealing, adopt the tagged `{sealed:…}`
   envelope now.
3. **Revoked-credential tombstone GC**: Go keeps tombstones forever; a relay
   enrolling/revoking for years accumulates them. Cap the tombstone set (e.g.
   keep last N, or prune `paired_at < now - 90d`) — the `credential_id`
   collision space is 144 bits so reuse is a non-issue; GC only affects how
   `ErrRevoked` vs `ErrAuthentication` is distinguished for ancient ids.
4. **`last_seen_at` write amplification**: every credential auth persists the
   file (fsync'd) just to bump `last_seen_at`/`locale` (`resolver.go:210-224`).
   On flash-backed mobile devices this may be too hot — consider batching or
   deferring the timestamp write. Go does not.
5. **`device-auth` vs `pairing` dir name**: proposal uses `pairing/`; harmless
   to rename, but pick before first shipped release.
6. **Multi-process access**: Go relies on the relay being a singleton (pid file
   aside). If the Rust relay can run as a plugin alongside a CLI tool that
   also touches the store, the advisory `devices.lock` needs a defined
   policy — OPEN QUESTION whether herdr CLI ever writes here.
