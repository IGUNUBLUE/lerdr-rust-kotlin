#!/bin/bash
# Marketplace build hook: install the exact pre-built release named by the
# plugin manifest. End-user hosts never compile Rust or install a toolchain.
set -eu

SCRIPT_DIR=${0%/*}
if [ "$SCRIPT_DIR" = "$0" ]; then
    SCRIPT_DIR=.
fi
SCRIPT_DIR=$(CDPATH='' cd "$SCRIPT_DIR" && pwd)
# PLUGIN_DIR is the directory holding herdr-plugin.toml — the plugin root for
# both managed installs and `herdr plugin link` checkouts.
PLUGIN_DIR=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd)
# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

require_user_service_context

VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$PLUGIN_DIR/herdr-plugin.toml")
[ -n "$VERSION" ] || {
    echo "lerdr: herdr-plugin.toml has no exact version" >&2
    exit 1
}

INSTALL_ROOT="$(relay_release_root)"
BIN_DIR=${LERDR_RELAY_BIN_DIR:-${HERDR_RELAY_BIN_DIR:-"$HOME/.local/bin"}}
INSTALLER=${LERDR_PLUGIN_INSTALLER:-${HERDR_PLUGIN_INSTALLER:-"$PLUGIN_DIR/install.sh"}}
RELEASE_REPOSITORY="$(relay_env RELEASE_REPOSITORY)"
if [ -z "$RELEASE_REPOSITORY" ]; then
    RELEASE_REPOSITORY=$(release_repository "$PLUGIN_DIR" || true)
fi
if [ -n "$RELEASE_REPOSITORY" ]; then
    # Export both spellings so an install.sh from either side of the rename
    # picks the repository up.
    export LERDR_RELEASE_REPOSITORY="$RELEASE_REPOSITORY"
    export HERDR_RELEASE_REPOSITORY="$RELEASE_REPOSITORY"
fi
TARGET_CONFIG_ROOT=${HERDR_PLUGIN_CONFIG_DIR:-}
if [ -z "$TARGET_CONFIG_ROOT" ] && command -v herdr >/dev/null 2>&1; then
    TARGET_CONFIG_ROOT="$(herdr plugin config-dir lerdr.events 2>/dev/null || true)"
fi
DEFAULT_CONFIG_ROOT="${XDG_CONFIG_HOME:-$HOME/.config}/lerdr"
TARGET_CONFIG_ROOT=${TARGET_CONFIG_ROOT:-$DEFAULT_CONFIG_ROOT}
case "$TARGET_CONFIG_ROOT" in
    /*) ;;
    *)
        echo "lerdr: plugin config directory must be absolute: $TARGET_CONFIG_ROOT" >&2
        exit 1
        ;;
esac
# A standalone pre-rename install kept its whole config under the old default.
if [ "$TARGET_CONFIG_ROOT" = "$DEFAULT_CONFIG_ROOT" ]; then
    migrate_legacy_dir \
        "${XDG_CONFIG_HOME:-$HOME/.config}/herdr-mobile-relay" \
        "$TARGET_CONFIG_ROOT"
fi
TARGET_ENV="$TARGET_CONFIG_ROOT/relay.env"
SOURCE_ENV="$(installed_service_env_file)"
RESOLVED_RELAY_ENV="$(relay_env RELAY_ENV)"
if [ -z "$SOURCE_ENV" ] && [ -n "$RESOLVED_RELAY_ENV" ] && [ -f "$RESOLVED_RELAY_ENV" ]; then
    if [ "$(canonical_file_path "$RESOLVED_RELAY_ENV")" = "$(canonical_file_path "$TARGET_ENV")" ]; then
        SOURCE_ENV="$RESOLVED_RELAY_ENV"
    fi
fi
ENV_FILE="$TARGET_ENV"
HERDR_PLUGIN_CONFIG_DIR="$TARGET_CONFIG_ROOT"
LERDR_RELAY_ENV="$TARGET_ENV"
HERDR_RELAY_ENV="$TARGET_ENV"
export INSTALL_ROOT BIN_DIR HERDR_PLUGIN_CONFIG_DIR LERDR_RELAY_ENV HERDR_RELAY_ENV

PLATFORM=$(uname -s)
SERVICE_FILE=
SERVICE_BACKUP=
service_was_active=false
service_should_run=false
recover_broken_service=false
service_cutover_started=false
# The unit moved to its lerdr name in this release. SERVICE_FILE points at
# whichever definition exists; NEW_SERVICE_FILE is where it must end up.
case "$PLATFORM" in
    Linux)
        NEW_SERVICE_FILE="$HOME/.config/systemd/user/lerdr.service"
        LEGACY_SERVICE_FILE="$HOME/.config/systemd/user/herdr-mobile-relay.service"
        SERVICE_FILE="$NEW_SERVICE_FILE"
        if [ ! -f "$SERVICE_FILE" ]; then
            SERVICE_FILE="$LEGACY_SERVICE_FILE"
        fi
        for unit_name in lerdr.service herdr-mobile-relay.service; do
            case "$(systemctl --user is-active "$unit_name" 2>/dev/null || true)" in
                active|activating|reloading) service_was_active=true ;;
            esac
        done
        ;;
    Darwin)
        NEW_SERVICE_FILE="$HOME/Library/LaunchAgents/com.lerdr.service.plist"
        LEGACY_SERVICE_FILE="$HOME/Library/LaunchAgents/com.herdr-mobile-relay.service.plist"
        SERVICE_FILE="$NEW_SERVICE_FILE"
        if [ ! -f "$SERVICE_FILE" ]; then
            SERVICE_FILE="$LEGACY_SERVICE_FILE"
        fi
        for label in com.lerdr.service com.herdr-mobile-relay.service; do
            if launchd_service_loaded "gui/$(id -u)/$label"; then
                service_was_active=true
            fi
        done
        ;;
esac
service_should_run=$service_was_active
service_file_renamed=false
if [ -n "$SERVICE_FILE" ] && [ -f "$SERVICE_FILE" ]; then
    SERVICE_BACKUP=$(mktemp "${TMPDIR:-/tmp}/lerdr-service.XXXXXX")
    cp "$SERVICE_FILE" "$SERVICE_BACKUP"
fi

validate_migration_source() {
    local source_env="$1"
    local source_canonical
    local service_canonical
    local service_env_path

    [ -f "$source_env" ] && [ ! -L "$source_env" ] || {
        echo "lerdr: installed service environment is not a regular file: $source_env" >&2
        return 1
    }
    grep -qE '^(LERDR|HERDR)_RELAY_TOKEN=' "$source_env" &&
        [ -n "$(env_file_setting "$source_env" RELAY_TOKEN)" ] ||
        {
            echo "lerdr: installed service environment has no relay token" >&2
            return 1
        }
    source_canonical="$(canonical_file_path "$source_env")"
    service_canonical="$(canonical_file_path "$(installed_service_env_file)")"
    [ "$source_canonical" = "$service_canonical" ] || {
        echo "lerdr: installed service environment changed during migration" >&2
        return 1
    }

    case "$PLATFORM" in
        Linux)
            service_env_path="$(sed -nE 's/^Environment=(LERDR|HERDR)_RELAY_ENV=//p' "$SERVICE_FILE" | tail -1)"
            [ "$service_env_path" = "$source_env" ] &&
                grep -E '^ExecStart=.*(lerdr|herdr-(mobile-relay|remote)|tailscale)-service\.sh([[:space:]]|$)' \
                    "$SERVICE_FILE" >/dev/null || {
                    echo "lerdr: refusing to migrate an unrecognized systemd service" >&2
                    return 1
                }
            ;;
        Darwin)
            grep -E '<string>com\.(lerdr|herdr-mobile-relay)\.service</string>' "$SERVICE_FILE" >/dev/null &&
                grep -E '<string>.*(lerdr|herdr-(mobile-relay|remote)|tailscale)-service\.sh</string>' \
                    "$SERVICE_FILE" >/dev/null || {
                    echo "lerdr: refusing to migrate an unrecognized launchd service" >&2
                    return 1
                }
            ;;
    esac
}

recognized_service_definition() {
    case "$PLATFORM" in
        Linux)
            grep -E '^Environment=(LERDR|HERDR)_RELAY_ENV=/.+' "$SERVICE_FILE" >/dev/null &&
                grep -E '^ExecStart=.*(lerdr|herdr-(mobile-relay|remote)|tailscale)-service\.sh([[:space:]]|$)' \
                    "$SERVICE_FILE" >/dev/null
            ;;
        Darwin)
            grep -E '<string>com\.(lerdr|herdr-mobile-relay)\.service</string>' "$SERVICE_FILE" >/dev/null &&
                grep -E '<key>(LERDR|HERDR)_RELAY_ENV</key>' "$SERVICE_FILE" >/dev/null &&
                grep -E '<string>.*(lerdr|herdr-(mobile-relay|remote)|tailscale)-service\.sh</string>' \
                    "$SERVICE_FILE" >/dev/null
            ;;
        *) return 1 ;;
    esac
}

validate_recovery_config() {
    [ -f "$TARGET_ENV" ] && [ ! -L "$TARGET_ENV" ] || {
        echo "lerdr: persistent relay environment is unavailable: $TARGET_ENV" >&2
        return 1
    }
    grep -qE '^(LERDR|HERDR)_RELAY_TOKEN=' "$TARGET_ENV" &&
        [ -n "$(env_file_setting "$TARGET_ENV" RELAY_TOKEN)" ] || {
        echo "lerdr: persistent relay environment has no relay token" >&2
        return 1
    }
}

# Rewrites ExecStart/WorkingDirectory/the relay-env key in place. env_key is
# HERDR_RELAY_ENV only when the target wrapper is a pre-rename bundle, which
# cannot read the LERDR_ spelling.
rewrite_service_release_paths() {
    local service_file="$1"
    local service_wrapper="$2"
    local work_dir="$3"
    local env_file="$4"
    local env_key="${5:-LERDR_RELAY_ENV}"
    local label="${6:-com.lerdr.service}"

    [ -f "$service_file" ] && [ -x "$service_wrapper" ] || return 1
    case "$PLATFORM" in
        Linux)
            local temp
            temp="$(mktemp "${service_file}.XXXXXX")" || return 1
            if ! sed -E \
                -e "s|^ExecStart=.*|ExecStart=$service_wrapper|" \
                -e "s|^WorkingDirectory=.*|WorkingDirectory=$work_dir|" \
                -e "s#^Environment=(LERDR|HERDR)_RELAY_ENV=.*#Environment=$env_key=$env_file#" \
                "$service_file" > "$temp"; then
                rm -f "$temp"
                return 1
            fi
            chmod --reference="$service_file" "$temp" 2>/dev/null || chmod 600 "$temp"
            if ! mv -f "$temp" "$service_file"; then
                rm -f "$temp"
                return 1
            fi
            grep -Fx "ExecStart=$service_wrapper" "$service_file" >/dev/null &&
                grep -Fx "WorkingDirectory=$work_dir" "$service_file" >/dev/null &&
                grep -Fx "Environment=$env_key=$env_file" "$service_file" >/dev/null
            ;;
        Darwin)
            update_launchd_release_paths \
                "$service_file" "$service_wrapper" "$work_dir" "$env_file" "$label" "$env_key"
            ;;
        *) return 1 ;;
    esac
}

CONFIG_BACKUP=
target_config_existed=false
if [ -e "$TARGET_CONFIG_ROOT" ]; then
    [ -d "$TARGET_CONFIG_ROOT" ] && [ ! -L "$TARGET_CONFIG_ROOT" ] || {
        echo "lerdr: persistent plugin config is not a regular directory: $TARGET_CONFIG_ROOT" >&2
        exit 1
    }
    [ -z "$(find "$TARGET_CONFIG_ROOT" -type l -print -quit)" ] || {
        echo "lerdr: persistent plugin config contains a symlink: $TARGET_CONFIG_ROOT" >&2
        exit 1
    }
    target_config_existed=true
fi
CONFIG_BACKUP=$(mktemp -d "${TMPDIR:-/tmp}/lerdr-plugin-config.XXXXXX")
if [ "$target_config_existed" = true ]; then
    cp -pR "$TARGET_CONFIG_ROOT/." "$CONFIG_BACKUP/"
fi

restore_target_config() {
    if [ -L "$TARGET_CONFIG_ROOT" ]; then
        echo "lerdr: refusing to restore through a symlinked config root" >&2
        return 1
    fi
    rm -rf "$TARGET_CONFIG_ROOT"
    if [ "$target_config_existed" = true ]; then
        mkdir -p "$TARGET_CONFIG_ROOT"
        cp -pR "$CONFIG_BACKUP/." "$TARGET_CONFIG_ROOT/"
    fi
}

copy_migration_entry() {
    local source_path="$1"
    local target_name="$2"
    local target_path="$TARGET_CONFIG_ROOT/$target_name"

    [ -e "$source_path" ] || return 0
    [ ! -L "$source_path" ] || {
        echo "lerdr: refusing symlinked migration source: $source_path" >&2
        return 1
    }
    rm -rf "$target_path"
    cp -pR "$source_path" "$target_path"
}

rewrite_path_prefix() {
    local filename="$1"
    local old_prefix="$2"
    local new_prefix="$3"
    local escaped_old
    local escaped_new
    local temp

    [ -f "$filename" ] || return 0
    escaped_old="$(printf '%s' "$old_prefix" | sed 's/[][\\.^$*+?{}|()]/\\&/g')"
    escaped_new="$(printf '%s' "$new_prefix" | sed 's/[\\&|]/\\&/g')"
    temp="$(mktemp "$(dirname "$filename")/.migration.XXXXXX")"
    sed "s|$escaped_old|$escaped_new|g" "$filename" > "$temp"
    chmod --reference="$filename" "$temp" 2>/dev/null || chmod 600 "$temp"
    mv -f "$temp" "$filename"
}

migrate_source_config() {
    local source_env="$1"
    local source_root

    source_root="$(dirname "$source_env")"
    if [ "$(canonical_file_path "$source_env")" = "$(canonical_file_path "$TARGET_ENV")" ]; then
        return
    fi
    echo "lerdr: migrating service state into persistent plugin config..." >&2
    mkdir -p "$TARGET_CONFIG_ROOT"
    chmod 700 "$TARGET_CONFIG_ROOT"
    copy_migration_entry "$source_env" relay.env
    copy_migration_entry "$source_root/device-auth" device-auth
    copy_migration_entry "$source_root/push" push
    copy_migration_entry "$source_root/phone-app-origin" phone-app-origin
    copy_migration_entry "$source_root/phone-app-origin-configured" phone-app-origin-configured
    copy_migration_entry "$source_root/update-state.json" update-state.json
    chmod 600 "$TARGET_ENV"
}

if [ -n "$SERVICE_BACKUP" ]; then
    source_env_missing=false
    if [ -z "$SOURCE_ENV" ] || [ ! -e "$SOURCE_ENV" ]; then
        source_env_missing=true
    fi
    if [ "$source_env_missing" = true ] && [ -n "$SOURCE_ENV" ] && [ -L "$SOURCE_ENV" ]; then
        source_env_missing=false
    fi

    if [ "$source_env_missing" = true ]; then
        if ! recognized_service_definition; then
            echo "lerdr: refusing to recover an unrecognized service definition" >&2
            rm -rf "$CONFIG_BACKUP"
            rm -f "$SERVICE_BACKUP"
            exit 1
        fi
        if ! validate_recovery_config; then
            rm -rf "$CONFIG_BACKUP"
            rm -f "$SERVICE_BACKUP"
            exit 1
        fi
        echo "lerdr: recovering broken service paths from persistent plugin config..." >&2
        SOURCE_ENV="$TARGET_ENV"
        recover_broken_service=true
        service_should_run=true
    elif ! validate_migration_source "$SOURCE_ENV"; then
        rm -rf "$CONFIG_BACKUP"
        rm -f "$SERVICE_BACKUP"
        exit 1
    fi
    if [ "${LERDR_NO_AUTO_SETUP:-${HERDR_MOBILE_RELAY_NO_AUTO_SETUP:-}}" = 1 ]; then
        service_should_run=true
    fi
fi

PREVIOUS_RELEASE=
PREVIOUS_VERSION=
PREVIOUS_REVISION=
PREVIOUS_WEB_HASH=
current_was_present=false
if [ -e "$INSTALL_ROOT/current" ] || [ -L "$INSTALL_ROOT/current" ]; then
    current_was_present=true
fi
if [ -L "$INSTALL_ROOT/current" ]; then
    previous_link=$(readlink "$INSTALL_ROOT/current")
    case "$previous_link" in
        /*) previous_candidate=$previous_link ;;
        *) previous_candidate="$INSTALL_ROOT/$previous_link" ;;
    esac
    if [ -d "$previous_candidate" ]; then
        PREVIOUS_RELEASE=$(CDPATH='' cd "$previous_candidate" && pwd -P)
        previous_manifest="$PREVIOUS_RELEASE/release-manifest.json"
        if [ -f "$previous_manifest" ]; then
            PREVIOUS_VERSION=$(sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$previous_manifest" | head -1)
            PREVIOUS_REVISION=$(sed -n 's/^[[:space:]]*"revision":[[:space:]]*"\([^"]*\)".*/\1/p' "$previous_manifest" | head -1)
            PREVIOUS_WEB_HASH=$(sed -n 's/^[[:space:]]*"web_hash":[[:space:]]*"\([^"]*\)".*/\1/p' "$previous_manifest" | head -1)
        fi
    fi
fi

rollback_armed=false
rollback_plugin_migration() {
    rollback_armed=false
    echo "lerdr: replacement failed; restoring previous service..." >&2

    if [ -n "$PREVIOUS_RELEASE" ] && [ -d "$PREVIOUS_RELEASE" ]; then
        # current may still point at either side of the binary rename; call
        # whichever executable name the bundle it resolves to carries.
        activate_bin=
        for candidate in lerdr-relay lerdr herdr-mobile-relay; do
            if [ -x "$INSTALL_ROOT/current/$candidate" ]; then
                activate_bin="$INSTALL_ROOT/current/$candidate"
                break
            fi
        done
        [ -n "$activate_bin" ] || return 1
        "$activate_bin" \
            activate-release "$INSTALL_ROOT" "$PREVIOUS_RELEASE" || return 1
    elif [ "$current_was_present" = false ]; then
        rm -f "$INSTALL_ROOT/current"
    fi
    if [ -n "$SERVICE_BACKUP" ] && [ -n "$SERVICE_FILE" ]; then
        restore_temp="${SERVICE_FILE}.rollback.$$"
        cp "$SERVICE_BACKUP" "$restore_temp" || return 1
        mv -f "$restore_temp" "$SERVICE_FILE" || return 1
        if [ "$service_file_renamed" = true ]; then
            rm -f "$NEW_SERVICE_FILE"
        fi
    fi
    restore_target_config || return 1

    if [ "$recover_broken_service" = true ] &&
       [ "$service_cutover_started" = true ]; then
        # A rolled-back current is an earlier bundle: it may carry the
        # Rust-era scripts/ layout, the Go-era relay/ layout, or the
        # pre-rename wrapper names, and only understand HERDR_RELAY_ENV.
        rollback_wrapper=
        for candidate in \
            "$INSTALL_ROOT/current/scripts/tailscale-service.sh" \
            "$INSTALL_ROOT/current/relay/tailscale-service.sh" \
            "$INSTALL_ROOT/current/relay/lerdr-service.sh"; do
            if [ -x "$candidate" ]; then
                rollback_wrapper="$candidate"
                break
            fi
        done
        rollback_env_key=LERDR_RELAY_ENV
        rollback_label=com.lerdr.service
        if [ -z "$rollback_wrapper" ]; then
            rollback_wrapper="$INSTALL_ROOT/current/relay/herdr-mobile-relay-service.sh"
            rollback_env_key=HERDR_RELAY_ENV
            rollback_label="$(basename "$SERVICE_FILE" .plist)"
        fi
        [ -x "$rollback_wrapper" ] || return 1
        rewrite_service_release_paths \
            "$SERVICE_FILE" "$rollback_wrapper" "$INSTALL_ROOT/current" "$TARGET_ENV" \
            "$rollback_env_key" "$rollback_label" ||
            return 1
    fi

    if [ "$service_cutover_started" != true ]; then
        echo "lerdr: previous running service was left untouched." >&2
        return 0
    fi
    if [ "$service_should_run" != true ]; then
        echo "lerdr: previous inactive service definition restored." >&2
        return 0
    fi
    rollback_unit="$(basename "$SERVICE_FILE" .plist)"
    case "$PLATFORM" in
        Linux)
            systemctl --user daemon-reload || return 1
            systemctl --user restart "$rollback_unit" || return 1
            ;;
        Darwin)
            reload_launchd_service_definition "$SERVICE_FILE" "$rollback_unit" ||
                return 1
            ;;
    esac

    rollback_env="${SOURCE_ENV:-$ENV_FILE}"
    rollback_port="$(env_file_setting "$rollback_env" RELAY_PORT)"
    rollback_port="${rollback_port:-8375}"
    if [ -n "$PREVIOUS_VERSION" ] && [ -n "$PREVIOUS_REVISION" ] && [ -n "$PREVIOUS_WEB_HASH" ]; then
        wait_for_relay_release_health \
            "$rollback_port" 30 1 \
            "$PREVIOUS_VERSION" "$PREVIOUS_REVISION" "$PREVIOUS_WEB_HASH" \
            >/dev/null || return 1
    else
        wait_for_relay_health "$rollback_port" 30 1 >/dev/null || return 1
    fi
    case "$PLATFORM" in
        Linux) systemctl --user is-active --quiet "$rollback_unit" || return 1 ;;
        Darwin)
            launchd_service_loaded \
                "gui/$(id -u)/$rollback_unit" || return 1
            ;;
    esac
    echo "lerdr: previous service recovered successfully." >&2
}

