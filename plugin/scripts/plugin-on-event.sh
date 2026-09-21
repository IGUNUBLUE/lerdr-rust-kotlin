#!/bin/sh
# Herdr event hook: a bounded local UDP send from the packaged Rust helper.
set -eu

RELEASE_ROOT=${LERDR_RELEASE_ROOT:-${HERDR_RELEASE_ROOT:-"${XDG_DATA_HOME:-$HOME/.local/share}/lerdr"}}
# The Rust binary is lerdr-relay; earlier bundles at this root carried the Go
# lerdr binary, and pre-rename installs used herdr-mobile-relay.
RELAY_BIN=${LERDR_RELAY_BIN:-${HERDR_RELAY_BIN:-}}
if [ -z "$RELAY_BIN" ] || [ ! -x "$RELAY_BIN" ]; then
    RELAY_BIN=
    for candidate in lerdr-relay lerdr herdr-mobile-relay; do
        if [ -x "$RELEASE_ROOT/current/$candidate" ]; then
            RELAY_BIN="$RELEASE_ROOT/current/$candidate"
            break
        fi
    done
fi
if [ -z "$RELAY_BIN" ]; then
    LEGACY_RELEASE_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/herdr-mobile-relay"
    for candidate in lerdr-relay lerdr herdr-mobile-relay; do
        if [ -x "$LEGACY_RELEASE_ROOT/current/$candidate" ]; then
            RELAY_BIN="$LEGACY_RELEASE_ROOT/current/$candidate"
            break
        fi
    done
fi
if [ -z "$RELAY_BIN" ]; then
    echo "lerdr: verified relay release is unavailable" >&2
    exit 1
fi
exec "$RELAY_BIN" event-hook
