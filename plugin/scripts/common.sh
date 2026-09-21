#!/bin/bash

# Operator-facing variables read their LERDR_ name first and the pre-rename
# HERDR_ name as a fallback, so existing relay.env files and documented
# invocations keep working. Host-injected variables (HERDR_RELAY_TOKEN,
# HERDR_RELAY_PORT, HERDR_RELAY_HOST, HERDR_PLUGIN_CONFIG_DIR, HERDR_SOCKET_PATH,
# HERDR_BIN) are not folded through this: the herdr host exports them under
# their own names and this layer must not shadow that contract.
relay_env() {
    local lerdr_name="LERDR_$1"
    local herdr_name="HERDR_$1"
    printf '%s\n' "${!lerdr_name:-${!herdr_name:-}}"
}

relay_env_or() {
    local value
    value="$(relay_env "$1")"
    printf '%s\n' "${value:-$2}"
}

# env_file_setting FILE SUFFIX prints a relay env-file value, reading the
# LERDR_ key first and the HERDR_ key an older install may have written.
env_file_setting() {
    local env_file="$1"
    local suffix="$2"
    local value

    value="$(env_file_value "$env_file" "LERDR_$suffix")"
    if [ -z "$value" ]; then
        value="$(env_file_value "$env_file" "HERDR_$suffix")"
    fi
    printf '%s\n' "$value"
}

# Carries a pre-rename directory forward to its lerdr name when the new path
# does not exist yet, so installed state survives the product rename.
migrate_legacy_dir() {
    local legacy="$1"
    local current="$2"

    if [ -e "$current" ] || [ -L "$current" ] || [ ! -e "$legacy" ]; then
        return 0
    fi
    mv "$legacy" "$current"
}

relay_release_root() {
    local configured
    configured="$(relay_env RELEASE_ROOT)"
    if [ -n "$configured" ]; then
        printf '%s\n' "$configured"
        return
    fi
    migrate_legacy_dir \
        "${XDG_DATA_HOME:-$HOME/.local/share}/herdr-mobile-relay" \
        "${XDG_DATA_HOME:-$HOME/.local/share}/lerdr"
    printf '%s\n' "${XDG_DATA_HOME:-$HOME/.local/share}/lerdr"
}

