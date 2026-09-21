#!/bin/bash
set -euo pipefail

LABELS=("lerdr.service" "herdr-mobile-relay.service" "herdr-remote.service")

for label in "${LABELS[@]}"; do
    systemctl --user disable --now "$label" >/dev/null 2>&1 || true
    rm -f "$HOME/.config/systemd/user/$label"
done
systemctl --user daemon-reload

echo "Stopped and removed Lerdr services"
