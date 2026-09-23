## Summary

<!-- What does this change and why? Link the issue if one exists. -->

## Verification

<!-- Paste the commands you ran and their results. Required — see
     CONTRIBUTING.md. Examples: cargo test -p <crate>, ./gradlew test,
     :app:assembleDebug, Roborazzi screenshots for UI changes. -->

- [ ] `cargo fmt --check` + `cargo clippy -D warnings` clean (Rust changes)
- [ ] Relevant tests pass (`cargo test -p …` / `./gradlew test`)
- [ ] `:app:assembleDebug` still green (app changes)
- [ ] Screenshot test added/updated (visual changes)

## AI disclosure

<!-- This project is AI-built — AI-assisted PRs are welcome when disclosed.
     Which tool/model produced or assisted this change? "None, hand-written"
     is a fine answer. -->

**AI involvement:** <!-- e.g. "SWE-2 (Devin), full implementation" / "Claude, tests only" / "None" -->

- [ ] I understand and can explain every line of this diff
- [ ] I ran the verification steps above myself

## Scope check

- [ ] This PR is one coherent unit (no unrelated changes bundled)
- [ ] No changes to `fixtures/` (frozen) unless this is a declared protocol revision
- [ ] No commit attribution trailers (`Co-Authored-By`, tool credits in history)
