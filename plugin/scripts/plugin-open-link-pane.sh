#!/bin/bash
set -euo pipefail

# open-link pane: render the link-handler URL as a terminal QR so a phone
# camera can open it — the deep-link transport until the protocol grows a
# dedicated open-URL action (docs/09-plugin-distribution.md).

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
export PATH="/opt/homebrew/bin:/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:$HOME/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

CLICKED_URL="${LERDR_CLICKED_URL:-}"

echo "🐑 Open link on phone"
echo ""
if [ -z "$CLICKED_URL" ]; then
    echo "  No link was forwarded to this pane."
    pause_before_close
    exit 0
fi

echo "  $CLICKED_URL"
echo ""
echo "  Scan with your phone's camera to open it:"
echo ""
if ! "$(relay_binary)" qr "$CLICKED_URL"; then
    echo "  (QR did not fit this pane — zoom out or copy the URL above.)"
fi

pause_before_close
