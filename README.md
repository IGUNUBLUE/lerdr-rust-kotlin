# lerdr-rust-kotlin

Lerdr: relay in **Rust**, Android app in **Kotlin + Jetpack Compose
(Material 3 Expressive)**. The product: your coding agents, live on the
phone — sessions, questions, approvals, terminals, and workspace files,
over an end-to-end encrypted channel.

The repo is self-contained: `docs/` is the spec and the plan, and the
committed golden vectors in `fixtures/` anchor the wire contract. There
is no external reference implementation to track.

## Why

- **Rust**: one static binary per computer, no GC, with the concurrency
  profile the relay needs (pane watches, per-client queues, coalescing)
  — memory-auditable and cheap on modest machines.
- **Native Kotlin**: no WebView. Real 60/120 fps rendering, native
  gestures, a real terminal keyboard, notifications and platform
  integrations without plugin IPC layers — an experience designed for
  the phone that feels like sitting at the computer.

## The core idea

**The protocol is the boundary.** `protocol v3` over `herdr-e2ee-v2` is
the frozen contract. The relay and the app evolve independently as long
as both speak it; the golden vectors keep them honest byte-for-byte.

## Documents

| Doc | Contents |
|---|---|
| [00 — Inventory](docs/00-inventory.md) | Feature surface inventory: packages, actions, app features, native surface |
| [01 — Research](docs/01-research.md) | Reference apps and what to copy from each |
| [02 — Architecture](docs/02-architecture.md) | Rust crates, Kotlin modules, stack decisions |
| [03 — Protocol](docs/03-protocol.md) | Wire contract: E2EE handshake, frames, actions, pane watch |
| [04 — App design](docs/04-app-design.md) | UX spec with Material 3 Expressive, screen by screen |
| [05 — Roadmap](docs/05-roadmap.md) | Phases, deliverables, cutover criteria |
| [06 — Risks](docs/06-risks.md) | Technical risks and mitigations |
| [07 — Execution prompt](docs/07-execution-prompt.md) | Copy-paste master prompt + per-phase loops |
| [08 — Herdr boundary](docs/08-herdr-boundary.md) | Deep API contract + improved actor topology |
| [09 — Plugin distribution](docs/09-plugin-distribution.md) | Shipping the Rust relay as a `lerdr.events` Herdr plugin |
| [10 — Spec gaps](docs/10-spec-gaps.md) | Strict self-review: what is not yet specified, by severity |
| [11 — Stack practices](docs/11-stack-practices.md) | Herdr API upgrades, M3E state, Kotlin/Rust testing + perf rules |

Repo-level agent skills live in [`.devin/skills/`](.devin/skills/) —
see [AGENTS.md](AGENTS.md).

![App concept mockup](docs/mockup.png)

## Project rules

- **Protocol stability first**: `protocol v3` / `herdr-e2ee-v2` is
  frozen; improvements (binary codec on the E2EE path, compression,
  metadata diffing) go into a deliberate Phase-5 revision — never by
  drift.
- **Golden vectors**: every cryptographic or parsing seam is pinned by
  the committed fixtures in `fixtures/`; they change only with a
  protocol revision.
- **Capability completeness**: the app covers the full action catalog
  documented in `docs/03-protocol.md` — the catalog is the checklist.
- **Android-only client**: no PWA in scope. The Rust relay is a pure
  WS+API backend — no `web/` asset pipeline.