cleanup_plugin_build() {
    status=$?
    trap - EXIT
    if [ "$status" -ne 0 ] && [ "$rollback_armed" = true ]; then
        if ! rollback_plugin_migration; then
            echo "lerdr: ERROR: automatic rollback also failed" >&2
        fi
    fi
    if [ -n "$SERVICE_BACKUP" ]; then
        rm -f "$SERVICE_BACKUP"
    fi
    rm -rf "$CONFIG_BACKUP"
    exit "$status"
}
trap cleanup_plugin_build EXIT

# A private plugin may clone through SSH while its release API still requires an
# HTTPS token. Reuse an existing gh login when no explicit or plugin-configured
# token exists. An SSH key cannot be converted into an API credential.
gh_release_token() {
    command -v gh >/dev/null 2>&1 || return 1
    gh auth token --hostname github.com 2>/dev/null
}

release_api_available_without_token() {
    command -v curl >/dev/null 2>&1 || return 0
    curl --fail --silent --show-error --location \
        --connect-timeout 5 --max-time 10 \
        -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/$RELEASE_REPOSITORY" >/dev/null 2>&1
}

explain_missing_release_auth() {
    echo "lerdr: cannot access release repository $RELEASE_REPOSITORY through GitHub's HTTPS API." >&2
    echo "lerdr: SSH access cloned the plugin source, but SSH keys do not authorize private release downloads." >&2
    echo "" >&2
    if ! command -v gh >/dev/null 2>&1; then
        case "$(uname -s)" in
            Darwin)
                if command -v brew >/dev/null 2>&1; then
                    echo "Install GitHub CLI:" >&2
                    echo "  brew install gh" >&2
                else
                    echo "Install GitHub CLI from https://cli.github.com/" >&2
                fi
                ;;
            *) echo "Install GitHub CLI from https://cli.github.com/" >&2 ;;
        esac
    fi
    echo "Authorize release access while keeping Git over SSH:" >&2
    echo "  gh auth login --hostname github.com --git-protocol ssh" >&2
    echo "Then rerun the same 'herdr plugin install' command." >&2
    echo "Alternatively, set GH_TOKEN to a token with Contents read access." >&2
}



