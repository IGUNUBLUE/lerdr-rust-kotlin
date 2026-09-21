#!/bin/bash
set -euo pipefail

# Prints the private phone setup link and QR for the Tailscale transport. The
# single argument is this machine's tailnet name; tailscale-serve.sh passes it
# after configuring Serve, and `link` callers get it from tailscale_fqdn.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

export PATH="/opt/homebrew/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:$HOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

ENV_FILE="$(relay_env_file "$SCRIPT_DIR")"

assert_service_env_matches "$ENV_FILE"
load_relay_env "$ENV_FILE"

relay_binary >/dev/null
if [ -z "${HERDR_RELAY_TOKEN:-}" ]; then
    echo "✗ No relay token in $ENV_FILE. Run Tailscale Serve setup first."
    exit 1
fi

TAILNET_HOST="${1:-}"
TAILNET_HOST="${TAILNET_HOST#https://}"
TAILNET_HOST="${TAILNET_HOST#wss://}"
TAILNET_HOST="${TAILNET_HOST%%/*}"
if [ -z "$TAILNET_HOST" ]; then
    echo "✗ Cannot determine this relay's tailnet name."
    echo "  Run scripts/tailscale-serve.sh start first, or pass the name directly:"
    echo "  scripts/setup-link.sh host.tail1234.ts.net"
    exit 1
fi

HOST_LABEL="$(host_label)"
RELAY_URL="wss://$TAILNET_HOST"
SETUP_FRAGMENT="$(build_setup_fragment "$HERDR_RELAY_TOKEN" "$HOST_LABEL" "$RELAY_URL")"
# The relay serves the app itself on the tailnet name, so the app base and the
# relay URL share one origin.
PHONE_APP_BASE="${LERDR_PHONE_APP_URL:-${HERDR_PHONE_APP_URL:-https://$TAILNET_HOST}}"

ARMED=0
arm_setup_link "$ENV_FILE" || ARMED=$?
echo "🐑 Lerdr phone setup"
echo ""
print_phone_setup "$PHONE_APP_BASE/#$SETUP_FRAGMENT"
echo ""
print_setup_link_arming "$ARMED"
echo "  The relay and tailscale serve must be running for the link to work:"
echo "  scripts/tailscale-serve.sh status"
