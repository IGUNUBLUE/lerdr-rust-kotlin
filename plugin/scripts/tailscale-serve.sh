#!/bin/bash
set -euo pipefail

# Tailscale Serve transport: publishes the local relay on this machine's
# tailnet HTTPS name and prints the phone setup QR. The tailnet carries the
# traffic end to end — no Cloudflare account, no gateway, no public ingress.
#
#   scripts/tailscale-serve.sh [start]   configure serve, verify HTTPS, print QR
#   scripts/tailscale-serve.sh off       stop serving the relay on the tailnet
#   scripts/tailscale-serve.sh status    show serve config and endpoint health
#   scripts/tailscale-serve.sh link      reprint the setup QR without changes
#
# Linux and macOS: works against a Homebrew tailscaled or the Tailscale.app
# GUI client. The phone side works on any Tailscale client.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

export PATH="/opt/homebrew/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:$HOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

ENV_FILE="$(relay_env_file "$SCRIPT_DIR")"
PORT="${LERDR_RELAY_PORT:-${HERDR_RELAY_PORT:-8375}}"
COMMAND="${1:-start}"

# The CLI ships standalone (Homebrew/package manager) and inside Tailscale.app.
TAILSCALE_BIN="${LERDR_TAILSCALE_BIN:-${HERDR_TAILSCALE_BIN:-}}"
if [ -z "$TAILSCALE_BIN" ] && command -v tailscale >/dev/null 2>&1; then
    TAILSCALE_BIN="$(command -v tailscale)"
fi
if [ -z "$TAILSCALE_BIN" ] && [ -x "/Applications/Tailscale.app/Contents/MacOS/Tailscale" ]; then
    TAILSCALE_BIN="/Applications/Tailscale.app/Contents/MacOS/Tailscale"
fi
if [ -z "$TAILSCALE_BIN" ]; then
    echo "✗ tailscale is not installed."
    echo "  Install it and sign this machine into your tailnet: https://tailscale.com/download"
    if [ "$(uname -s)" = "Darwin" ]; then
        echo "  macOS: brew install tailscale && sudo brew services start tailscaled,"
        echo "  or install Tailscale.app from the App Store / tailscale.com."
    fi
    exit 1
fi

