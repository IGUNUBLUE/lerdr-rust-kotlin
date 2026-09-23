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
# Which os/arch pairs to build. Cross builds are limited by the toolchain on
# the host (darwin needs an Apple SDK, musl needs a musl C toolchain), so the
# default is the host's own target; CI passes LERDR_RELEASE_TARGETS per runner
# and stitches the per-target checksums together at publish time.
if [ -z "${LERDR_RELEASE_TARGETS:-}" ]; then
    case "$(uname -s)/$(uname -m)" in
        Linux/x86_64|Linux/amd64) LERDR_RELEASE_TARGETS="linux/amd64" ;;
        Linux/arm64|Linux/aarch64) LERDR_RELEASE_TARGETS="linux/arm64" ;;
        Darwin/x86_64) LERDR_RELEASE_TARGETS="darwin/amd64" ;;
        Darwin/arm64) LERDR_RELEASE_TARGETS="darwin/arm64" ;;
        *)
            echo "unsupported release host: $(uname -s)/$(uname -m)" >&2
            exit 1
            ;;
    esac
fi
for TARGET in $LERDR_RELEASE_TARGETS; do
    case "$TARGET" in
        linux/amd64|linux/arm64|darwin/amd64|darwin/arm64) ;;
        *)
            echo "unknown release target $TARGET (expected os/arch of linux|darwin/amd64|arm64)" >&2
            exit 2
            ;;
    esac
done

command -v rustup >/dev/null 2>&1 || {
    echo "rustup is required on the release builder to check installed targets" >&2
    exit 1
}
for TARGET in $LERDR_RELEASE_TARGETS; do
    case "$TARGET" in
        linux/amd64)   RUST_TARGET=x86_64-unknown-linux-musl ;;
        linux/arm64)   RUST_TARGET=aarch64-unknown-linux-musl ;;
        darwin/amd64)  RUST_TARGET=x86_64-apple-darwin ;;
        darwin/arm64)  RUST_TARGET=aarch64-apple-darwin ;;
    esac
    rustup target list --installed 2>/dev/null | grep -qx "$RUST_TARGET" || {
        echo "missing Rust target $RUST_TARGET (rustup target add $RUST_TARGET)" >&2
        exit 1
    }
    case "$TARGET" in
        linux/*)
            # ring's build script needs a musl C compiler, and rustc drives
            # the final link through a C toolchain whenever C objects are
            # present — both must be musl-aware. cc-rs probes
            # <arch>-linux-musl-gcc itself; a bare musl-gcc (distro
            # musl-tools) matches only the host arch, so pin both env vars.
            cc_env="CC_$(printf '%s' "$RUST_TARGET" | tr '-' '_')"
            linker_env="CARGO_TARGET_$(printf '%s' "$RUST_TARGET" | tr 'a-z-' 'A-Z_')_LINKER"
            eval "cc_set=\${$cc_env:-}"
            eval "linker_set=\${$linker_env:-}"
            if [ -z "$cc_set" ] || [ -z "$linker_set" ]; then
                if command -v "${RUST_TARGET%%-*}-linux-musl-gcc" >/dev/null 2>&1; then
                    musl_cc="${RUST_TARGET%%-*}-linux-musl-gcc"
                elif [ "${RUST_TARGET%%-*}" = "$(uname -m)" ] &&
                    command -v musl-gcc >/dev/null 2>&1; then
                    musl_cc=musl-gcc
                else
                    echo "linux musl builds need a musl C toolchain (apt install musl-tools) or $cc_env" >&2
                    exit 1
                fi
                [ -n "$cc_set" ] || eval "export $cc_env=$musl_cc"
                [ -n "$linker_set" ] || eval "export $linker_env=$musl_cc"
            fi
            ;;
    esac
done

mkdir -p "$OUTPUT_DIR"
WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/lerdr-release.XXXXXX")
trap 'rm -rf "$WORK_DIR"' EXIT INT TERM

# LERDR_VERSION/LERDR_REVISION stamp the binaries at compile time — the
# release pipeline sets them so `release_version()` (lerdr-core) and the
# `version`/`support` subcommands report the release identity. Cargo tracks
# option_env! reads in dep-info, so stamped builds rebuild cleanly.
export LERDR_VERSION="$VERSION"
export LERDR_REVISION="$REVISION"

# The lerdr-relay binary lives in the lerdr-coord package (src/bin/) — `-p
# lerdr-relay` would only build the library crate and produce no binary.
# Native build of the same crate drives release-manifest/verify-release on
# this host; the per-target binaries are verified offline inside their
# archives.
cargo build \
    --release --locked \
    --manifest-path "$WORKSPACE/Cargo.toml" \
    -p lerdr-coord --bin lerdr-relay
RELEASE_TOOL="$WORKSPACE/target/release/lerdr-relay"
[ -x "$RELEASE_TOOL" ] || {
    echo "could not locate the lerdr-relay host build" >&2
    exit 1
}

# The binary's own release-manifest/verify-release subcommands (the oracle's
# internal/release contract) are authoritative once they land; until then
# scripts/release-manifest.py produces the identical schema-1 manifest.
if "$RELEASE_TOOL" release-manifest --help >/dev/null 2>&1 &&
    "$RELEASE_TOOL" verify-release --help >/dev/null 2>&1; then
    manifest_tool() { "$RELEASE_TOOL" release-manifest "$@" >/dev/null; }
    verify_tool() { "$RELEASE_TOOL" verify-release --allow-cross-target "$@" >/dev/null; }
else
    command -v python3 >/dev/null 2>&1 || {
        echo "python3 is required on the release builder until lerdr-relay grows release-manifest/verify-release" >&2
        exit 1
    }
    MANIFEST_PY="$REPO_DIR/scripts/release-manifest.py"
    manifest_tool() {
        python3 "$MANIFEST_PY" build "$@"
    }
    verify_tool() {
        # python verify ROOT --target T — map the binary's flag order.
        shift_dir=
        shift_target=
        while [ $# -gt 0 ]; do
            case "$1" in
                --target) shift_target=$2; shift 2 ;;
                *) shift_dir=$1; shift ;;
            esac
        done
        python3 "$MANIFEST_PY" verify "$shift_dir" --target "$shift_target"
    }
fi

for TARGET in $LERDR_RELEASE_TARGETS; do
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
        --release --locked \
        --manifest-path "$WORKSPACE/Cargo.toml" \
        -p lerdr-coord --bin lerdr-relay \
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
    manifest_tool "$STAGE" "$VERSION" "$REVISION" "$TARGET"
    # This host tool cannot execute cross-built binaries; native CI verifies each extracted executable.
    verify_tool --target "$TARGET" "$STAGE"
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
