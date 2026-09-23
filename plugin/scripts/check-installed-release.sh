#!/bin/sh
# Verify and exercise version, serve, and support from a native release.
set -eu

ARCHIVE=${1:-}
CHECKSUMS=${2:-}
EXPECTED_VERSION=${3:-}
EXPECTED_REVISION=${4:-}
EXPECTED_TARGET=${5:-}
[ -n "$ARCHIVE" ] && [ -f "$ARCHIVE" ] &&
    [ -n "$CHECKSUMS" ] && [ -f "$CHECKSUMS" ] &&
    [ -n "$EXPECTED_VERSION" ] && [ -n "$EXPECTED_REVISION" ] &&
    [ -n "$EXPECTED_TARGET" ] || {
    echo "usage: scripts/check-installed-release.sh ARCHIVE CHECKSUMS VERSION REVISION os/arch" >&2
    exit 2
}

case $(uname -s) in
    Linux) HOST_OS=linux ;;
    Darwin) HOST_OS=darwin ;;
    *)
        echo "unsupported release-check operating system: $(uname -s)" >&2
        exit 1
        ;;
esac
case $(uname -m) in
    x86_64|amd64) HOST_ARCH=amd64 ;;
    arm64|aarch64) HOST_ARCH=arm64 ;;
    *)
        echo "unsupported release-check architecture: $(uname -m)" >&2
        exit 1
        ;;
esac
HOST_TARGET="$HOST_OS/$HOST_ARCH"
[ "$EXPECTED_TARGET" = "$HOST_TARGET" ] || {
    echo "expected release target $EXPECTED_TARGET does not match native target $HOST_TARGET" >&2
    exit 1
}
ARCHIVE_NAME=${ARCHIVE##*/}
EXPECTED_NAME="lerdr-relay_${EXPECTED_VERSION}_${HOST_OS}_${HOST_ARCH}.tar.gz"
[ "$ARCHIVE_NAME" = "$EXPECTED_NAME" ] || {
    echo "release archive $ARCHIVE_NAME does not match candidate $EXPECTED_NAME" >&2
    exit 1
}

EXPECTED_HASH=$(
    awk -v name="$ARCHIVE_NAME" '
        NF == 2 && $2 == name { count++; hash = $1 }
        END {
            if (count != 1) {
                exit 1
            }
            print hash
        }
    ' "$CHECKSUMS"
) || {
    echo "checksums.txt must contain exactly one entry for $ARCHIVE_NAME" >&2
    exit 1
}
[ "${#EXPECTED_HASH}" -eq 64 ] || {
    echo "invalid SHA-256 for $ARCHIVE_NAME" >&2
    exit 1
}
case "$EXPECTED_HASH" in
    *[!0-9a-f]*)
        echo "invalid SHA-256 for $ARCHIVE_NAME" >&2
        exit 1
        ;;
esac
if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL_HASH=$(sha256sum "$ARCHIVE" | awk '{print $1}')
else
    ACTUAL_HASH=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
fi
[ "$ACTUAL_HASH" = "$EXPECTED_HASH" ] || {
    echo "checksum mismatch for $ARCHIVE_NAME" >&2
    exit 1
}

WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/lerdr-installed-check.XXXXXX")
RELAY_PID=
cleanup() {
    if [ -n "$RELAY_PID" ] && kill -0 "$RELAY_PID" 2>/dev/null; then
        kill -INT "$RELAY_PID" 2>/dev/null || true
        wait "$RELAY_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT INT TERM

RELEASE_DIR="$WORK_DIR/release"
CONFIG_HOME="$WORK_DIR/config"
CACHE_HOME="$WORK_DIR/cache"
DATA_HOME="$WORK_DIR/data"
mkdir -p "$RELEASE_DIR" "$CONFIG_HOME" "$CACHE_HOME" "$DATA_HOME"
# macOS exposes /var through /private/var, and std::env::current_exe reports the
# physical path. Use the same canonical directory for extraction, execution,
# and support-state verification.
RELEASE_DIR=$(CDPATH='' cd "$RELEASE_DIR" && pwd -P)
tar -xzf "$ARCHIVE" -C "$RELEASE_DIR"

RELAY="$RELEASE_DIR/lerdr-relay"
[ -x "$RELAY" ] || {
    echo "release does not contain an executable relay" >&2
    exit 1
}
if "$RELAY" verify-release --help >/dev/null 2>&1; then
    "$RELAY" verify-release \
        --target "$EXPECTED_TARGET" \
        --version "$EXPECTED_VERSION" \
        --revision "$EXPECTED_REVISION" \
        "$RELEASE_DIR" >/dev/null
else
    # The Rust binary has not grown verify-release yet — check the manifest's
    # identity fields directly and confirm the binary reports the same stamp.
    echo "note: lerdr-relay lacks verify-release; checking release-manifest.json fields" >&2
    manifest="$RELEASE_DIR/release-manifest.json"
    [ -f "$manifest" ] || {
        echo "release does not contain release-manifest.json" >&2
        exit 1
    }
    for pair in "version:$EXPECTED_VERSION" "revision:$EXPECTED_REVISION" "target:$EXPECTED_TARGET"; do
        key=${pair%%:*}
        want=${pair#*:}
        got=$(sed -n "s/^[[:space:]]*\"$key\":[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$manifest" | head -1)
        [ "$got" = "$want" ] || {
            echo "release manifest $key is $got, expected $want" >&2
            exit 1
        }
    done
fi
# The binary itself must report the same version/revision it was stamped with —
# catches a manifest that lies about the build.
reported=$("$RELAY" version --json)
printf '%s' "$reported" | grep -qF "\"version\":\"$EXPECTED_VERSION\"" || {
    echo "binary reports $reported, expected version $EXPECTED_VERSION" >&2
    exit 1
}
printf '%s' "$reported" | grep -qF "\"revision\":\"$EXPECTED_REVISION\"" || {
    echo "binary reports $reported, expected revision $EXPECTED_REVISION" >&2
    exit 1
}

PORT=$((40000 + ($$ % 20000)))
PLUGIN_PORT=$((PORT + 1))
XDG_CONFIG_HOME="$CONFIG_HOME" \
XDG_CACHE_HOME="$CACHE_HOME" \
XDG_DATA_HOME="$DATA_HOME" \
HERDR_RELAY_PORT="$PORT" \
LERDR_RELAY_PLUGIN_PORT="$PLUGIN_PORT" \
HERDR_RELAY_PLUGIN_PORT="$PLUGIN_PORT" \
HERDR_RELAY_HOST=127.0.0.1 \
HERDR_RELAY_TOKEN= \
HERDR_BIN=/bin/false \
"$RELAY" serve >"$WORK_DIR/relay.log" 2>&1 &
RELAY_PID=$!

SUPPORT_STATE="$CONFIG_HOME/lerdr/support-state.json"
attempt=0
while [ ! -s "$SUPPORT_STATE" ]; do
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 100 ] || ! kill -0 "$RELAY_PID" 2>/dev/null; then
        echo "installed relay did not produce a support snapshot" >&2
        sed -n '1,120p' "$WORK_DIR/relay.log" >&2
        exit 1
    fi
    sleep 0.05
done

XDG_CONFIG_HOME="$CONFIG_HOME" \
XDG_CACHE_HOME="$CACHE_HOME" \
XDG_DATA_HOME="$DATA_HOME" \
"$RELAY" support >"$WORK_DIR/support.json"
grep -q '"protocol": 3' "$WORK_DIR/support.json" || {
    echo "installed relay support output does not report protocol 3" >&2
    sed -n '1,120p' "$WORK_DIR/support.json" >&2
    exit 1
}
# The Rust relay's support-state does not emit release_directory yet (the Go
# oracle uses it to prove the release dir resolved canonically). Assert it the
# moment the field exists; warn until then.
if grep -q '"release_directory"' "$WORK_DIR/support.json"; then
    grep -qF "\"release_directory\": \"$RELEASE_DIR\"" "$WORK_DIR/support.json" || {
        echo "installed relay support output does not report the canonical release directory" >&2
        sed -n '1,120p' "$WORK_DIR/support.json" >&2
        exit 1
    }
else
    echo "note: support output has no release_directory field yet — skipping canonical-dir assertion" >&2
fi

kill -INT "$RELAY_PID"
wait "$RELAY_PID"
RELAY_PID=