INSTALL_TOKEN=${GH_TOKEN:-${GITHUB_TOKEN:-}}
if [ -z "$INSTALL_TOKEN" ]; then
    for TOKEN_ENV in "$TARGET_ENV" "${SOURCE_ENV:-}"; do
        [ -n "$TOKEN_ENV" ] && [ -f "$TOKEN_ENV" ] || continue
        configured_token_file="$(env_file_setting "$TOKEN_ENV" GITHUB_TOKEN_FILE)"
        expected_token_file="$(dirname "$TOKEN_ENV")/github-token"
        if [ "$configured_token_file" = "$expected_token_file" ] &&
           [ -f "$configured_token_file" ] &&
           [ ! -L "$configured_token_file" ]; then
            case "$(ls -ld "$configured_token_file" | awk '{print $1}')" in
                -rw-------*) ;;
                *) configured_token_file= ;;
            esac
        else
            configured_token_file=
        fi
        if [ -n "$configured_token_file" ]; then
            IFS= read -r INSTALL_TOKEN < "$configured_token_file" || true
            [ -z "$INSTALL_TOKEN" ] || break
        fi
    done
fi
# The canonical repository answered to 0cv/herdr-mobile-relay before the rename;
# either spelling identifies this project's own releases. A private fork is the
# only case that needs an authenticated API check.
case "$RELEASE_REPOSITORY" in
    IGUNUBLUE/lerdr|0cv/herdr-mobile-relay|"") noncanonical_repository=false ;;
    *) noncanonical_repository=true ;;
