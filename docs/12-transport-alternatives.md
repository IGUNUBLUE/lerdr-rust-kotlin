# 12 — Transport alternatives evaluated (ADR)

**Status:** decided — keep the relay. Evaluated 2026-09 against the
shipped alternative [`sherpr`](https://codeberg.org/alevui/sherpr)
(Flutter, F-Droid-first, "drive your agent fleet over SSH").

## The alternative: SSH-direct, no relay

Sherpr proves the same product — phone controls Herdr agents — works
without a companion daemon. Important precision: it does **not** use
Herdr's `--remote` thin-client path (that protocol renders the desktop
TUI — a terminal, not a mobile UX). It uses SSH as a *transport* for its
own protocol: exec/direct channels into the host's `herdr` socket and
shell.

What that model buys, honestly:

- **Zero daemon.** `sshd` is the endpoint; `herdr-e2ee-v2`, protocol v3,
  the session actor, and the push queue all disappear.
- **Auth for free.** ed25519 keys in the platform keystore + TOFU
  host-key pinning — equivalent in spirit to our QR pairing, no second
  credential system.
- **The full API surface, untranslated.** The app speaks native Herdr
  socket methods; every new upstream capability is available instantly.
  Our relay will always lag the schema (today: 66 of 129 methods used).
- **Multi-machine for free.** SSH endpoints double as a fleet
  dashboard's host list; our relay is single-host.
- **Notifications without a push provider.** An opt-in on-device
  background watcher holds the SSH session and raises local alerts.
  Sherpr chose this deliberately (FLOSS, no Play Services/FCM).

## What the relay carries that SSH-direct cannot

1. **Reliable push while the app is closed.** The durable `queue.json`
   + delivery machinery is a server-side wake channel that works when
   the phone holds no connection at all. The on-device watcher is a
   foreground-service SSH session — subject to Android/OEM background
   kills; a real tradeoff, not a bug.
2. **Server-side semantic authority.** The classifier, attention ledger,
   question forms, and ack state live in one place serving every device.
   SSH-direct must re-derive semantics on-device per app, and every
   semantic improvement ships as an app update.
3. **Plugin citizenship.** `[[events]]`, `[[startup]]`, `link_handlers`,
   and our manifest panes exist *inside* Herdr whether or not any phone
   is connected — the event-hook fires, `agent.view.set` stays
   installed. An SSH client is outside that ecosystem entirely.
4. **Already built and verified.** ~15k LOC of tested, shadow-verified
   relay; pivoting discards working code for a different tradeoff, not a
   missing capability.

## Decision

**Keep the relay.** The daemon is not "transport that could be SSH" —
it is a protocol bridge + always-on watcher + plugin citizen. If the
product were only "interactive terminal while attached," SSH-direct
wins on simplicity. For "your agent needs you → reliable notification →
approve from the phone," something must stay awake when the phone is
not — that is the relay.

## Worth borrowing

- **Multi-machine** — sherpr gets a fleet view free from SSH endpoints;
  our equivalent would be one relay per host with the app aggregating
  paired devices (app-side concern; the wire already supports multiple
  pairings).
- **On-device watcher pattern** — a lightweight foreground watch could
  complement push on FLOSS builds where FCM is unacceptable.
- **Direct API access** — sherpr never waits on a relay port to expose a
  new Herdr method; our mitigation is the SchemaRegistry capability
  ledger + the audit habit that produced `docs/11`'s method map.

## References

- `docs/02-architecture.md` — relay architecture this decision keeps.
- `docs/11-stack-practices.md` — the 0.9.1 capability audit (unused
  methods the relay could still consume).
- sherpr: `codeberg.org/alevui/sherpr` — reconciled against Herdr 0.7.1
  (we target 0.9.1); GPLv3, not affiliated with Herdr.
