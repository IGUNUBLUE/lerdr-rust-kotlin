#!/bin/bash
set -euo pipefail

# Rebuilds the checkout's release binary and restarts lerdr.service under it.
# This is the dev-checkout counterpart to install-tailscale-service.sh: the
# installed unit must already point at LERDR_RELAY_BIN=.../target/release/
# lerdr-relay (the installer bakes that path into ExecStart), otherwise it is
# reinstalled to do so.
#
#   scripts/deploy-relay.sh          build + restart + health check
#   SKIP_BUILD=1 scripts/deploy-relay.sh   restart against the existing binary

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RELAY_DIR="$REPO_ROOT/relay"
RELAY_BIN="$RELAY_DIR/target/release/lerdr-relay"
UNIT_FILE="$HOME/.config/systemd/user/lerdr.service"

export PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin:$HOME/.cargo/bin:$PATH"

# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

ENV_FILE="$(relay_env_file "$SCRIPT_DIR")"
load_relay_env "$ENV_FILE"
PORT="${LERDR_RELAY_PORT:-${HERDR_RELAY_PORT:-8375}}"

if [ "${SKIP_BUILD:-0}" != 1 ]; then
    echo "Building release binary..."
    (cd "$RELAY_DIR" && cargo build --release -p lerdr-coord --bin lerdr-relay)
fi
[ -x "$RELAY_BIN" ] || { echo "missing $RELAY_BIN (build failed?)" >&2; exit 1; }

# The unit must exec this checkout's binary; reinstall when it points
# elsewhere (packaged install or first-time dev deploy).
if [ ! -f "$UNIT_FILE" ] || ! grep -qF "ExecStart=$RELAY_BIN serve" "$UNIT_FILE"; then
    echo "Installing lerdr.service against $RELAY_BIN..."
    LERDR_RELAY_BIN="$RELAY_BIN" "$SCRIPT_DIR/install-tailscale-service.sh"
    exit 0
fi

echo "Restarting lerdr.service..."
systemctl --user restart lerdr.service

if HEALTH="$(wait_for_relay_health "$PORT")"; then
    echo "Relay health: $HEALTH"
else
    echo "Relay did not become healthy on 127.0.0.1:$PORT" >&2
    echo "  journalctl --user -u lerdr.service -n 80 --no-pager" >&2
    exit 1
fi
