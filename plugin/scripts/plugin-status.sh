#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${HERDR_BIN_PATH:-}" ]; then
    export HERDR_BIN="$HERDR_BIN_PATH"
fi

export PATH="/opt/homebrew/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:$HOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

ENV_FILE="$(relay_env_file "$SCRIPT_DIR")"
load_relay_env "$ENV_FILE"

echo "🐑 Lerdr status"
echo ""
echo "  Config file:  $ENV_FILE"
if [ -f "$ENV_FILE" ] && grep -qE '^(LERDR|HERDR)_RELAY_TOKEN=..*' "$ENV_FILE"; then
    echo "  Relay token:  present"
else
    echo "  Relay token:  missing — run the Quick Start action once"
fi

case "$(uname -s)" in
    Darwin)
        SERVICE_FILE=""
        SERVICE_LABEL=""
        for label in com.lerdr.service com.herdr-mobile-relay.service; do
            if [ -f "$HOME/Library/LaunchAgents/$label.plist" ]; then
                SERVICE_FILE="$HOME/Library/LaunchAgents/$label.plist"
                SERVICE_LABEL="$label"
                break
            fi
        done
        if [ -z "$SERVICE_FILE" ]; then
            echo "  Service:      not installed"
        elif launchctl print "gui/$(id -u)/$SERVICE_LABEL" >/dev/null 2>&1; then
            echo "  Service:      installed (active)"
        else
            echo "  Service:      installed (inactive)"
        fi
        ;;
    Linux)
        SERVICE_FILE=""
        SERVICE_LABEL=""
        for label in lerdr.service herdr-mobile-relay.service; do
            if [ -f "$HOME/.config/systemd/user/$label" ]; then
                SERVICE_FILE="$HOME/.config/systemd/user/$label"
                SERVICE_LABEL="$label"
                break
            fi
        done
        if [ -z "$SERVICE_FILE" ]; then
            echo "  Service:      not installed"
        else
            SERVICE_STATE="$(systemctl --user is-active "$SERVICE_LABEL" 2>/dev/null || true)"
            if [ -n "$SERVICE_STATE" ]; then
                echo "  Service:      installed ($SERVICE_STATE)"
            else
                echo "  Service:      installed (status unavailable)"
            fi
        fi
        ;;
esac
SERVICE_ENV="$(installed_service_env_file)"
if [ -n "$SERVICE_ENV" ]; then
    echo "  Service env:  $SERVICE_ENV"
fi

PORT="${LERDR_RELAY_PORT:-${HERDR_RELAY_PORT:-8375}}"
if HEALTH="$(curl -fsS --max-time 3 "http://127.0.0.1:$PORT/healthz" 2>/dev/null)"; then
    echo "  Relay health: $HEALTH"
else
    echo "  Relay health: not reachable on 127.0.0.1:$PORT — is the relay running?"
fi

echo ""
TAILSCALE_BIN="${LERDR_TAILSCALE_BIN:-${HERDR_TAILSCALE_BIN:-}}"
if [ -z "$TAILSCALE_BIN" ] && command -v tailscale >/dev/null 2>&1; then
    TAILSCALE_BIN="$(command -v tailscale)"
fi
if [ -z "$TAILSCALE_BIN" ] && [ -x "/Applications/Tailscale.app/Contents/MacOS/Tailscale" ]; then
    TAILSCALE_BIN="/Applications/Tailscale.app/Contents/MacOS/Tailscale"
fi
if [ -n "$TAILSCALE_BIN" ]; then
    echo "  Tailscale serve:"
    "$TAILSCALE_BIN" serve status 2>/dev/null | sed 's/^/    /' ||
        echo "    unavailable"
else
    echo "  Tailscale:    CLI not found — install Tailscale to expose the relay"
fi

echo ""
echo "  Sanitized support snapshot:"
"$(relay_binary)" support 2>/dev/null || true

pause_before_close
