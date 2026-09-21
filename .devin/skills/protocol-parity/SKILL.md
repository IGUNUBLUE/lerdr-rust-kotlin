---
name: protocol-parity
description: "Trigger: protocol change, wire format, e2ee, fixture, vector, golden test, compatibility. Enforces frozen wire-protocol rules and the golden-vector workflow for the Lerdr Rust+Kotlin reimplementation."
license: Apache-2.0
metadata:
  author: "IGUNUBLUE"
  version: "1.0"
---

# Protocol Parity

## Activation Contract

Use whenever touching `relay/crates/lerdr-*`, `app/core/{network,crypto,terminal,model}`,
`fixtures/`, or anything that produces/consumes wire bytes.

## Hard Rules

- The wire protocol is **frozen**: `protocol v3` over `herdr-e2ee-v2`. Any byte-level deviation is a bug, not a design choice.
- Never "improve" the format in flight — negotiated changes belong to protocol v2, explicitly versioned.
- Crypto specifics that MUST NOT drift: uncompressed 65-byte P-256 points; base64 RawURLEncoding (no padding); `\x00`-joined binding strings; direction labels `c2s`/`s2c` in AAD; BE64 sequences; HKDF info strings `herdr-e2ee-v2 c2s|s2c`; sequence starts at 0 per direction, strictly ordered.
- `pane_delta` messages are never coalesced; `pane_content`/`pane_resync` are replaceable. Ack gate: one unacked `ack_required` frame max; pending expires ~4s → full resync.

## Execution Steps

1. Before changing shared types or codecs, read `docs/03-protocol.md` and find the closest fixture.
2. If behavior is ambiguous, generate a fixture from the Go implementation (`~/Projects/lerdr`, test-only export hooks) — never guess.
3. Add/adjust a vector test proving byte-parity, in both directions when applicable.
4. Run the full fixture suite for the touched crate/module before committing.

## Output Contract

- New vectors live under `fixtures/<area>/` with the generator script updated in `fixtures/gen/`.
- PRs state which fixtures cover the change; uncovered protocol changes do not merge.

## References

- `docs/03-protocol.md` — normative wire spec.
- `fixtures/` — golden vectors.
