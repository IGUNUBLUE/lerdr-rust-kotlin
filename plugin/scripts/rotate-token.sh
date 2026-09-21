#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

export PATH="/opt/homebrew/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:$HOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

ENV_FILE="$(relay_env_file "$SCRIPT_DIR")"

echo "🐑 Lerdr token rotation"
echo ""

assert_service_env_matches "$ENV_FILE"
if [ ! -f "$ENV_FILE" ]; then
    echo "✗ $ENV_FILE does not exist. Run the Setup action first."
    exit 1
fi

NEW_TOKEN="$(generate_token)"
# Both spellings get the same token so a rollback to a pre-rename binary keeps
# working.
set_env_value_atomic "$ENV_FILE" HERDR_RELAY_TOKEN "$NEW_TOKEN"
set_env_value_atomic "$ENV_FILE" LERDR_RELAY_TOKEN "$NEW_TOKEN"

echo "✓ Wrote a new relay token to $ENV_FILE"
echo "  Phones configured with the old token stop working once the relay restarts."
echo ""

# Restart the background service when one is installed so the new token takes
# effect immediately; otherwise the next relay start picks it up.
RESTARTED=""
case "$(uname -s)" in
    Darwin)
        for label in com.lerdr.service com.herdr-mobile-relay.service; do
            SERVICE="gui/$(id -u)/$label"
            if launchctl print "$SERVICE" >/dev/null 2>&1; then
                launchctl kickstart -k "$SERVICE"
                RESTARTED=1
                break
            fi
        done
        ;;
    Linux)
        for unit in lerdr.service herdr-mobile-relay.service; do
            if systemctl --user cat "$unit" >/dev/null 2>&1; then
                systemctl --user restart "$unit"
                RESTARTED=1
                break
            fi
        done
        ;;
esac
if [ -n "$RESTARTED" ]; then
    echo "✓ Restarted the background service with the new token."
else
    echo "  No background service found. Restart the relay (or rerun Tailscale"
    echo "  Serve setup) to apply the new token."
fi
echo ""

# Re-add the relay on each phone with the new token. tailscale-serve.sh link
# fails cleanly when Serve is not configured yet.
if ! "$SCRIPT_DIR/tailscale-serve.sh" link; then
    echo ""
    echo "  Run Tailscale Serve setup first, then reprint the QR code."
fi
