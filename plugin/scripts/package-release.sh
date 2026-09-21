#!/bin/sh
# Build complete, manifest-verified release archives for all supported targets.
# Linux artifacts are static musl binaries — plugin build hosts are minimal by
# design and must never need a Rust toolchain or a matching glibc.
set -eu

SCRIPT_DIR=${0%/*}
if [ "$SCRIPT_DIR" = "$0" ]; then
    SCRIPT_DIR=.
fi
SCRIPT_DIR=$(CDPATH='' cd "$SCRIPT_DIR" && pwd)
# plugin/ is the plugin root; the cargo workspace lives at relay/ beside it.
PLUGIN_DIR=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd)
REPO_DIR=$(CDPATH='' cd "$PLUGIN_DIR/.." && pwd)
WORKSPACE="$REPO_DIR/relay"
VERSION=${1:-}
REVISION=${2:-}
OUTPUT_DIR=${3:-"$REPO_DIR/dist/release"}

[ -n "$VERSION" ] || {
    echo "usage: scripts/package-release.sh VERSION REVISION [OUTPUT_DIR]" >&2
    exit 2
}
[ -n "$REVISION" ] || {
    echo "usage: scripts/package-release.sh VERSION REVISION [OUTPUT_DIR]" >&2
    exit 2
}
case "$VERSION" in
    v*) VERSION=${VERSION#v} ;;
esac
SOURCE_VERSION=$(sed -n 's/^version = "\([0-9.]*\)"$/\1/p' "$PLUGIN_DIR/herdr-plugin.toml")
[ "$VERSION" = "$SOURCE_VERSION" ] || {
    echo "requested release version $VERSION does not match herdr-plugin.toml $SOURCE_VERSION" >&2
    exit 1
}

command -v cargo >/dev/null 2>&1 || {
    echo "cargo is required on the release builder" >&2
    exit 1
}
command -v tar >/dev/null 2>&1 || {
    echo "tar is required on the release builder" >&2
    exit 1
}

# The build host needs the native toolchain for the manifest tool plus every
# cross target below (`rustup target add`, a musl linker for linux targets,
# and an Apple SDK for darwin targets — usually run on their native CI hosts).
command -v rustup >/dev/null 2>&1 || {
    echo "rustup is required on the release builder to check installed targets" >&2
    exit 1
}
for RUST_TARGET in \
    x86_64-unknown-linux-musl \
    aarch64-unknown-linux-musl \
    x86_64-apple-darwin \
    aarch64-apple-darwin; do
    rustup target list --installed 2>/dev/null | grep -qx "$RUST_TARGET" || {
        echo "missing Rust target $RUST_TARGET (rustup target add $RUST_TARGET)" >&2
        exit 1
    }
done

mkdir -p "$OUTPUT_DIR"
WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/lerdr-release.XXXXXX")
trap 'rm -rf "$WORK_DIR"' EXIT INT TERM

# LERDR_BUILD_VERSION/LERDR_BUILD_REVISION stamp the binaries at compile time;
# lerdr-relay must read them (build.rs/option_env!) so `verify-release`,
# `release-manifest`, and /healthz report the release identity.
export LERDR_BUILD_VERSION="$VERSION"
export LERDR_BUILD_REVISION="$REVISION"

# Native build of the same crate drives release-manifest/verify-release on this
# host; the per-target binaries are verified offline inside their archives.
cargo build \
    --release \
    --manifest-path "$WORKSPACE/Cargo.toml" \
    -p lerdr-relay
RELEASE_TOOL="$WORKSPACE/target/release/lerdr-relay"
[ -x "$RELEASE_TOOL" ] || {
    echo "could not locate the lerdr-relay host build" >&2
    exit 1
}

for TARGET in linux/amd64 linux/arm64 darwin/amd64 darwin/arm64; do
    GOOS=${TARGET%/*}
    GOARCH=${TARGET#*/}
    case "$TARGET" in
        linux/amd64)   RUST_TARGET=x86_64-unknown-linux-musl ;;
        linux/arm64)   RUST_TARGET=aarch64-unknown-linux-musl ;;
        darwin/amd64)  RUST_TARGET=x86_64-apple-darwin ;;
        darwin/arm64)  RUST_TARGET=aarch64-apple-darwin ;;
    esac
    ARCHIVE="lerdr-relay_${VERSION}_${GOOS}_${GOARCH}.tar.gz"
    STAGE="$WORK_DIR/${GOOS}-${GOARCH}"
    mkdir -p "$STAGE/scripts"

    cargo build \
        --release \
        --manifest-path "$WORKSPACE/Cargo.toml" \
        -p lerdr-relay \
        --target "$RUST_TARGET"
    cp "$WORKSPACE/target/$RUST_TARGET/release/lerdr-relay" "$STAGE/lerdr-relay"
    cp "$REPO_DIR/README.md" "$STAGE/README.md"
    # Ship the operator-facing subset of scripts/ inside the release — the same
    # set the Go packaging carried under relay/.
    for WRAPPER in \
        common.sh \
        install-tailscale-service.sh \
        plugin-on-event.sh \
        plugin-on-startup.sh \
        rotate-token.sh \
        setup-link.sh \
        tailscale-serve.sh \
        tailscale-service.sh \
        uninstall.sh \
        uninstall-service.sh \
        uninstall-systemd-user-service.sh; do
        cp "$SCRIPT_DIR/$WRAPPER" "$STAGE/scripts/$WRAPPER"
    done
    "$RELEASE_TOOL" release-manifest "$STAGE" "$VERSION" "$REVISION" "$TARGET" >/dev/null
    # This host tool cannot execute cross-built binaries; native CI verifies each extracted executable.
    "$RELEASE_TOOL" verify-release --allow-cross-target --target "$TARGET" "$STAGE" >/dev/null
    tar -C "$STAGE" -czf "$OUTPUT_DIR/$ARCHIVE" .
done

CHECKSUMS="$OUTPUT_DIR/checksums.txt"
: > "$CHECKSUMS"
for ARCHIVE in "$OUTPUT_DIR"/lerdr-relay_"$VERSION"_*.tar.gz; do
    NAME=${ARCHIVE##*/}
    if command -v sha256sum >/dev/null 2>&1; then
        HASH=$(sha256sum "$ARCHIVE" | awk '{print $1}')
    else
        HASH=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
    fi
    printf '%s  %s\n' "$HASH" "$NAME" >> "$CHECKSUMS"
done

echo "Release bundles written to $OUTPUT_DIR"
