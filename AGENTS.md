# Agent Instructions

New implementation of Lerdr: Rust relay + Kotlin/Compose app.
`~/Projects/lerdr` (Go + Tauri/WebView) is the **reference and oracle** —
it defines product behavior and generates fixtures; nothing is ported
line-by-line. Everything is specified in `docs/` — read `00-inventory`,
`02-architecture`, `03-protocol`, `04-app-design`, `05-roadmap`,
`08-herdr-boundary`, `10-spec-gaps` before implementing anything.

## Rules

- Wire protocol starts at `protocol v3` / `herdr-e2ee-v2` (proven contract,
  golden vectors are the oracle); changes land only through the deliberate
  Phase-5 revision — never by drift.
- English only: code, docs, commits.
- Every commit builds and passes tests; one PR per coherent unit; no
  direct pushes to main.
- Only touch `~/Projects/lerdr` to *generate* fixtures (test-only hooks).
- Parallel work uses disjoint file ownership; shared/generated files
  belong to the orchestrator.

## Verification

- Rust: `cargo test -p <crate>` + `tests/vectors` when wire-facing.
- Kotlin: `./gradlew :module:test` + fixture conformance for
  protocol/terminal/crypto.
- App visual changes: screenshot test (Paparazzi/Roborazzi) in the PR.

## Skills

Repo-local (authored, `.devin/skills/`):

| Skill | When |
|---|---|
| `protocol-parity` | Wire bytes, codecs, crypto, fixtures |
| `herdr-api` | Herdr socket/CLI boundary, events, capabilities |
| `rust-relay` | Crate work under `relay/crates/` |
| `android-app` | Gradle modules, Compose, M3E |

Installed via `npx skills add` (`.agents/skills/`, lockfile
`skills-lock.json` — restore with `npx skills experimental_install`):

| Skill | When |
|---|---|
| `rust-async-patterns` | Tokio/async patterns reference (wshobson/agents) |
| `navigation-3` | Nav3 graphs, scenes, deep links (official android/skills) |
| `testing-setup` | Android test strategy/harnesses (official android/skills) |
