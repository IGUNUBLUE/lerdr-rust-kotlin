# relay/tests

Cross-crate harnesses:

- `vectors/` — fixture conformance: every suite under `../../fixtures/`
  decoded and asserted. Harness crate: `lerdr-fixture` (JSON suite loader)
  + per-domain test crates. A virtual workspace cannot host test targets,
  so the conformance suites live per crate — e.g. `crypto.*` in
  `crates/lerdr-e2ee/tests/vectors/`.
- `interop/` — Rust↔Go and Rust↔Kotlin runtime checks (phase 3+).
