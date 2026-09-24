# Agent Instructions

Lerdr: Rust relay + Kotlin/Compose Android app. `docs/` is the spec and
the authority — read `00-inventory`, `02-architecture`, `03-protocol`,
`04-app-design`, `05-roadmap`, `08-herdr-boundary`, `10-spec-gaps` before
implementing anything. The project is self-contained: it does not track
or compare against any external implementation.

## Rules

- Wire protocol is `protocol v3` / `herdr-e2ee-v2` (frozen contract,
  anchored by the committed golden vectors in `fixtures/`); changes land
  only through the deliberate Phase-5 revision — never by drift.
- English only: code, docs, commits.
- Every commit builds and passes tests; one PR per coherent unit; no
  direct pushes to main.
- `fixtures/` vectors are frozen — they change only as part of a
  deliberate protocol revision.
- Parallel work uses disjoint file ownership; shared/generated files
  belong to the orchestrator.
- Comments citing the original implementation (e.g. "the reference") are
  historical provenance, not a standing comparison rule.

## Verification

- Rust: `cargo test -p <crate>` + `tests/vectors` when wire-facing.
  Workspace root: `relay/` (fmt + `clippy -D warnings` clean).
- Kotlin: `cd app && ./gradlew test` — JVM modules use `:core:x:test`,
  Android-library modules use `:core:x:testDebugUnitTest`, and
  `:app:assembleDebug` must stay green. JDK 17 at `~/.local/opt/jdk17`;
  SDK at `~/Android/Sdk` (pinned via `app/local.properties`).
- Toolchain floors: Gradle 9.7.1 wrapper, AGP 9.4.1 (built-in Kotlin —
  never add `org.jetbrains.kotlin.android`), compileSdk 37
  (`platforms;android-37.2`), minSdk 28, targetSdk 36.
- Fixture consumers: `com.lerdr.core.testing.Fixtures` (Kotlin),
  `lerdr-fixture` crate (Rust).
- Live gates (not in CI): `LERDR_RUST_INTEROP=1 :core:transport:test
  --tests RustInteropTest` pairs a throwaway device against a spawned
  Rust relay; `HERDR_LIVE=1 cargo test -p lerdr-herdr --test live` hits
  the real Herdr socket; `LERDR_PROBE=1 LERDR_PROBE_TOKEN=<32B>
  :core:transport:test --tests WireProbeTest --rerun-tasks` pairs a
  throwaway device against the Rust relay at `ws://127.0.0.1:8377` and
  prints every inbound frame + codec result (re-arm the invitation with
  `kill -USR1 <relay-pid>` — on the systemd deployment use
  `systemctl --user kill -s USR1 lerdr.service` since the PID changes
  per restart; leaves an enrolled credential behind).
- App visual changes: screenshot test (Roborazzi) in the PR.

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

Vendored from user-global (`.agents/skills/`, committed directly — not
in `skills-lock.json`). `agent-browser`, `find-skills`, `herdr`,
`judgment-day`, `no-commit-attribution`, `skill-creator`,
`skill-improver` and `skill-registry` live only in `~/.agents/skills/`
(global) — not vendored here.

| Skill | When |
|---|---|
| `android-cli` | `android` CLI: devices, emulators, SDK, docs |
| `chained-pr` | Split large PRs into chained review slices |
| `cognitive-doc-design` | Low-cognitive-load docs (guides, RFCs, onboarding) |
| `context7-mcp` | Library/framework API docs lookup |
| `mobile-android-design` | M3/Compose design patterns |
| `work-unit-commits` | Plan commits as reviewable work units |
