#!/bin/bash
set -euo pipefail

# LaunchAgent wrapper for the Tailscale transport: tailscaled owns the tailnet
# listener and persists the serve configuration, so the supervised process is
# just the relay. launchd KeepAlive restarts it; there is no tunnel child.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

ENV_FILE="$(relay_env_file "$SCRIPT_DIR")"

if [ -f "$ENV_FILE" ]; then
    set -a
    # shellcheck source=/dev/null
    . "$ENV_FILE"
    set +a
fi

PATH="/opt/homebrew/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:$HOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
# Agents often install their CLI into a per-tool bin directory that the service
# PATH does not include (e.g. ~/.opencode/bin). Append any that exist so the
# relay can detect them; base entries keep precedence.
for agent_bin in "$HOME"/.[!.]*/bin; do
    [ -d "$agent_bin" ] && PATH="$PATH:$agent_bin"
done
export PATH
# Both spellings go to the relay process: LERDR_ is canonical, HERDR_ keeps a
# rolled-back pre-rename binary working off the same env file.
export LERDR_RELAY_HOST="${LERDR_RELAY_HOST:-${HERDR_RELAY_HOST:-127.0.0.1}}"
export HERDR_RELAY_HOST="$LERDR_RELAY_HOST"
export LERDR_RELAY_PORT="${LERDR_RELAY_PORT:-${HERDR_RELAY_PORT:-8375}}"
export HERDR_RELAY_PORT="$LERDR_RELAY_PORT"

if [ -z "${HERDR_BIN:-}" ] && command -v herdr >/dev/null 2>&1; then
    HERDR_BIN="$(command -v herdr)"
    export HERDR_BIN
fi

exec "$(relay_binary)" serve
