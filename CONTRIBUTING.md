# Contributing

Thanks for your interest in lerdr. This project is **beta** — contributions
are welcome, with a few ground rules that keep the codebase coherent.

## Before you start

- Read `AGENTS.md` — it's written for coding agents, but it is the
  shortest accurate description of how this repo is organized and
  verified.
- The spec lives in `docs/`. The frozen wire contract lives in
  `fixtures/`. Behavior is defined by those two — not by any external
  implementation.
- Check open issues/PRs before starting significant work.

## AI-assisted contributions

This project was built mostly by AI agents (see the README origin story),
so AI-assisted PRs are explicitly welcome — under these conditions:

- **Disclose it.** Say which tool/model produced the change in your PR
  description (the PR template has a field for it).
- **Own what you submit.** You are responsible for the change whether a
  human or a model wrote it. Understand every line you send.
- **Run the verification below.** "The AI wrote it" is not an excuse for
  code that doesn't build, fails tests, or drifts from the spec.
- **No spec drift.** Wire-protocol changes only land through a deliberate
  protocol revision (`fixtures/` is frozen). Don't let a model quietly
  reshape the contract.

## Development setup

- **Rust**: MSRV 1.88. Workspace root: `relay/`.
- **Android**: JDK 17, Android SDK `platforms;android-37.2`,
  `build-tools;37.0.0`. Gradle via the wrapper (`app/`).
- Toolchain floors: Gradle 9.7.1, AGP 9.4.1 (built-in Kotlin — do not add
  `org.jetbrains.kotlin.android`), minSdk 28, targetSdk 36.

## Verification — required before opening a PR

```sh
# Rust (workspace root: relay/)
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p <crate-you-touched>

# Wire-facing changes: golden vectors must stay green
cargo test -p lerdr-protocol --test vectors

# Kotlin / Android (from app/)
./gradlew test                    # or :core:<module>:test for JVM modules
./gradlew :app:assembleDebug      # must stay green

# UI/visual changes: add or update a Roborazzi screenshot test
```

## Pull requests

- One PR per coherent unit — no bundled unrelated changes.
- English only: code, docs, commit messages.
- Every commit must build and pass tests.
- Fill in the PR template — including the AI-disclosure field.
- No `Co-Authored-By` or tool attribution trailers in commits; AI
  disclosure lives in the PR body, not the git history.

## Scope notes

- `fixtures/` is frozen — changes only as part of a declared protocol
  revision.
- New features that need new wire actions are Phase-5 candidates —
  open an issue to discuss the spec before implementing.
- Historical references to prior implementations in comments are
  provenance, not authority.
