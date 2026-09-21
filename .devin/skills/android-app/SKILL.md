---
name: android-app
description: "Trigger: kotlin, compose, android app, module, viewmodel, material expressive, m3e, composable. Conventions for the Kotlin app: nowinandroid modules, UDF, Compose performance, M3E isolation."
license: Apache-2.0
metadata:
  author: "IGUNUBLUE"
  version: "1.0"
---

# Android App Conventions

## Activation Contract

Use when creating/editing modules under `app/` (Gradle) — features, core
libraries, Compose UI.

## Hard Rules

- **Modules**: single feature modules (no api/impl split — Nav3 pattern), `core:*` for shared, `navigation` owns entries, `app` wires. A class used by one feature stays in that feature.
- **UDF**: repositories expose `Flow`, never suspend-gets; ViewModel per screen with `StateFlow` UI state; UI collects via `collectAsStateWithLifecycle`. Events down, data up.
- **Compose perf**: `@Immutable`/`@Stable` on models entering composition; `key()` on all lazy items; defer state reads into layout/draw phases; no `remember { mutableStateOf(heavy) }` recomputation traps.
- **M3 Expressive isolation**: expressive-alpha composables only inside `core:designsystem` wrappers with stable signatures; feature code never imports experimental M3E APIs directly.
- **No mocking libraries** — fakes live in `core:testing` with test-only hooks (nowinandroid convention); real `DataStore` on a temp folder, never mocked.
- **Secrets**: credentials via Keystore-backed store only; never DataStore plaintext, never logs.
- **Foreground service**: `dataSync` type, persistent notification, restart on `onTaskRemoved`; battery-exemption UX gated behind settings.
- **Testing**: `runTest` + `UnconfinedTestDispatcher`; Turbine for multi-emission Flows; Roborazzi on Robolectric RNG (`@GraphicsMode(NATIVE)`) — never Paparazzi (incompatible with Hilt/Robolectric); protocol conformance = fixture replay from `fixtures/`, not hand-written cases.
- **Terminal rendering**: no per-line `LazyColumn` for 60fps deltas — custom `Layout`/canvas over a ring-buffer model, `drawWithCache`, invalidate by fingerprint ranges.
- **Profiles**: macrobenchmark generates Baseline + Startup Profiles; Compose compiler metrics/stability reports diff-reviewed in CI.

## Decision Gates

| Need | Where |
|---|---|
| Wire DTO | `core:model` (kotlinx.serialization) |
| Terminal semantics | `core:terminal` — pure Kotlin, fixture-tested |
| Screen state | feature VM; session/connectivity lives in `core:service`+`core:data` |
| Expressive component | `core:designsystem` wrapper |

## Output Contract

New modules: register in `settings.gradle.kts` + `README.md` module map. UI changes: Roborazzi screenshot in PR when visual.

## References

- `docs/04-app-design.md` — screens + parity checklist.
- `docs/08-herdr-boundary.md` — module layout rationale.
- `docs/11-stack-practices.md` — M3E alpha state, Compose perf rules, testing matrix.
- Installed skills: `navigation-3`, `testing-setup` (official android/skills).
