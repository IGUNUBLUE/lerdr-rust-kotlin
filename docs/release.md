# Cutting a release

Tag `v<version>` on `main` and `.github/workflows/release.yml` builds the
bundles, smokes them on native runners, and publishes a GitHub release.
`plugin-build.sh` then installs that exact version — no Rust toolchain on
user hosts.

## Version bumps (in the release PR — never at build time)

Three version sources must agree; `scripts/check-version-sync.sh` verifies
them (also runs in `relay.yml` on every PR):

1. `plugin/herdr-plugin.toml` → `version = "x.y.z"`
2. Every `relay/crates/*/Cargo.toml` → `[package] version` (or
   `relay/Cargo.toml` → `[workspace.package] version` once the workspace
   owns it — the checker handles both)
3. The git tag `vx.y.z` (checked by the release workflow's verify job)

Release PR should also refresh `relay/Cargo.lock` after bumping and may bump
`versionName`/`versionCode` in `app/app/build.gradle.kts` (currently a
static `0.1.0` — the APK's embedded version does not follow the tag yet).

Then:

```sh
git tag v<x.y.z> <commit-on-main>
git push origin v<x.y.z>
```

## What the pipeline produces

- `lerdr-relay_<v>_<os>_<arch>.tar.gz` — linux amd64/arm64 (static musl),
  darwin amd64/arm64. Each tarball: `lerdr-relay` binary stamped with
  `LERDR_VERSION`/`LERDR_REVISION`, `README.md`, the operator `scripts/*.sh`
  wrappers, and `release-manifest.json` (manifest schema 1).
- `checksums.txt` — sha256 over all tarballs; `install.sh` refuses a release
  without it.
- `lerdr_<v>_universal.apk` — only when `ANDROID_KEYSTORE_BASE64` +
  `ANDROID_KEYSTORE_PASSWORD` + `ANDROID_KEY_ALIAS` + `ANDROID_KEY_PASSWORD`
  secrets are configured. Otherwise a zipaligned **unsigned** APK is kept as
  a workflow artifact (`lerdr_<v>_universal-unsigned.apk`) and is not
  attached to the release.
- The release is `--latest` when the tag is on `main`, `--prerelease`
  otherwise (the in-app update check skips prereleases).

## Gates before publish

1. `verify` — version sync + every check workflow that ran for the tagged
   commit (`fixture-check.yml`, `relay.yml`, `app.yml`) must be green.
   Path-filtered workflows that never ran for the commit do not block.
2. `native-smoke` — each tarball is extracted on a runner of its own
   platform: checksum, manifest, `version` stamp, `serve` boot +
   `support-state.json`, clean shutdown.
3. Publish replaces assets on re-run (`--clobber`) and refuses to republish a
   promoted release from a non-main commit.

## Manual / heavy lanes (never on PRs)

`interop.yml` holds the gates that need real infrastructure:

- `shadow-self` — rust-vs-rust determinism, runs on `relay/**`/`tools/shadow/**`
  PRs and nightly.
- `live` — `workflow_dispatch` onto a self-hosted machine with a live Herdr
  socket: `HERDR_LIVE=1` (`lerdr-herdr`), `LERDR_LIVE=1` (`lerdr-coord`), and
  `LERDR_RUST_INTEROP=1` (app spawns the freshly built `lerdr-relay`).
- `probe` — `workflow_dispatch` or nightly; hermetic on hosted runners:
  builds `lerdr-relay`, starts it on `127.0.0.1:8377` with a generated
  32-byte token, runs `WireProbeTest`.

## Known gaps (tracked for the relay crates)

- `lerdr-relay` has no `release-manifest`/`verify-release`/`seal-release`/
  `prune-releases`/`activate-release` subcommands yet. Packaging falls back
  to `scripts/release-manifest.py` (identical manifest schema) and the
  install-time checks degrade to manifest-field + `version --json` stamps —
  both probe for the subcommands and switch to them automatically when they
  land. `install.sh` still requires the binary-side subcommands at install
  time, so full plugin installs stay blocked until they exist.
- `support-state.json` does not emit `release_directory` yet — the smoke
  check asserts it once the field exists.
