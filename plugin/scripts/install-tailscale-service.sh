#!/bin/bash
set -euo pipefail

# Installs the relay as a user service for the Tailscale transport: systemd on
# Linux, a LaunchAgent on macOS. Unlike the Cloudflare variant there is no
# tunnel process to supervise: tailscaled owns the tailnet listener and
# persists the serve configuration, so the unit runs only the relay.

LABEL="lerdr.service"
LAUNCHD_LABEL="com.lerdr.service"
LEGACY_LABELS=("herdr-mobile-relay.service" "herdr-remote.service")
LEGACY_LAUNCHD_LABELS=("com.herdr-mobile-relay.service" "com.herdr-remote.service")
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT_FILE="$UNIT_DIR/$LABEL"
PLIST="$HOME/Library/LaunchAgents/$LAUNCHD_LABEL.plist"
LOG_DIR="$HOME/Library/Logs/lerdr"

export PATH="$HOME/.local/bin:/usr/local/bin:/opt/homebrew/bin:/home/linuxbrew/.linuxbrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

ENV_FILE="$(relay_env_file "$SCRIPT_DIR")"
PORT="${LERDR_RELAY_PORT:-${HERDR_RELAY_PORT:-8375}}"

finish_install() {
    echo "Waiting for relay health on 127.0.0.1:$PORT..."
    if ! HEALTH="$(wait_for_relay_health "$PORT")"; then
        echo "Relay service was installed, but it did not become healthy."
        echo "Inspect it with:"
        if [ "$(uname -s)" = "Darwin" ]; then
            echo "  launchctl print gui/$(id -u)/$LAUNCHD_LABEL"
            echo "  tail -n 80 '$LOG_DIR/service.log' '$LOG_DIR/service.err'"
        else
            echo "  systemctl --user status $LABEL --no-pager"
            echo "  journalctl --user -u $LABEL -n 80 --no-pager"
        fi
        exit 1
    fi
    echo "Relay health: $HEALTH"
    echo ""
    echo "Publish it on the tailnet and print the phone QR with:"
    echo "  scripts/tailscale-serve.sh start"
}

install_systemd() {
    if ! command -v systemctl >/dev/null 2>&1; then
        echo "systemctl not found"
        exit 1
    fi

    RELAY_BIN="$(relay_binary)"
    ensure_relay_env "$ENV_FILE"

    RELEASE_ROOT="$(relay_release_root)"
    WORK_DIR="$RELEASE_ROOT/current"
    if [ ! -d "$WORK_DIR" ]; then
        WORK_DIR="$SCRIPT_DIR/.."
    fi
    # systemd rejects non-normalized paths such as ".../scripts/..".
    WORK_DIR="$(cd "$WORK_DIR" && pwd -P)"

    # The service PATH is static, so mirror the foreground wrapper's per-agent
    # bin discovery at install time; new agents installed later can be added
    # through HERDR_BIN or an explicit Environment edit in the unit.
    SERVICE_PATH="/opt/homebrew/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:$HOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    for agent_bin in "$HOME"/.[!.]*/bin; do
        [ -d "$agent_bin" ] && SERVICE_PATH="$SERVICE_PATH:$agent_bin"
    done

    mkdir -p "$UNIT_DIR"

    cat > "$UNIT_FILE" <<EOF
[Unit]
Description=Lerdr (Tailscale transport)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=$WORK_DIR
Environment=LERDR_RELAY_ENV=$ENV_FILE
# The binary uses LERDR_RELAY_ENV only to locate its runtime directory; the
# relay key itself must come from the env file, like the foreground wrappers
# source it before exec. EnvironmentFile is the systemd-native equivalent.
EnvironmentFile=$ENV_FILE
Environment=LERDR_RELAY_HOST=127.0.0.1
Environment=LERDR_RELAY_PORT=$PORT
Environment=HERDR_RELAY_HOST=127.0.0.1
Environment=HERDR_RELAY_PORT=$PORT
Environment=PATH=$SERVICE_PATH
ExecStart=$RELAY_BIN serve
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
EOF

    systemctl --user daemon-reload
    for legacy_label in "${LEGACY_LABELS[@]}"; do
        systemctl --user disable --now "$legacy_label" >/dev/null 2>&1 || true
        rm -f "$UNIT_DIR/$legacy_label"
    done
    systemctl --user daemon-reload
    systemctl --user enable "$LABEL"
    systemctl --user restart "$LABEL"

    echo "Installed and started $LABEL"
    echo "Unit: $UNIT_FILE"
    echo "Env:  $ENV_FILE"
    echo "Logs: journalctl --user -u $LABEL -f"
    finish_install
}

install_launchd() {
    require_user_service_context
    ensure_relay_env "$ENV_FILE"
    chmod +x "$SCRIPT_DIR/tailscale-service.sh"
    mkdir -p "$HOME/Library/LaunchAgents" "$LOG_DIR"

    RELEASE_ROOT="$(relay_release_root)"
    # Rust-era bundles ship their runtime scripts under scripts/; Go-era
    # bundles used relay/. Resolve whichever the active release carries.
    SERVICE_WRAPPER="$RELEASE_ROOT/current/scripts/tailscale-service.sh"
    if [ ! -x "$SERVICE_WRAPPER" ]; then
        SERVICE_WRAPPER="$RELEASE_ROOT/current/relay/tailscale-service.sh"
    fi
    WORK_DIR="$RELEASE_ROOT/current"
    if [ ! -x "$SERVICE_WRAPPER" ]; then
        SERVICE_WRAPPER="$SCRIPT_DIR/tailscale-service.sh"
    fi
    if [ ! -d "$WORK_DIR" ]; then
        WORK_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
    fi

    cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LAUNCHD_LABEL</string>
    <key>ProgramArguments</key>
    <array>
        <string>$SERVICE_WRAPPER</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
        <key>NetworkState</key>
        <true/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>WorkingDirectory</key>
    <string>$WORK_DIR</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>LERDR_RELAY_ENV</key>
        <string>$ENV_FILE</string>
    </dict>
    <key>StandardOutPath</key>
    <string>$LOG_DIR/service.log</string>
    <key>StandardErrorPath</key>
    <string>$LOG_DIR/service.err</string>
</dict>
</plist>
EOF

    for legacy_label in "${LEGACY_LAUNCHD_LABELS[@]}"; do
        legacy_plist="$HOME/Library/LaunchAgents/$legacy_label.plist"
        launchctl bootout "gui/$UID" "$legacy_plist" >/dev/null 2>&1 || true
        rm -f "$legacy_plist"
    done
    reload_launchd_service_definition "$PLIST" "$LAUNCHD_LABEL"

    echo "Installed and started $LAUNCHD_LABEL"
    echo "Plist: $PLIST"
    echo "Env:   $ENV_FILE"
    echo "Logs:  $LOG_DIR/service.log and $LOG_DIR/service.err"
    finish_install
}

case "$(uname -s)" in
    Linux)  install_systemd ;;
    Darwin) install_launchd ;;
    *)
        echo "The Tailscale transport supports Linux and macOS."
        exit 1
        ;;
esac
