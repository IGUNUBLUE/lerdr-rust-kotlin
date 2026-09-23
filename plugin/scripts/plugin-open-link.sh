#!/bin/bash
set -euo pipefail

# Action entrypoint for the [[link_handlers]] manifest section. herdr invokes
# it for a modified-click on a matched URL with HERDR_PLUGIN_CLICKED_URL and
# HERDR_PLUGIN_LINK_HANDLER_ID set (plus the full invocation context in
# HERDR_PLUGIN_CONTEXT_JSON). The clicked URL is forwarded into the open-link
# pane's environment — a fresh plugin invocation would otherwise not see it.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

PLUGIN_ID="${LERDR_PLUGIN_ID:-${HERDR_PLUGIN_ID:-lerdr.events}}"
HERDR_COMMAND="${HERDR_BIN_PATH:-herdr}"

CLICKED_URL="${HERDR_PLUGIN_CLICKED_URL:-}"
if [ -z "$CLICKED_URL" ] && [ -n "${HERDR_PLUGIN_CONTEXT_JSON:-}" ]; then
    CLICKED_URL="$(json_string_field "$HERDR_PLUGIN_CONTEXT_JSON" clicked_url)"
fi

if [ -z "$CLICKED_URL" ]; then
    echo "No clicked URL in the plugin invocation context." >&2
    exit 2
fi

args=(
    plugin pane open
    --plugin "$PLUGIN_ID"
    --entrypoint open-link
    --placement overlay
    --env "PATH=$PATH"
    --env "LERDR_CLICKED_URL=$CLICKED_URL"
    --env "LERDR_LINK_HANDLER_ID=${HERDR_PLUGIN_LINK_HANDLER_ID:-}"
    --focus
)
# Anchor the overlay next to the pane the link was clicked in, like
# open-plugin-pane.sh does for the other entries.
if [ -n "${HERDR_PANE_ID:-}" ]; then
    args+=(--target-pane "$HERDR_PANE_ID")
fi

exec "$HERDR_COMMAND" "${args[@]}"