esac
if [ -z "$INSTALL_TOKEN" ] && [ "$noncanonical_repository" = true ]; then
    INSTALL_TOKEN="$(gh_release_token || true)"
fi
if [ -z "$INSTALL_TOKEN" ] &&
   [ "$noncanonical_repository" = true ] &&
   ! release_api_available_without_token; then
    explain_missing_release_auth
    exit 1
fi


rollback_armed=true
migrate_source_config "${SOURCE_ENV:-$TARGET_ENV}"

echo "lerdr: installing verified release $VERSION..." >&2
if [ -n "$INSTALL_TOKEN" ]; then
    GH_TOKEN="$INSTALL_TOKEN" sh "$INSTALLER" "$VERSION"
else
    sh "$INSTALLER" "$VERSION"
fi
# The freshly activated release always carries the Rust binary; current may
# still resolve to a Go-era bundle when the install step left it untouched.
CURRENT_BIN=
for candidate in lerdr-relay lerdr herdr-mobile-relay; do
    if [ -x "$INSTALL_ROOT/current/$candidate" ]; then
        CURRENT_BIN="$INSTALL_ROOT/current/$candidate"
        break
    fi
done
[ -n "$CURRENT_BIN" ] || {
    echo "lerdr: installed release has no executable relay binary" >&2
    exit 1
}
"$CURRENT_BIN" verify-release "$INSTALL_ROOT/current" >/dev/null
MANIFEST="$INSTALL_ROOT/current/release-manifest.json"
REVISION=$(sed -n 's/^[[:space:]]*"revision":[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST" | head -1)
WEB_HASH=$(sed -n 's/^[[:space:]]*"web_hash":[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST" | head -1)
[ -n "$REVISION" ] && [ -n "$WEB_HASH" ] || {
    echo "lerdr: installed release manifest has no identity" >&2
    exit 1
}

# Store the repository credential separately; the service receives only its
# path, so the relay and agent subprocesses never inherit it.
if [ -n "$INSTALL_TOKEN" ]; then
    GH_TOKEN="$INSTALL_TOKEN" ensure_relay_env "$TARGET_ENV"
fi
unset INSTALL_TOKEN

# Cut over an existing service to the new release root. A unit still carrying
# its pre-rename name is copied to the lerdr name first; the old file comes out
# only after the replacement is written and reloaded.
SERVICE_WRAPPER="$INSTALL_ROOT/current/scripts/tailscale-service.sh"
service_restarted=false
restarted_unit=
case "$PLATFORM" in
    Linux)
        UNIT_FILE="$SERVICE_FILE"
        if [ -f "$UNIT_FILE" ] && [ -x "$SERVICE_WRAPPER" ]; then
            echo "lerdr: updating service unit to new release..." >&2
            service_cutover_started=true
            if [ "$UNIT_FILE" != "$NEW_SERVICE_FILE" ]; then
                cp "$UNIT_FILE" "$NEW_SERVICE_FILE" || {
                    echo "lerdr: service unit could not be copied to its new name" >&2
                    exit 1
                }
            fi
            rewrite_service_release_paths \
                "$NEW_SERVICE_FILE" "$SERVICE_WRAPPER" "$INSTALL_ROOT/current" "$TARGET_ENV" || {
                echo "lerdr: service unit could not be updated safely" >&2
                exit 1
            }
            systemctl --user daemon-reload 2>/dev/null || true
            if [ "$UNIT_FILE" != "$NEW_SERVICE_FILE" ]; then
                if systemctl --user is-enabled --quiet "$(basename "$UNIT_FILE")" 2>/dev/null; then
                    systemctl --user enable lerdr.service 2>/dev/null || true
                fi
                systemctl --user disable --now "$(basename "$UNIT_FILE")" 2>/dev/null || true
                rm -f "$UNIT_FILE"
                systemctl --user daemon-reload 2>/dev/null || true
                service_file_renamed=true
            fi
            if [ "$service_should_run" = true ]; then
                echo "lerdr: restarting existing service..." >&2
                systemctl --user restart lerdr.service
                service_restarted=true
                restarted_unit=lerdr.service
            fi
        elif systemctl --user is-active --quiet lerdr.service 2>/dev/null; then
            echo "lerdr: restarting existing service..." >&2
            systemctl --user restart lerdr.service
            service_restarted=true
            restarted_unit=lerdr.service
        elif systemctl --user is-active --quiet herdr-mobile-relay.service 2>/dev/null; then
            echo "lerdr: restarting existing service..." >&2
            systemctl --user restart herdr-mobile-relay.service
            service_restarted=true
            restarted_unit=herdr-mobile-relay.service
        fi
        ;;
    Darwin)
        PLIST="$SERVICE_FILE"
        if [ -f "$PLIST" ] && [ -x "$SERVICE_WRAPPER" ]; then
            echo "lerdr: updating service plist to new release..." >&2
            service_cutover_started=true
            if [ "$PLIST" != "$NEW_SERVICE_FILE" ]; then
                cp "$PLIST" "$NEW_SERVICE_FILE" || {
                    echo "lerdr: service plist could not be copied to its new name" >&2
                    exit 1
                }
            fi
            rewrite_service_release_paths \
                "$NEW_SERVICE_FILE" "$SERVICE_WRAPPER" "$INSTALL_ROOT/current" "$TARGET_ENV" || {
                echo "lerdr: service plist could not be updated safely" >&2
                exit 1
            }
            if [ "$PLIST" != "$NEW_SERVICE_FILE" ]; then
                launchctl bootout "gui/$(id -u)/$(basename "$PLIST" .plist)" >/dev/null 2>&1 || true
                rm -f "$PLIST"
                service_file_renamed=true
            fi
            if [ "$service_should_run" = true ]; then
                echo "lerdr: reloading existing service..." >&2
                reload_launchd_service_definition \
                    "$NEW_SERVICE_FILE" "com.lerdr.service"
                service_restarted=true
                restarted_unit=com.lerdr.service
            fi
        elif launchd_service_loaded \
            "gui/$(id -u)/com.lerdr.service"; then
            echo "lerdr: restarting existing service..." >&2
            launchctl kickstart -k "gui/$(id -u)/com.lerdr.service"
            service_restarted=true
            restarted_unit=com.lerdr.service
        elif launchd_service_loaded \
            "gui/$(id -u)/com.herdr-mobile-relay.service"; then
            echo "lerdr: restarting existing service..." >&2
            launchctl kickstart -k "gui/$(id -u)/com.herdr-mobile-relay.service"
            service_restarted=true
            restarted_unit=com.herdr-mobile-relay.service
        fi
        ;;
