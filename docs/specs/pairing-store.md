# Pairing Store — device credentials, invitations, and the Rust-native schema

This is the normative pairing-store specification for Lerdr. §A records the
frozen protocol semantics; §B defines the Rust-native store with no byte
compatibility or migration requirement.

The protocol details were historically derived from the retired Go relay.
Its paths and line references below are archival provenance only: implement
against this document, `docs/03-protocol.md`, and the committed fixtures.

---

# §A. Protocol and semantic requirements

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

## A.7 Retired store layout (historical contrast only)

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

# §B. Current Rust-native store

Lerdr intentionally has no byte-compatibility or migration path for the
retired store. The production `FileAuthStore` is the authority for the
on-disk format described here.

## B.1 Layout

```text
$LERDR_RELAY_DEVICE_AUTH_DIR or <runtime-dir>/device-auth/
└── devices.json      # JSON, mode 0600; parent directory mode 0700
```

`LERDR_RELAY_DEVICE_AUTH_DIR` overrides the default
`<runtime-dir>/device-auth`; the legacy `HERDR_RELAY_DEVICE_AUTH_DIR` spelling
is also accepted.
The relay creates the directory when absent, rejects a symlinked store
directory, and names the file `devices.json`.

## B.2 JSON schema

```json
{
  "schema_version": 1,
  "invitation": {
    "invitation_id": "…",
    "version": 1,
    "secret": "43-character base64url",
    "expires_at_ms": 0,
    "name": "…",
    "role": "controller",
    "locale": "en",
    "failed_attempts": 0,
    "next_attempt_at_ms": 0,
    "pending_credential_id": ""
  },
  "credentials": [{
    "device_id": "…",
    "credential_id": "…",
    "name": "…",
    "role": "controller",
    "locale": "en",
    "paired_at_ms": 0,
    "last_seen_at_ms": 0,
    "version": 1,
    "revoked": false,
    "secret": "43-character base64url"
  }]
}
```

`invitation` is omitted when absent. `last_seen_at_ms` is omitted when zero;
`secret` is omitted after revocation. `expires_at_ms`, `next_attempt_at_ms`,
`paired_at_ms`, and `last_seen_at_ms` are Unix milliseconds. On load the relay
caps the file at 4 MiB, requires `schema_version == 1`, and validates
credentials, invitation limits, and secret encoding.

## B.3 At-rest policy

Secrets are plaintext base64url in a `0600` file. The relay needs them for
every handshake; an attacker able to read that file already controls the
relay's OS account. A separate sealing key is not part of the current design.

## B.4 Persistence discipline

Every mutation serializes JSON with a trailing newline to a same-directory
temporary file, syncs it, applies `0600`, renames it atomically, reapplies
`0600`, and best-effort syncs the directory. The in-memory state is mutated
and persisted as one locked transaction.

## B.5 Revocation and versioning

Revoked credentials remain inline as tombstones with `revoked: true`, an
incremented `version`, and no secret. `auth_version` must match exactly.
The tombstone lets the relay distinguish a revoked credential from an unknown
one. The store has one live invitation; invitation redemption uses
`pending_credential_id` to make retries idempotent.

## B.6 Invitation lifecycle

- Lifetime: 10 minutes; non-bootstrap invitations burn after five failed
  proofs with exponential backoff.
- Bootstrap invitations do not count failed proofs and can re-arm on expiry
  when no credentials exist or re-enrollment is enabled.
- `reset_devices` replaces credentials with a fresh bootstrap invitation.

## B.7 Store API

`FileAuthStore` provides `open`, credential and invitation snapshots,
invitation creation, device rename/revoke/reset, and the `DeviceAuthStore`
operations for resolving and completing E2EE authentication. The selector is
`{kind: invitation|credential, id, version, locale}`.
