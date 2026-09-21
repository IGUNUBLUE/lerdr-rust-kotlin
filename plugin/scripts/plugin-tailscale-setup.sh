#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

SERVICE_ENV="$(installed_service_env_file)"
if [ -n "$SERVICE_ENV" ]; then
    export LERDR_RELAY_ENV="$SERVICE_ENV"
fi

if ! "$SCRIPT_DIR/tailscale-serve.sh" start; then
    echo ""
    echo "Tailscale setup did not complete. Check tailscale status, then rerun."
    pause_before_close
    exit 1
fi

pause_before_close