esac

if [ "$service_restarted" = true ]; then
    PORT="$(env_file_setting "$TARGET_ENV" RELAY_PORT)"
    PORT="${PORT:-8375}"
    echo "lerdr: verifying replacement service identity..." >&2
    if ! wait_for_relay_release_health \
        "$PORT" 30 1 "$VERSION" "$REVISION" "$WEB_HASH" >/dev/null; then
        echo "lerdr: replacement service did not report the expected release identity" >&2
        exit 1
    fi
    case "$PLATFORM" in
        Linux)
            systemctl --user is-active --quiet "$restarted_unit" || {
                echo "lerdr: replacement service is not active" >&2
                exit 1
            }
            ;;
        Darwin)
            launchd_service_loaded \
                "gui/$(id -u)/$restarted_unit" || {
                echo "lerdr: replacement service is not loaded" >&2
                exit 1
            }
            ;;
    esac
fi

rollback_armed=false

# Nobody sees this script's output, so an install that only prints "release is
# ready" leaves a person with no idea what exists or what is still missing. The
# menu answers both and costs one keystroke to leave, so every install opens it,
# upgrades included. The action is invoked detached and after this build exits,
# since herdr will not open a pane for a plugin whose build is still running.
schedule_setup_menu() {
    [ "${LERDR_NO_AUTO_SETUP:-${HERDR_MOBILE_RELAY_NO_AUTO_SETUP:-}}" != 1 ] || return 0
    command -v herdr >/dev/null 2>&1 || return 0
    (
        sleep 2
        herdr plugin action invoke setup --plugin lerdr.events
    ) >/dev/null 2>&1 &
}

echo "" >&2
echo "lerdr: release $VERSION is ready." >&2
schedule_setup_menu
echo "lerdr: start setup with:" >&2
echo "  herdr plugin action invoke setup --plugin lerdr.events" >&2
