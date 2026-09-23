#!/bin/sh
# Version-sync gate for releases.
#
# The Herdr plugin manifest (`plugin/herdr-plugin.toml`), every workspace
# crate (`relay/crates/*/Cargo.toml`, or `workspace.package.version` when the
# workspace owns it), and the release tag must agree. Release PRs bump all
# three by hand — doc 05/09: "bump in the release PR, never at build time".
# This script only verifies; it never rewrites.
#
# usage: scripts/check-version-sync.sh [TAG]
#   TAG is optional; when given ("v1.2.3" or "1.2.3") it must equal the
#   manifest version too.
set -eu

SCRIPT_DIR=${0%/*}
if [ "$SCRIPT_DIR" = "$0" ]; then
    SCRIPT_DIR=.
fi
REPO_DIR=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd)
MANIFEST="$REPO_DIR/plugin/herdr-plugin.toml"
WORKSPACE="$REPO_DIR/relay/Cargo.toml"

TAG=${1:-}
case "$TAG" in
    v*) TAG=${TAG#v} ;;
esac

# First bare `version = "..."` in the manifest — `min_herdr_version` and the
# sectioned keys never match this anchor.
manifest_version=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$MANIFEST" | head -1)
[ -n "$manifest_version" ] || {
    echo "check-version-sync: no version in $MANIFEST" >&2
    exit 1
}

# A `[package]`/`[workspace.package]` `version = "..."` line. Cargo.toml files
# may instead hold `version.workspace = true`, which resolves to the
# workspace value — reported as "@workspace" for the caller to substitute.
package_version() {
    awk '
        /^\[[^]]*\]/ { inpkg = ($0 == "[package]" || $0 == "[workspace.package]") }
        inpkg && /^version[[:space:]]*\.[[:space:]]*workspace[[:space:]]*=/ { print "@workspace"; exit }
        inpkg && /^version[[:space:]]*=[[:space:]]*"/ {
            line = $0
            sub(/^[^"]*"/, "", line)
            sub(/".*$/, "", line)
            print line
            exit
        }
    ' "$1"
}

workspace_version=$(package_version "$WORKSPACE")

fail=0
check() {
    # $1 = label, $2 = version (literal "@workspace" resolves to the workspace value)
    version=$2
    [ "$version" = "@workspace" ] && version=$workspace_version
    if [ -z "$version" ]; then
        echo "check-version-sync: no version found in $1" >&2
        fail=1
        return
    fi
    if [ "$version" != "$manifest_version" ]; then
        echo "check-version-sync: $1 = $version, manifest = $manifest_version" >&2
        fail=1
    fi
}

if [ -n "$workspace_version" ] && [ "$workspace_version" != "@workspace" ]; then
    check "relay/Cargo.toml [workspace.package]" "$workspace_version"
fi

for crate_toml in "$REPO_DIR"/relay/crates/*/Cargo.toml; do
    crate=${crate_toml%/Cargo.toml}
    check "${crate##"$REPO_DIR"/}" "$(package_version "$crate_toml")"
done

if [ -n "$TAG" ] && [ "$TAG" != "$manifest_version" ]; then
    echo "check-version-sync: tag v$TAG does not match manifest $manifest_version" >&2
    fail=1
fi

[ "$fail" -eq 0 ] || {
    echo "check-version-sync: versions must agree — bump them in the release PR" >&2
    exit 1
}
echo "check-version-sync: $manifest_version is in sync${TAG:+ (tag v$TAG)}"