# The Rust binary is lerdr-relay; earlier releases of this same install root
# carried the Go lerdr binary, and pre-rename installs used
# herdr-mobile-relay. Resolving an existing release walks the newest name
# first so a Go-era bundle still answers during a transition window.
relay_binary() {
    local binary
    local common_dir
    local packaged_dir
    local packaged_binary
    local configured
    local candidate

    common_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
    packaged_dir="$(dirname "$common_dir")"
    if [ -f "$packaged_dir/release-manifest.json" ]; then
        for candidate in lerdr-relay lerdr herdr-mobile-relay; do
            packaged_binary="$packaged_dir/$candidate"
            if [ -x "$packaged_binary" ]; then
                printf '%s\n' "$packaged_binary"
                return 0
            fi
        done
    fi
    configured="$(relay_env RELAY_BIN)"
    binary="${configured:-$(relay_release_root)/current/lerdr-relay}"
    if [ ! -x "$binary" ]; then
        for candidate in lerdr herdr-mobile-relay; do
            if [ -x "$(relay_release_root)/current/$candidate" ]; then
                binary="$(relay_release_root)/current/$candidate"
                break
            fi
        done
    fi
    # systemd ExecStart and chdir'd callers need an absolute path; a
    # repo-relative override like target/release/lerdr-relay is legal input.
    case "$binary" in
        /*) ;;
        *) binary="$(cd "$(dirname "$binary")" && pwd -P)/$(basename "$binary")" ;;
    esac
    if [ ! -x "$binary" ]; then
        echo "✗ Verified relay release is unavailable: $binary" >&2
        echo "  Reinstall the exact plugin version; production launchers do not build or fall back." >&2
        return 1
    fi
    printf '%s\n' "$binary"
}

# The plugin is installed from a git clone, so its verified release belongs to
# the repository that checkout points at: a fork or a private canary installs
# its own bundle instead of this project's. Anything that is not a plain GitHub
# owner/repo fails, leaving the installer's compiled default in place.
release_repository() {
    local checkout="$1"
    local url
    local owner_repo

    command -v git >/dev/null 2>&1 || return 1
    url="$(git -C "$checkout" remote get-url origin 2>/dev/null)" || return 1
    case "$url" in
        *github.com[:/]*) owner_repo="${url##*github.com}" ;;
        *) return 1 ;;
    esac
    owner_repo="${owner_repo#[:/]}"
    owner_repo="${owner_repo%.git}"
    case "$owner_repo" in
        */*/* | /* | */ | *[!A-Za-z0-9._/-]*) return 1 ;;
        */*) printf '%s\n' "$owner_repo" ;;
        *) return 1 ;;
    esac
}

relay_env_file() {
    local script_dir="$1"
    local config_dir
    local plugin_env
    local configured

    configured="$(relay_env RELAY_ENV)"
    if [ -n "$configured" ]; then
        printf '%s\n' "$configured"
        return
    fi
    if [ -z "${HERDR_PLUGIN_CONFIG_DIR:-}" ]; then
        printf '%s/.env\n' "$script_dir"
        return
    fi

    config_dir="$HERDR_PLUGIN_CONFIG_DIR"
    plugin_env="$config_dir/relay.env"
    mkdir -p "$config_dir"
    chmod 700 "$config_dir"
    if [ ! -f "$plugin_env" ] && [ -f "$script_dir/.env" ]; then
        umask 077
        cp "$script_dir/.env" "$plugin_env"
        chmod 600 "$plugin_env"
    fi
    if [ ! -d "$config_dir/push" ] && [ -d "$script_dir/push" ]; then
        umask 077
        cp -R "$script_dir/push" "$config_dir/push"
        chmod -R go-rwx "$config_dir/push"
    fi
    printf '%s\n' "$plugin_env"
}

canonical_file_path() {
    local path="$1"
    local directory
    local filename

    directory="$(dirname "$path")"
    filename="$(basename "$path")"
    if [ -d "$directory" ]; then
        directory="$(cd "$directory" && pwd -P)"
    fi
    printf '%s/%s\n' "${directory%/}" "$filename"
}

installed_service_env_file() {
    local service_file
    local legacy_file

    case "$(uname -s)" in
        Linux)
            service_file="$HOME/.config/systemd/user/lerdr.service"
            legacy_file="$HOME/.config/systemd/user/herdr-mobile-relay.service"
            if [ ! -r "$service_file" ]; then
                service_file="$legacy_file"
            fi
            if [ -r "$service_file" ]; then
                sed -nE 's/^Environment=(LERDR|HERDR)_RELAY_ENV=//p' "$service_file" | tail -1
            fi
            ;;
        Darwin)
            service_file="$HOME/Library/LaunchAgents/com.lerdr.service.plist"
            legacy_file="$HOME/Library/LaunchAgents/com.herdr-mobile-relay.service.plist"
            if [ ! -r "$service_file" ]; then
                service_file="$legacy_file"
            fi
            if [ -r "$service_file" ]; then
                awk '
                    /<key>(LERDR|HERDR)_RELAY_ENV<\/key>/ { found = 1; next }
                    found && /<string>/ {
                        sub(/^.*<string>/, "")
                        sub(/<\/string>.*$/, "")
                        print
                        exit
                    }
                ' "$service_file"
            fi
            ;;
    esac
}

update_launchd_release_paths() {
    local plist="$1"
    local service_wrapper="$2"
    local work_dir="$3"
    local env_file="${4:-}"
    local label="${5:-com.lerdr.service}"
    local env_key="${6:-LERDR_RELAY_ENV}"
    local stale_key=HERDR_RELAY_ENV
    local plist_buddy="${LERDR_PLIST_BUDDY:-${HERDR_PLIST_BUDDY:-/usr/libexec/PlistBuddy}}"

    [ -x "$plist_buddy" ] || {
        echo "PlistBuddy is unavailable: $plist_buddy" >&2
        return 1
    }
    [ "$env_key" = "$stale_key" ] && stale_key=LERDR_RELAY_ENV
    "$plist_buddy" -c "Set :Label $label" "$plist"
    "$plist_buddy" -c "Set :ProgramArguments:0 $service_wrapper" "$plist"
    "$plist_buddy" -c "Set :WorkingDirectory $work_dir" "$plist"
    # The other spelling is dropped rather than left to shadow the new key.
    "$plist_buddy" -c "Delete :EnvironmentVariables:$stale_key" "$plist" >/dev/null 2>&1 || true
    if [ -n "$env_file" ]; then
        "$plist_buddy" -c "Set :EnvironmentVariables:$env_key $env_file" "$plist"
    fi
}

require_user_service_context() {
    if [ "$(id -u)" -ne 0 ]; then
        return
    fi

    echo "Refusing to manage the Lerdr user service as root." >&2
    echo "Run the command again as the signed-in macOS or Linux user, without sudo." >&2
    return 1
}

launchd_service_loaded() {
    local service_target="$1"
    launchctl print "$service_target" >/dev/null 2>&1
}

reload_launchd_service_definition() {
    local plist="$1"
    local label="$2"
    local domain="gui/$(id -u)"
    local service_target="$domain/$label"
    local attempt
    local unloaded=false
    local bootstrapped=false

    require_user_service_context || return 1
    [ -f "$plist" ] && [ ! -L "$plist" ] || {
        echo "Cannot reload launchd service: plist is not a regular file: $plist" >&2
        return 1
    }
    if command -v plutil >/dev/null 2>&1; then
        plutil -lint "$plist" >/dev/null || {
            echo "Cannot reload launchd service: plist validation failed: $plist" >&2
            return 1
        }
    fi

    # A migration changes ProgramArguments, WorkingDirectory, and the relay
    # environment. Unload the plist using the form used by the legacy service
    # installer, then wait until launchd has actually removed its cached job.
    if launchd_service_loaded "$service_target"; then
        if ! launchctl bootout "$domain" "$plist"; then
            launchctl bootout "$service_target" || {
                echo "Could not unload launchd service $service_target" >&2
                return 1
            }
        fi
        for attempt in 1 2 3 4 5 6 7 8 9 10; do
            if ! launchd_service_loaded "$service_target"; then
                unloaded=true
                break
            fi
            sleep 1
        done
        if [ "$unloaded" != true ]; then
            echo "Timed out waiting for launchd to unload $service_target" >&2
            return 1
        fi
    fi

    # launchd can briefly reject bootstrap while completing a bootout. Retry
    # the registration, accepting success only when the exact job is loaded.
    for attempt in 1 2 3 4 5; do
        if [ "$attempt" -eq 5 ]; then
            if launchctl bootstrap "$domain" "$plist"; then
                bootstrapped=true
            fi
        elif launchctl bootstrap "$domain" "$plist" >/dev/null 2>&1; then
            bootstrapped=true
        fi
        if [ "$bootstrapped" = true ] ||
           launchd_service_loaded "$service_target"; then
            bootstrapped=true
            break
        fi
        sleep 1
    done
    if [ "$bootstrapped" != true ]; then
        echo "Could not bootstrap launchd service $service_target" >&2
        return 1
    fi

    launchctl enable "$service_target"
    launchctl kickstart -k "$service_target"
}

assert_service_env_matches() {
    local resolved_env
    local service_env

    resolved_env="$(canonical_file_path "$1")"
    service_env="$(installed_service_env_file)"
    if [ -z "$service_env" ]; then
        return
    fi
    service_env="$(canonical_file_path "$service_env")"
    if [ "$resolved_env" = "$service_env" ]; then
        return
    fi

    echo "✗ Refusing to use a different relay configuration than the installed service." >&2
    echo "  This command resolved: $resolved_env" >&2
    echo "  Installed service uses: $service_env" >&2
    echo "  Run the matching Herdr plugin action, or explicitly set:" >&2
    echo "  LERDR_RELAY_ENV=$service_env" >&2
    return 1
}

# A pane invoked straight from herdr has to hold the terminal open long enough
# to be read. Under the setup menu it must not: the menu pauses once on the way
# back, so a second prompt here would cost two keystrokes to return.
pause_before_close() {
    [ "$(relay_env SETUP_MENU)" != 1 ] || return 0
    if [ -t 0 ]; then
        echo ""
        read -r -p "Press Enter to close this pane." _answer
    fi
}

# A menu reads as a wall of text when the choice and its explanation carry the
# same weight. Bold the choice, but only when a terminal will render it: piped
# output stays plain for logs and tests, and NO_COLOR is honoured.
menu_item() {
    local key="$1"
    local title="$2"

    if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
        printf '  \033[1m%s. %s\033[0m\n' "$key" "$title"
        return 0
    fi
    printf '  %s. %s\n' "$key" "$title"
}

generate_token() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 16
        return
    fi
    if command -v uuidgen >/dev/null 2>&1; then
        uuidgen | tr '[:upper:]' '[:lower:]' | tr -d '-'
        return
    fi
    echo "Cannot generate a relay token: install openssl or uuidgen." >&2
    return 1
}

generate_instance_id() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 16
        return
    fi
    if command -v uuidgen >/dev/null 2>&1; then
        uuidgen | tr '[:upper:]' '[:lower:]'
        return
    fi
    echo "Cannot generate a relay instance ID: install openssl or uuidgen." >&2
    return 1
}

env_file_value() {
    local env_file="$1"
    local key="$2"

    if [ ! -f "$env_file" ]; then
        return
    fi
    (
        set -a
        # shellcheck source=/dev/null
        . "$env_file"
        set +a
        printenv "$key" 2>/dev/null || true
    )
}

set_env_value_atomic() {
    local env_file="$1"
    local key="$2"
    local value="$3"
    local directory
    local temp_file

    case "$value" in
        *"'"*)
            echo "Cannot write $key: single quotes are not supported in relay environment values." >&2
            return 1
            ;;
    esac

    directory="$(dirname "$env_file")"
    mkdir -p "$directory"
    temp_file="$(mktemp "$directory/.relay-env.XXXXXX")"
    if [ -f "$env_file" ]; then
        grep -v "^${key}=" "$env_file" > "$temp_file" || true
    fi
    printf "%s='%s'\n" "$key" "$value" >> "$temp_file"
    chmod 600 "$temp_file"
    mv "$temp_file" "$env_file"
}

remove_env_value_if_equals_atomic() {
    local env_file="$1"
    local key="$2"
    local expected="$3"
    local current
    local directory
    local temp_file

    if [ ! -f "$env_file" ]; then
        return
    fi
    current="$(
        set -a
        # shellcheck source=/dev/null
        . "$env_file"
        set +a
        printenv "$key" 2>/dev/null || true
    )"
    if [ "$current" != "$expected" ]; then
        return
    fi

    directory="$(dirname "$env_file")"
    temp_file="$(mktemp "$directory/.relay-env.XXXXXX")"
    grep -v "^${key}=" "$env_file" > "$temp_file" || true
    chmod 600 "$temp_file"
    mv "$temp_file" "$env_file"
}

remove_env_value_atomic() {
    local env_file="$1"
    local key="$2"
    local directory
    local temp_file

    if [ ! -f "$env_file" ] || ! grep -q "^${key}=" "$env_file"; then
        return
    fi
    directory="$(dirname "$env_file")"
    temp_file="$(mktemp "$directory/.relay-env.XXXXXX")"
    grep -v "^${key}=" "$env_file" > "$temp_file" || true
    chmod 600 "$temp_file"
    mv "$temp_file" "$env_file"
}

persist_github_token() {
    local env_file="$1"
    local token_file
    local temp_file

    if [ -z "${GH_TOKEN:-}" ]; then
        return 0
    fi
    token_file="$(dirname "$env_file")/github-token"
    temp_file="$(mktemp "$(dirname "$env_file")/.github-token.XXXXXX")"
    printf '%s\n' "$GH_TOKEN" > "$temp_file"
    chmod 600 "$temp_file"
    mv "$temp_file" "$token_file"
    set_env_value_atomic "$env_file" LERDR_GITHUB_TOKEN_FILE "$token_file"
}

append_env_default() {
    local env_file="$1"
    local key="$2"
    local value="$3"

    if grep -q "^${key}=" "$env_file"; then
        return
    fi
    set_env_value_atomic "$env_file" "$key" "$value"
}

ensure_relay_env() {
    local env_file="$1"
    local token

    if [ ! -f "$env_file" ]; then
        umask 077
        touch "$env_file"
        echo "Created $env_file"
    fi

    chmod 600 "$env_file"
    if ! grep -qE '^(LERDR|HERDR)_RELAY_TOKEN=' "$env_file" ||
       [ -z "$(env_file_setting "$env_file" RELAY_TOKEN)" ]; then
        # Both spellings carry the same token: a rollback to a pre-rename
        # binary still reads HERDR_RELAY_TOKEN.
        token="$(generate_token)"
        set_env_value_atomic "$env_file" HERDR_RELAY_TOKEN "$token"
        set_env_value_atomic "$env_file" LERDR_RELAY_TOKEN "$token"
    fi
    if ! grep -qE '^(LERDR|HERDR)_RELAY_INSTANCE_ID=' "$env_file" ||
       [ -z "$(env_file_setting "$env_file" RELAY_INSTANCE_ID)" ]; then
        set_env_value_atomic "$env_file" LERDR_RELAY_INSTANCE_ID "$(generate_instance_id)"
    fi
    persist_github_token "$env_file"
    # Migrate older installs that exposed the token to the complete service
    # process tree. Only the credential-file path remains in relay.env.
    remove_env_value_atomic "$env_file" GH_TOKEN
}

load_relay_env() {
    local env_file="$1"
    if [ -f "$env_file" ]; then
        set -a
        # shellcheck source=/dev/null
        . "$env_file"
        set +a
    fi
    # Mirror renamed keys onto the legacy spellings these scripts still read.
    # LERDR_ wins over HERDR_, matching the binary's own resolution.
    HERDR_RELAY_TOKEN="${LERDR_RELAY_TOKEN:-${HERDR_RELAY_TOKEN:-}}"
    HERDR_RELAY_HOST="${LERDR_RELAY_HOST:-${HERDR_RELAY_HOST:-}}"
    HERDR_RELAY_PORT="${LERDR_RELAY_PORT:-${HERDR_RELAY_PORT:-}}"
    HERDR_RELAY_PLUGIN_PORT="${LERDR_RELAY_PLUGIN_PORT:-${HERDR_RELAY_PLUGIN_PORT:-}}"
    HERDR_RELAY_INSTANCE_ID="${LERDR_RELAY_INSTANCE_ID:-${HERDR_RELAY_INSTANCE_ID:-}}"
}

wait_for_relay_health() {
    local port="${1:-8375}"
    local attempts="${2:-15}"
    local delay="${3:-1}"
    local health
    local attempt

    if ! command -v curl >/dev/null 2>&1; then
        echo "curl is required to verify relay health." >&2
        return 1
    fi

    case "$attempts" in
        ""|*[!0-9]*|0)
            echo "Health-check attempts must be a positive integer." >&2
            return 1
            ;;
    esac

    for ((attempt = 1; attempt <= attempts; attempt++)); do
        if health="$(curl -fsS --max-time 2 "http://127.0.0.1:$port/healthz" 2>/dev/null)"; then
            case "$health" in
                *'"status": "ok"'*|*'"status":"ok"'*)
                    if [[ "$health" == *'"instance":'* && "$health" == *'"version":'* && "$health" == *'"protocol":'* ]]; then
                        printf '%s\n' "$health"
                        return 0
                    fi
                    ;;
            esac
        fi
        if [ "$attempt" -lt "$attempts" ]; then
            sleep "$delay"
        fi
    done

    return 1
}

json_string_field() {
    local json="$1"
    local key="$2"
    printf '%s\n' "$json" |
        sed -n "s/.*\"$key\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p" |
        head -1
}

verify_relay_release_health() {
    local health="$1"
    local expected_version="$2"
    local expected_revision="$3"
    local expected_web_hash="$4"

    [ "$(json_string_field "$health" status)" = "ok" ] &&
        [ "$(json_string_field "$health" release_version)" = "$expected_version" ] &&
        [ "$(json_string_field "$health" revision)" = "$expected_revision" ] &&
        [ "$(json_string_field "$health" bundle_hash)" = "$expected_web_hash" ]
}

wait_for_relay_release_health() {
    local port="$1"
    local attempts="$2"
    local delay="$3"
    local expected_version="$4"
    local expected_revision="$5"
    local expected_web_hash="$6"
    local health
    local attempt

    [ -n "$expected_version" ] &&
        [ -n "$expected_revision" ] &&
        [ -n "$expected_web_hash" ] || {
            echo "Exact release health verification requires version, revision, and web hash." >&2
            return 1
        }

    case "$attempts" in
        ""|*[!0-9]*|0)
            echo "Health-check attempts must be a positive integer." >&2
            return 1
            ;;
    esac

    for ((attempt = 1; attempt <= attempts; attempt++)); do
        if health="$(wait_for_relay_health "$port" 1 0)" &&
           verify_relay_release_health \
               "$health" "$expected_version" "$expected_revision" "$expected_web_hash"; then
            printf '%s\n' "$health"
            return 0
        fi
        if [ "$attempt" -lt "$attempts" ]; then
            sleep "$delay"
        fi
    done

    return 1
}

host_label() {
    hostname -s 2>/dev/null || hostname 2>/dev/null || echo relay
}

# The token passes through argv only for the short-lived compiled helper.
build_setup_fragment() {
    "$(relay_binary)" setup-fragment "$1" "$2" "${3:-}"
}

# OSC 8 is a terminal protocol, not a vendor feature. Use it for any interactive
# terminal unless the user or terminal explicitly asks for plain output; modern
# terminals understand it and older ones safely ignore it.
stdout_is_terminal() {
    [ -t 1 ]
}

terminal_hyperlinks_enabled() {
    stdout_is_terminal &&
        [ -z "${NO_COLOR+x}" ] &&
        [ "${TERM:-}" != dumb ]
}

# Setup URLs enter both the QR encoder and an OSC 8 sequence. Raw whitespace is
# not valid in a URL, and control bytes could terminate the terminal sequence.
# The compiled origin parser enforces HTTPS, with canonical loopback HTTP only.
phone_setup_url_is_safe() {
    local url="$1"
    local origin
    local normalized

    case "$url" in
        *[[:cntrl:]]* | *[[:space:]]*) return 1 ;;
    esac
    if [[ ! "$url" =~ ^(https?://[^/?#]+) ]]; then
        return 1
    fi
    origin="${BASH_REMATCH[1]}"
    if ! normalized="$("$(relay_binary)" normalize-origin \
        --allow-loopback-http "$origin" 2>/dev/null)"; then
        return 1
    fi
    case "$origin" in
        https://*) return 0 ;;
        *) [ "$normalized" = "$origin" ] ;;
    esac
}

# Prints an indented terminal QR code for the URL, or nothing when it cannot
# be drawn because the terminal is too narrow. A wrapped QR is worse than the
# plain link.
# Callers must keep working with empty output. Kept separate from
# build_setup_fragment on purpose: this call is allowed to fail, that one
# is not.
render_setup_qr() {
    local url="$1"
    local cols
    cols="$(tput cols 2>/dev/null || true)"
    "$(relay_binary)" qr --columns "${cols:-80}" "$url" 2>/dev/null || true
}

# The URL has already been validated before this internal emitter is called.
emit_phone_setup_url() {
    local phone_url="$1"

    if terminal_hyperlinks_enabled; then
        printf '  \033]8;;%s\033\\%s\033]8;;\033\\\n' "$phone_url" "$phone_url"
    else
        printf '  %s\n' "$phone_url"
    fi
}

print_phone_setup_url() {
    phone_setup_url_is_safe "$1" || return 1
    emit_phone_setup_url "$1"
}

# Shared tail of the setup-link output: QR code when possible,
# always the link. Invalid values fail before either output sink sees them.
print_phone_setup() {
    local phone_url="$1"
    local qr_code

    phone_setup_url_is_safe "$phone_url" || return 1
    qr_code="$(render_setup_qr "$phone_url")"
    if [ -n "$qr_code" ]; then
        echo "  Scan this QR code with your phone camera:"
        echo ""
        printf '%s\n' "$qr_code"
        echo ""
        echo "  This code contains your relay token; do not share screenshots of it."
        echo ""
        echo "  Or open this private setup link on your phone:"
    else
        echo "  Open this private setup link on your phone:"
    fi
    emit_phone_setup_url "$phone_url"
}

# The bootstrap invitation in a setup link is one-use: the first phone that
# pairs consumes it. Printing the link is the operator asking for one more
# pairing, so the running relay is told to arm a fresh invitation before the
# link is shown. The relay records its pid beside relay.env and re-arms on
# SIGUSR1. Returns 1 when no relay is running here.
arm_setup_link() {
    local env_file="$1"
    local pid

    pid="$(head -1 "$(dirname "$env_file")/relay.pid" 2>/dev/null || true)"
    case "$pid" in
        ''|*[!0-9]*) return 1 ;;
    esac
    kill -0 "$pid" 2>/dev/null || return 1
    kill -USR1 "$pid" 2>/dev/null || return 1
    sleep 0.3
}

print_setup_link_arming() {
    if [ "$1" -eq 0 ]; then
        echo "  This link pairs one phone within 10 minutes. Print it again for another."
    else
        echo "  The relay is not running here; start it, then print the link again."
    fi
}

require_supported_platform() {
    case "$(uname -s)" in
        Darwin|Linux)
            return
            ;;
        *)
            echo "Unsupported platform: Lerdr currently supports only Linux and macOS."
            exit 1
            ;;
    esac
}
