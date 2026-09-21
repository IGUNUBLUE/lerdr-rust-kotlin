#!/bin/sh
# Herdr startup hook: fires after session restore and server.live_handoff.
# agent.view.set projections are transient and per-server, so the relay
# re-asserts its canonical view, subscriptions, and socket state from here —
# the same defense-in-depth path as plugin-on-event.sh. herdr runs this for
# you — never invoke it by hand.
set -eu

RELEASE_ROOT=${LERDR_RELEASE_ROOT:-${HERDR_RELEASE_ROOT:-"${XDG_DATA_HOME:-$HOME/.local/share}/lerdr"}}
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
exec "$RELAY_BIN" startup-hook
