#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"
# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

ENV_FILE="$(relay_env_file "$SCRIPT_DIR")"
load_relay_env "$ENV_FILE"

# The menu opens after every install, so it has to answer "what do I have" before
# it asks "what next". Every probe is bounded and optional: a status line that
# cannot be determined is omitted, never fatal.
installed_release_version() {
    local manifest="$(relay_release_root)/current/release-manifest.json"

    [ -f "$manifest" ] || return 1
    sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -1
}

running_health() {
    local port="${LERDR_RELAY_PORT:-${HERDR_RELAY_PORT:-8375}}"

    curl -fsS --max-time 2 "http://127.0.0.1:$port/healthz" 2>/dev/null
}

service_state() {
    case "$(uname -s)" in
        Darwin)
            local label
            for label in com.lerdr.service com.herdr-mobile-relay.service; do
                [ -f "$HOME/Library/LaunchAgents/$label.plist" ] && break
                label=""
            done
            [ -n "$label" ] || return 1
            launchd_service_loaded "gui/$(id -u)/$label" &&
                printf 'installed (loaded)\n' || printf 'installed (not loaded)\n'
            ;;
        Linux)
            local unit
            for unit in lerdr.service herdr-mobile-relay.service; do
                [ -f "$HOME/.config/systemd/user/$unit" ] && break
                unit=""
            done
            [ -n "$unit" ] || return 1
            printf 'installed (%s)\n' \
                "$(systemctl --user is-active "$unit" 2>/dev/null || echo unknown)"
            ;;
        *) return 1 ;;
    esac
}

tailscale_summary() {
    local tailscale_bin

    tailscale_bin="${LERDR_TAILSCALE_BIN:-${HERDR_TAILSCALE_BIN:-}}"
    if [ -z "$tailscale_bin" ]; then
        tailscale_bin="$(command -v tailscale 2>/dev/null || true)"
    fi
    if [ -z "$tailscale_bin" ] && [ -x "/Applications/Tailscale.app/Contents/MacOS/Tailscale" ]; then
        tailscale_bin="/Applications/Tailscale.app/Contents/MacOS/Tailscale"
    fi
    if [ -z "$tailscale_bin" ]; then
        printf 'tailscale not installed - start with 1\n'
        return 0
    fi
    if "$tailscale_bin" serve status 2>/dev/null | grep -q .; then
        printf 'tailscale serve configured\n'
    else
        printf 'tailscale installed, serve not configured - start with 1\n'
    fi
}

print_status() {
    local installed running health service

    installed="$(installed_release_version || true)"
    health="$(running_health || true)"
    running="$(json_string_field "$health" release_version)"
    if [ -n "$installed" ]; then
        if [ -z "$running" ]; then
            printf '  Relay:      %s installed, not running\n' "$installed"
        elif [ "$running" = "$installed" ]; then
            printf '  Relay:      %s running\n' "$running"
        else
            printf '  Relay:      %s installed, %s still running - restart pending\n' \
                "$installed" "$running"
        fi
    fi
    service="$(service_state || true)"
    [ -z "$service" ] || printf '  Service:    %s\n' "$service"
    printf '  Phone path: %s\n' "$(tailscale_summary)"
}

render_menu() {
    echo "🐑 Lerdr Setup"
    echo ""
    print_status
    echo ""
    echo "Choose a complete setup action:"
    echo ""
    echo "Connection"
    echo ""
    menu_item 1 "Tailscale Serve"
    echo "     Publish the relay on this machine's tailnet HTTPS name via"
    echo "     tailscale serve (Linux and macOS), then print the private setup QR."
    echo ""
    menu_item 2 "Show Phone Setup QR"
    echo "     Reprint the private setup link and QR for the tailnet endpoint."
    echo ""
    echo "Diagnostics"
    echo ""
    menu_item 3 "Show Full Status"
    echo "     Service, health, and a sanitized support snapshot."
    echo ""
    menu_item q "Exit, change nothing"
    echo ""
}

# Every action runs as a child, so finishing one comes back here with the status
# recomputed instead of ending the pane. Ctrl-C belongs to the action: the menu
# must survive it without swallowing it. A handler, never `trap '' INT` - an
# ignored signal is inherited by children as SIG_IGN, which would leave a
# prompt loop with no way out at all. Actions pause here rather than inside each
# script, which is why pause_before_close stands down under LERDR_SETUP_MENU.
run_action() {
    local action="$1"
    shift

    echo ""
    trap 'printf "\n"' INT
    (
        cd "$SCRIPT_DIR"
        LERDR_SETUP_MENU=1 "$action" "$@"
    ) || true
    load_relay_env "$ENV_FILE"
    trap - INT
    if [ -t 0 ]; then
        echo ""
        read -r -p "Press Enter to return to the menu." _answer || return 0
    fi
}

while true; do
    render_menu
    while true; do
        if ! read -r -p "Choice [1]: " choice; then
            echo ""
            exit 0
        fi
        case "${choice:-1}" in
            1) run_action "$SCRIPT_DIR/plugin-tailscale-setup.sh"; break ;;
            2) run_action "$SCRIPT_DIR/plugin-setup-link.sh"; break ;;
            3) run_action "$SCRIPT_DIR/plugin-status.sh"; break ;;
            q | Q) exit 0 ;;
            *) echo "Enter 1, 2, 3, or q." ;;
        esac
    done
done