# This node's MagicDNS name (e.g. host.tail1234.ts.net), without the trailing
# dot. Empty when tailscaled is down, logged out, or MagicDNS is off.
tailscale_fqdn() {
    local status_json fqdn

    if ! status_json="$("$TAILSCALE_BIN" status --json 2>/dev/null)"; then
        echo "✗ tailscale status failed — is tailscaled running and this machine logged in?" >&2
        return 1
    fi
    if command -v python3 >/dev/null 2>&1; then
        fqdn="$(printf '%s' "$status_json" |
            python3 -c 'import json,sys; print(json.load(sys.stdin).get("Self",{}).get("DNSName","").rstrip("."))' \
                2>/dev/null)"
    else
        # Self serializes before Peer entries, so the first DNSName is ours.
        fqdn="$(printf '%s' "$status_json" | sed -n 's/.*"DNSName":"\([^"]*\)".*/\1/p' | head -1)"
        fqdn="${fqdn%.}"
    fi
    if [ -z "$fqdn" ]; then
        echo "✗ This node has no tailnet DNS name." >&2
        echo "  Enable MagicDNS in the tailnet admin: https://login.tailscale.com/admin/dns" >&2
        return 1
    fi
    printf '%s\n' "$fqdn"
}

# The serve config already forwards tailnet HTTPS to this relay port.
serve_proxies_relay() {
    "$TAILSCALE_BIN" serve status 2>/dev/null |
        grep -qE "proxy https?://(127\.0\.0\.1|localhost):$PORT([/:[:space:]]|$)"
}

configure_serve() {
    local output

    # "$TAILSCALE_BIN" serve blocks while the node lacks Serve/HTTPS-cert approval,
    # printing a one-time enable URL first; that wait is the intended flow.
    if ! output="$("$TAILSCALE_BIN" serve --bg --yes "$PORT" 2>&1)"; then
        printf '%s\n' "$output" >&2
        if printf '%s' "$output" | grep -q "Serve is not enabled"; then
            echo "" >&2
            echo "  Also required once per tailnet: HTTPS Certificates at" >&2
            echo "  https://login.tailscale.com/admin/dns" >&2
        fi
        return 1
    fi
    printf '%s\n' "$output"
}

# The first HTTPS request triggers tailnet certificate provisioning, which can
# take several seconds; retry rather than report a dead endpoint.
wait_for_https() {
    local fqdn="$1"
    local attempt

    for attempt in $(seq 1 30); do
        if curl -fsS --max-time 5 "https://$fqdn/healthz" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    return 1
}

# setup-link.sh needs the verified release; a source checkout can satisfy the
# same requirement by pointing LERDR_RELAY_BIN at its own build. Surface that
# as a hint instead of letting the generic release error stand alone.
require_relay_binary() {
    if relay_binary >/dev/null 2>&1; then
        return 0
    fi
    local repo_bin="$SCRIPT_DIR/../../relay/target/release/lerdr-relay"
    echo "✗ Verified relay release is unavailable." >&2
    if [ -x "$repo_bin" ]; then
        echo "  This checkout has a built binary; point the launcher at it:"
        echo "  LERDR_RELAY_BIN=relay/target/release/lerdr-relay scripts/tailscale-serve.sh start"
    else
        echo "  Install the plugin release, or build one:"
        echo "  cargo build --release -p lerdr-relay   # from relay/"
    fi
    return 1
}

print_setup_link() {
    local fqdn="$1"

    LERDR_PHONE_APP_URL="https://$fqdn" "$SCRIPT_DIR/setup-link.sh" "$fqdn"
}

case "$COMMAND" in
    start)
        FQDN="$(tailscale_fqdn)"
        if serve_proxies_relay; then
            echo "▸ Tailscale Serve already proxies tailnet HTTPS to 127.0.0.1:$PORT"
        elif "$TAILSCALE_BIN" serve status 2>/dev/null | grep -q .; then
            echo "✗ tailscale serve is already configured for a different target:"
            "$TAILSCALE_BIN" serve status
            echo ""
            echo "  Refusing to replace it. Free the tailnet listener first:"
            echo "  scripts/tailscale-serve.sh off    (or: tailscale serve --https=443 off)"
            exit 1
        else
            echo "▸ Exposing 127.0.0.1:$PORT as https://$FQDN inside the tailnet..."
            configure_serve
        fi

        if ! curl -fsS --max-time 3 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1; then
            echo "▸ The relay is not answering on 127.0.0.1:$PORT yet."
            echo "  Start it first (setup menu or the background service), then rerun"
            echo "  this command for the HTTPS check and a freshly armed QR."
        elif ! wait_for_https "$FQDN"; then
            echo "✗ https://$FQDN did not become healthy within 30s."
            echo "  Inspect with: tailscale serve status"
            exit 1
        else
            echo "▸ Tailnet endpoint healthy: https://$FQDN"
        fi
        echo ""
        require_relay_binary
        print_setup_link "$FQDN"
        ;;
    link)
        FQDN="$(tailscale_fqdn)"
        if ! serve_proxies_relay; then
            echo "✗ Tailscale Serve is not forwarding to the relay; run: scripts/tailscale-serve.sh start"
            exit 1
        fi
        require_relay_binary
        print_setup_link "$FQDN"
        ;;
    off)
        if ! "$TAILSCALE_BIN" serve status 2>/dev/null | grep -q .; then
            echo "Tailscale Serve has no configuration; nothing to stop."
            exit 0
        fi
        "$TAILSCALE_BIN" serve status
        echo ""
        "$TAILSCALE_BIN" serve --https=443 off
        echo "Stopped serving on the tailnet. The relay itself is untouched."
        ;;
    status)
        FQDN="$(tailscale_fqdn)"
        echo "Tailnet name: $FQDN"
        echo ""
        if ! "$TAILSCALE_BIN" serve status 2>/dev/null | grep -q .; then
            echo "Tailscale Serve: no configuration (run scripts/tailscale-serve.sh start)"
        else
            echo "Tailscale Serve:"
            "$TAILSCALE_BIN" serve status
            echo ""
            if curl -fsS --max-time 5 "https://$FQDN/healthz" >/dev/null 2>&1; then
                echo "Endpoint: https://$FQDN healthy"
            else
                echo "Endpoint: https://$FQDN not answering"
            fi
        fi
        ;;
    *)
        echo "Usage: $0 [start|off|status|link]"
        exit 2
        ;;
esac
