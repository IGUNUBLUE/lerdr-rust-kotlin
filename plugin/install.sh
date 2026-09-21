#!/bin/sh
# Install one exact, complete Lerdr release. No toolchain needed.
set -eu

REPO=${LERDR_RELEASE_REPOSITORY:-${HERDR_RELEASE_REPOSITORY:-IGUNUBLUE/lerdr}}
BINARY=lerdr-relay
# Earlier releases of this same install root shipped the Go lerdr binary;
# pre-rename installs used herdr-mobile-relay. Both names resolve when reading
# an existing install, but release archives only ever ship lerdr-relay.
LEGACY_BINARIES="lerdr herdr-mobile-relay"
SENTINEL_NAME=.lerdr-installation
LEGACY_SENTINEL=.herdr-mobile-relay-installation

info() { printf '==> %s\n' "$1" >&2; }
fatal() { printf 'error: %s\n' "$1" >&2; exit 1; }

detect_os() {
    case "$(uname -s)" in
        Linux) printf '%s\n' linux ;;
        Darwin) printf '%s\n' darwin ;;
        *) fatal "unsupported OS: $(uname -s)" ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64) printf '%s\n' amd64 ;;
        aarch64|arm64) printf '%s\n' arm64 ;;
        *) fatal "unsupported architecture: $(uname -m)" ;;
    esac
}

run_with_timeout() {
    lerdr_timeout_seconds=$1
    shift
    "$@" &
    lerdr_command_pid=$!
    lerdr_elapsed=0
    while kill -0 "$lerdr_command_pid" 2>/dev/null; do
        if [ "$lerdr_elapsed" -ge "$lerdr_timeout_seconds" ]; then
            kill -9 "$lerdr_command_pid" 2>/dev/null || true
            wait "$lerdr_command_pid" 2>/dev/null || true
            lerdr_command_pid=
            return 124
        fi
        sleep 1
        lerdr_elapsed=$((lerdr_elapsed + 1))
    done
    if wait "$lerdr_command_pid"; then
        lerdr_status=0
    else
        lerdr_status=$?
    fi
    lerdr_command_pid=
    return "$lerdr_status"
}

terminate_active_command() {
    lerdr_signal=$1
    if [ -z "${lerdr_command_pid:-}" ]; then
        return 0
    fi
    kill "$lerdr_signal" "$lerdr_command_pid" 2>/dev/null || true
    lerdr_signal_elapsed=0
    while kill -0 "$lerdr_command_pid" 2>/dev/null; do
        if [ "$lerdr_signal_elapsed" -ge 2 ]; then
            kill -KILL "$lerdr_command_pid" 2>/dev/null || true
            break
        fi
        sleep 1
        lerdr_signal_elapsed=$((lerdr_signal_elapsed + 1))
    done
    wait "$lerdr_command_pid" 2>/dev/null || true
    lerdr_command_pid=
}

on_install_exit() {
    terminate_active_command -TERM
    if [ -n "${work_dir:-}" ]; then
        rm -rf "$work_dir"
    fi
}

on_install_signal() {
    lerdr_signal=$1
    lerdr_exit_status=$2
    terminate_active_command "$lerdr_signal"
    exit "$lerdr_exit_status"
}

fetch() {
    if command -v curl >/dev/null 2>&1; then
        if [ -n "${GH_TOKEN:-}" ]; then
            run_with_timeout 120 curl --fail --show-error --silent --location \
                --connect-timeout 10 --max-time 120 --output "$2" \
                -H "Authorization: token ${GH_TOKEN}" \
                -H "Accept: application/octet-stream" "$1"
        else
            run_with_timeout 120 curl --fail --show-error --silent --location \
                --connect-timeout 10 --max-time 120 --output "$2" "$1"
        fi
    elif command -v wget >/dev/null 2>&1; then
        if [ -n "${GH_TOKEN:-}" ]; then
            run_with_timeout 120 wget --quiet --timeout=120 --tries=1 --output-document="$2" \
                --header="Authorization: token ${GH_TOKEN}" \
                --header="Accept: application/octet-stream" "$1"
        else
            run_with_timeout 120 wget --quiet --timeout=120 --tries=1 --output-document="$2" "$1"
        fi
    else
        fatal "curl or wget is required"
    fi
}

fetch_json() {
    if command -v curl >/dev/null 2>&1; then
        if [ -n "${GH_TOKEN:-}" ]; then
            run_with_timeout 120 curl --fail --show-error --silent --location \
                --connect-timeout 10 --max-time 120 \
                -H "Authorization: token ${GH_TOKEN}" \
                -H "Accept: application/vnd.github+json" "$1"
        else
            run_with_timeout 120 curl --fail --show-error --silent --location \
                --connect-timeout 10 --max-time 120 \
                -H "Accept: application/vnd.github+json" "$1"
        fi
    else
        if [ -n "${GH_TOKEN:-}" ]; then
            run_with_timeout 120 wget --quiet --timeout=120 --tries=1 --output-document=- \
                --header="Authorization: token ${GH_TOKEN}" \
                --header="Accept: application/vnd.github+json" "$1"
        else
            run_with_timeout 120 wget --quiet --timeout=120 --tries=1 --output-document=- \
                --header="Accept: application/vnd.github+json" "$1"
        fi
    fi
}

resolve_asset_url() {
    release_json=$1
    asset_name=$2
    # GitHub places the asset API URL before the asset name and nested uploader
    # URLs after it. Split at every URL key, then select the record whose
    # following fields contain the exact asset name.
    printf '%s' "$release_json" |
        tr -d '\n\r\t ' |
        sed 's/"url":"/\
"url":"/g' |
        awk -v name="\"name\":\"$asset_name\"" '
            index($0, name) == 0 { next }
            {
                line = $0
                sub(/^"url":"/, "", line)
                sub(/".*$/, "", line)
                print line
                exit
            }
        '
}

resolve_tag_revision() {
    commit_json=$1
    printf '%s' "$commit_json" |
        tr -d '\n\r\t ' |
        sed 's/"sha":"/\
"sha":"/' |
        sed -n 's/^"sha":"\([0-9a-fA-F][0-9a-fA-F]*\)".*/\1/p' |
        head -1
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

validate_archive_paths() {
    tar -tzf "$1" | awk '
        {
            name = $0
            sub(/^\.\//, "", name)
            if (name ~ /^\// || name ~ /(^|\/)\.\.(\/|$)/ || name ~ /\\/) {
                bad = 1
            }
        }
        END { exit bad ? 1 : 0 }
    ' || return 1
    tar -tvzf "$1" | awk '
        {
            kind = substr($1, 1, 1)
            if (kind != "-" && kind != "d") {
                bad = 1
            }
        }
        END { exit bad ? 1 : 0 }
    ' || return 1
}

legacy_root_entry_allowed() {
    root_kind=$1
    entry_name=$2
    entry_path=$3
    case "$root_kind:$entry_name" in
        config:relay.env|config:.env|config:phone-app-origin|config:phone-app-origin-configured|config:stable-setup.json|\
        config:update-state.json|config:app-deploy-state.json|config:support-state.json|config:github-token|\
        config:update.lock|config:app-deploy.lock)
            [ -f "$entry_path" ] && [ ! -L "$entry_path" ]
            ;;
        config:update-job-*.json|config:app-deploy-job-*.json)
            [ -f "$entry_path" ] && [ ! -L "$entry_path" ]
            ;;
        config:device-auth|config:push|config:cloudflared)
            [ -d "$entry_path" ] && [ ! -L "$entry_path" ]
            ;;
        cache:activity.jsonl|cache:activity.tombstones|cache:post-install.sh|cache:post-install.log|\
        cache:approval-verification*|cache:notification-approval-fix-test-*)
            [ -f "$entry_path" ] && [ ! -L "$entry_path" ]
            ;;
        cache:claude-history|cache:uploads|cache:push)
            [ -d "$entry_path" ] && [ ! -L "$entry_path" ]
            ;;
        *)
            return 1
            ;;
    esac
}

validate_legacy_root() {
    legacy_root=$1
    root_kind=$2
    [ -d "$legacy_root" ] && [ ! -L "$legacy_root" ] || return 1
    [ -z "$(find "$legacy_root" -type l -print -quit)" ] || return 1
    found=false
    cache_state=false
    cache_waiter_script=false
    cache_waiter_log=false
    for legacy_entry in "$legacy_root"/* "$legacy_root"/.[!.]* "$legacy_root"/..?*; do
        [ -e "$legacy_entry" ] || continue
        legacy_name=${legacy_entry##*/}
        case "$legacy_name" in
            "$SENTINEL_NAME"|"$LEGACY_SENTINEL") continue ;;
        esac
        legacy_root_entry_allowed "$root_kind" "$legacy_name" "$legacy_entry" || return 1
        found=true
        case "$legacy_name" in
            activity.jsonl|claude-history|uploads) cache_state=true ;;
            post-install.sh) cache_waiter_script=true ;;
            post-install.log) cache_waiter_log=true ;;
        esac
    done
    [ "$found" = true ] || return 1
    if [ "$root_kind" = config ]; then
        legacy_env=
        [ ! -f "$legacy_root/relay.env" ] || legacy_env="$legacy_root/relay.env"
        [ -n "$legacy_env" ] || [ ! -f "$legacy_root/.env" ] || legacy_env="$legacy_root/.env"
        [ -n "$legacy_env" ] || return 1
        grep -q '^HERDR_RELAY_TOKEN=' "$legacy_env" || return 1
    elif [ "$root_kind" = cache ]; then
        [ "$cache_state" = true ] ||
            { [ "$cache_waiter_script" = true ] && [ "$cache_waiter_log" = true ]; } ||
            return 1
    fi
}

verify_sentinel() {
    sentinel_file=$1
    sentinel_root=$2
    canonical_root=$(CDPATH='' cd "$sentinel_root" && pwd -P)
    grep -Fx "root=$canonical_root" "$sentinel_file" >/dev/null || return 1
    grep -Fx 'product=lerdr' "$sentinel_file" >/dev/null ||
        grep -Fx 'product=herdr-mobile-relay' "$sentinel_file" >/dev/null
}

write_install_sentinel() {
    sentinel_root=$1
    root_kind=${2:-new}
    sentinel="$sentinel_root/$SENTINEL_NAME"
    legacy_sentinel="$sentinel_root/$LEGACY_SENTINEL"
    # A pre-rename install carried the old sentinel name: verify ownership under
    # the old product name, then rewrite it as the current sentinel.
    sentinel_owned=
    if [ -f "$legacy_sentinel" ] && [ ! -e "$sentinel" ]; then
        verify_sentinel "$legacy_sentinel" "$sentinel_root" ||
            fatal "installation root has a mismatched ownership sentinel: $sentinel_root"
        rm -f "$legacy_sentinel"
        sentinel_owned=1
    fi
    if [ -f "$sentinel" ]; then
        verify_sentinel "$sentinel" "$sentinel_root" ||
            fatal "installation root has a mismatched ownership sentinel: $sentinel_root"
        return
    fi
    if [ -e "$sentinel_root" ]; then
        [ -d "$sentinel_root" ] ||
            fatal "installation root is not a directory: $sentinel_root"
        if [ -z "$sentinel_owned" ] &&
           [ -n "$(find "$sentinel_root" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
            case "$root_kind" in
                config|cache)
                    validate_legacy_root "$sentinel_root" "$root_kind" ||
                        fatal "refusing to claim nonempty directory without a validated Python 0.8.6 layout: $sentinel_root"
                    ;;
                *)
                    fatal "refusing to claim nonempty directory without an ownership sentinel: $sentinel_root"
                    ;;
            esac
        fi
    else
        mkdir -p "$sentinel_root"
    fi
    chmod 700 "$sentinel_root"
    canonical_root=$(CDPATH='' cd "$sentinel_root" && pwd -P)
    sentinel_temp="$sentinel_root/$SENTINEL_NAME.$$"
    {
        printf 'product=lerdr\n'
        printf 'root=%s\n' "$canonical_root"
    } > "$sentinel_temp"
    chmod 600 "$sentinel_temp"
    mv -f "$sentinel_temp" "$sentinel"
}

# The release root keeps a `current` symlink into its own releases/ directory.
# A link recorded as an absolute path breaks when the root is renamed, so it is
# repointed at the new root while relative links survive a move untouched.
fixup_current_link() {
    root=$1
    old_root=$2
    [ -L "$root/current" ] || return 0
    current_target=$(readlink "$root/current")
    case "$current_target" in
        "$old_root"/*)
            rm -f "$root/current"
            ln -s "$root/${current_target#"$old_root"/}" "$root/current" || return 1
            ;;
    esac
}

# Carries a pre-rename install root forward to its lerdr name. A missing new
# root takes the whole old directory; when both exist (for example a fresh
# install ran before this upgrade) the old entries merge in without replacing
# anything, and leftovers stay behind rather than being deleted.
migrate_install_root() {
    new_root=$1
    old_root=$2
    root_kind=${3:-new}
    [ "$new_root" != "$old_root" ] || return 0
    [ -e "$old_root" ] || [ -L "$old_root" ] || return 0
    [ ! -L "$old_root" ] ||
        fatal "refusing to migrate a symlinked installation root: $old_root"
    [ -d "$old_root" ] ||
        fatal "refusing to migrate a non-directory installation root: $old_root"
    if [ ! -e "$old_root/$SENTINEL_NAME" ] && [ ! -e "$old_root/$LEGACY_SENTINEL" ]; then
        case "$root_kind" in
            config|cache)
                validate_legacy_root "$old_root" "$root_kind" ||
                    fatal "refusing to migrate unowned directory: $old_root"
                ;;
            *)
                fatal "refusing to migrate directory without an ownership sentinel: $old_root"
                ;;
        esac
    fi
    if [ ! -e "$new_root" ] && [ ! -L "$new_root" ]; then
        mv "$old_root" "$new_root" ||
            fatal "could not migrate pre-rename directory $old_root"
        # A sentinel carried by the move still records the old canonical root;
        # repoint it so ownership verification sees the directory's new home.
        for carried in "$new_root/$SENTINEL_NAME" "$new_root/$LEGACY_SENTINEL"; do
            if [ -f "$carried" ]; then
                carried_new=$(CDPATH='' cd "$new_root" && pwd -P)
                carried_temp="$carried.$$"
                sed "s|^root=.*|root=$carried_new|" "$carried" > "$carried_temp" &&
                    chmod 600 "$carried_temp" &&
                    mv -f "$carried_temp" "$carried" ||
                    fatal "could not repoint ownership sentinel in $new_root"
            fi
        done
        fixup_current_link "$new_root" "$old_root" ||
            fatal "could not repoint current release link in $new_root"
        info "Migrated pre-rename directory $old_root to $new_root"
        return 0
    fi
    [ -d "$new_root" ] ||
        fatal "cannot merge $old_root into a non-directory: $new_root"
    for old_entry in "$old_root"/* "$old_root"/.[!.]* "$old_root"/..?*; do
        [ -e "$old_entry" ] || [ -L "$old_entry" ] || continue
        old_name=${old_entry##*/}
        case "$old_name" in
            "$SENTINEL_NAME"|"$LEGACY_SENTINEL")
                [ ! -e "$new_root/$SENTINEL_NAME" ] &&
                    [ ! -e "$new_root/$LEGACY_SENTINEL" ] || continue
                ;;
            releases)
                if [ -d "$new_root/releases" ]; then
                    for old_release in "$old_entry"/*; do
                        [ -e "$old_release" ] || [ -L "$old_release" ] || continue
                        release_name=${old_release##*/}
                        if [ ! -e "$new_root/releases/$release_name" ]; then
                            mv "$old_release" "$new_root/releases/$release_name" ||
                                fatal "could not migrate release $release_name"
                        fi
                    done
                    rmdir "$old_entry" 2>/dev/null || true
                    continue
                fi
                ;;
        esac
        if [ ! -e "$new_root/$old_name" ] && [ ! -L "$new_root/$old_name" ]; then
            mv "$old_entry" "$new_root/$old_name" ||
                fatal "could not migrate $old_entry"
        fi
    done
    fixup_current_link "$new_root" "$old_root" ||
        fatal "could not repoint current release link in $new_root"
    rmdir "$old_root" 2>/dev/null ||
        info "Kept pre-rename leftovers in $old_root"
    [ -d "$old_root" ] ||
        info "Migrated pre-rename directory $old_root into $new_root"
}

prepare_install_roots() {
    release_root=$1
    config_root=$2
    cache_root=$3
    migrate_release=${4:-0}
    migrate_config=${5:-0}
    legacy_release_root="${XDG_DATA_HOME:-$HOME/.local/share}/herdr-mobile-relay"
    legacy_config_root="${XDG_CONFIG_HOME:-$HOME/.config}/herdr-mobile-relay"
    legacy_cache_root="${XDG_CACHE_HOME:-$HOME/.cache}/herdr-mobile-relay"

    [ "$migrate_release" = 1 ] &&
        migrate_install_root "$release_root" "$legacy_release_root" new
    [ "$migrate_config" = 1 ] &&
        migrate_install_root "$config_root" "$legacy_config_root" config
    migrate_install_root "$cache_root" "$legacy_cache_root" cache

    write_install_sentinel "$release_root" new
    write_install_sentinel "$config_root" config
    write_install_sentinel "$cache_root" cache
}

retire_legacy_service() {
    case "$(uname -s)" in
        Linux)
            command -v systemctl >/dev/null 2>&1 || return 0
            if [ -f "$HOME/.config/systemd/user/herdr-mobile-relay.service" ] ||
               systemctl --user is-active --quiet herdr-mobile-relay.service 2>/dev/null ||
               systemctl --user is-enabled --quiet herdr-mobile-relay.service 2>/dev/null; then
                systemctl --user stop herdr-mobile-relay.service 2>/dev/null || true
                systemctl --user disable herdr-mobile-relay.service 2>/dev/null || true
                info "Stopped and disabled pre-rename service herdr-mobile-relay.service"
            fi
            ;;
        Darwin)
            legacy_label=com.herdr-mobile-relay.service
            legacy_plist="$HOME/Library/LaunchAgents/$legacy_label.plist"
            if [ -f "$legacy_plist" ] ||
               launchctl print "gui/$(id -u)/$legacy_label" >/dev/null 2>&1; then
                launchctl bootout "gui/$(id -u)" "$legacy_plist" 2>/dev/null ||
                    launchctl bootout "gui/$(id -u)/$legacy_label" 2>/dev/null || true
                info "Unloaded pre-rename service $legacy_label"
            fi
            ;;
    esac
}

main() {
    command -v tar >/dev/null 2>&1 || fatal "tar is required"
    command -v awk >/dev/null 2>&1 || fatal "awk is required"
    command -v find >/dev/null 2>&1 || fatal "find is required"

    version=${VERSION:-${1:-}}
    [ -n "$version" ] || fatal "an exact VERSION is required; unpinned latest installs are refused"
    version=${version#v}
    case "$version" in
        *[!0-9.]*|.*|*..*|*.) fatal "VERSION must use MAJOR.MINOR.PATCH" ;;
    esac
    [ "$(printf '%s' "$version" | awk -F. '{print NF}')" -eq 3 ] ||
        fatal "VERSION must use MAJOR.MINOR.PATCH"

    os=$(detect_os)
    arch=$(detect_arch)
    target="$os/$arch"
    archive="${BINARY}_${version}_${os}_${arch}.tar.gz"
    tag="v$version"
    default_release_root="${XDG_DATA_HOME:-$HOME/.local/share}/lerdr"
    release_root=${INSTALL_ROOT:-$default_release_root}
    shim_dir=${BIN_DIR:-"$HOME/.local/bin"}
    relay_env_file=${LERDR_RELAY_ENV:-${HERDR_RELAY_ENV:-}}
    default_config_root="${XDG_CONFIG_HOME:-$HOME/.config}/lerdr"
    if [ -n "$relay_env_file" ]; then
        config_root=$(dirname "$relay_env_file")
    else
        config_root=${HERDR_PLUGIN_CONFIG_DIR:-$default_config_root}
    fi
    cache_root="${XDG_CACHE_HOME:-$HOME/.cache}/lerdr"
    migrate_release=0
    migrate_config=0
    [ "$release_root" = "$default_release_root" ] && migrate_release=1
    [ "$config_root" = "$default_config_root" ] && migrate_config=1

    work_dir=$(mktemp -d "${TMPDIR:-/tmp}/lerdr-install.XXXXXX")
    trap 'on_install_exit' EXIT
    trap 'on_install_signal -INT 130' INT
    trap 'on_install_signal -TERM 143' TERM
    archive_path="$work_dir/$archive"
    checksums_path="$work_dir/checksums.txt"
    stage="$work_dir/release"
    commit_json_path="$work_dir/commit.json"

    info "Resolving ${BINARY} ${version} (${target}) from ${REPO}"
    fetch_json "https://api.github.com/repos/${REPO}/commits/${tag}" > "$commit_json_path" ||
        fatal "could not resolve release tag from GitHub API (private repositories require GH_TOKEN; SSH access authenticates Git only)"
    commit_json=$(awk '{ printf "%s", $0 }' "$commit_json_path")
    tag_revision=$(resolve_tag_revision "$commit_json")
    case "$tag_revision" in
        ????????????????????????????????????????) ;;
        *) fatal "release tag did not resolve to an exact commit" ;;
    esac
    info "Downloading ${archive} from ${REPO}"
    if [ -n "${GH_TOKEN:-}" ]; then
        api_url="https://api.github.com/repos/${REPO}/releases/tags/${tag}"
        release_json_path="$work_dir/release.json"
        fetch_json "$api_url" > "$release_json_path" ||
            fatal "could not fetch release metadata from GitHub API"
        release_json=$(awk '{ printf "%s", $0 }' "$release_json_path")
        archive_url=$(resolve_asset_url "$release_json" "$archive")
        checksum_url=$(resolve_asset_url "$release_json" "checksums.txt")
        [ -n "$archive_url" ] || fatal "release has no asset named $archive"
        [ -n "$checksum_url" ] || fatal "release has no asset named checksums.txt"
        fetch "$checksum_url" "$checksums_path" ||
            fatal "required checksums.txt download failed"
        fetch "$archive_url" "$archive_path" ||
            fatal "release archive download failed"
    else
        base_url=${LERDR_RELEASE_BASE_URL:-${HERDR_RELEASE_BASE_URL:-"https://github.com/${REPO}/releases/download/${tag}"}}
        fetch "$base_url/checksums.txt" "$checksums_path" ||
            fatal "required checksums.txt download failed"
        fetch "$base_url/$archive" "$archive_path" ||
            fatal "release archive download failed"
    fi

    matches=$(awk -v name="$archive" '
        NF == 2 {
            file = $2
            sub(/^\*/, "", file)
            if (file == name) print tolower($1)
        }
    ' "$checksums_path")
    count=$(printf '%s\n' "$matches" | awk 'NF { count++ } END { print count + 0 }')
    [ "$count" -eq 1 ] || fatal "checksums.txt must contain one exact entry for $archive"
    expected=$(printf '%s\n' "$matches" | awk 'NF { print; exit }')
    actual=$(sha256_file "$archive_path")
    [ "$expected" = "$actual" ] || fatal "checksum mismatch for $archive"

    validate_archive_paths "$archive_path" || fatal "archive contains an unsafe path"
    mkdir -p "$stage"
    chmod 700 "$stage"
    tar -xzf "$archive_path" -C "$stage" || fatal "release extraction failed"
    # Release archives ship lerdr-relay; a Go-era bundle still carrying the old
    # executable name resolves the same way the runtime wrappers do.
    release_binary=
    for candidate in $BINARY $LEGACY_BINARIES; do
        if [ -x "$stage/$candidate" ]; then
            release_binary=$candidate
            break
        fi
    done
    [ -n "$release_binary" ] || fatal "archive is missing the relay executable"
    [ -f "$stage/release-manifest.json" ] || fatal "archive is missing release-manifest.json"

    "$stage/$release_binary" verify-release --target "$target" "$stage" >/dev/null ||
        fatal "offline release verification failed"
    manifest_version=$(sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$stage/release-manifest.json" | head -1)
    revision=$(sed -n 's/^[[:space:]]*"revision":[[:space:]]*"\([^"]*\)".*/\1/p' "$stage/release-manifest.json" | head -1)
    [ "$manifest_version" = "$version" ] || fatal "release manifest version mismatch"
    [ "$revision" = "$tag_revision" ] ||
        fatal "release manifest revision does not match tag commit"

    releases_dir="$release_root/releases"
    final_dir="$releases_dir/${version}-${revision}-${os}-${arch}"
    prepare_install_roots "$release_root" "$config_root" "$cache_root" \
        "$migrate_release" "$migrate_config"
    # Read `current` only after migration: fixup_current_link may have
    # repointed an absolute link left over from the pre-rename root.
    previous_dir=
    if [ -L "$release_root/current" ]; then
        previous_link=$(readlink "$release_root/current")
        case "$previous_link" in
            /*) previous_dir=$previous_link ;;
            *) previous_dir="$release_root/$previous_link" ;;
        esac
    fi
    mkdir -p "$releases_dir" "$shim_dir"
    chmod 700 "$release_root" "$releases_dir"
    if [ -e "$final_dir" ]; then
        "$stage/$release_binary" verify-release --target "$target" "$final_dir" >/dev/null ||
            fatal "existing target release directory is invalid"
    else
        mv "$stage" "$final_dir" || fatal "could not install release directory"
    fi
    "$final_dir/$release_binary" seal-release "$final_dir" ||
        fatal "could not seal installed release directory"
    if [ -n "$previous_dir" ]; then
        "$final_dir/$release_binary" prune-releases "$release_root" "$final_dir" "$previous_dir" ||
            fatal "could not prune obsolete releases"
    else
        "$final_dir/$release_binary" prune-releases "$release_root" "$final_dir" ||
            fatal "could not prune obsolete releases"
    fi
    # A still-running pre-rename service would hold the old install alive.
    retire_legacy_service
    shim_temp="$shim_dir/.${BINARY}.$$"
    rm -f "$shim_temp"
    ln -s "$release_root/current/$release_binary" "$shim_temp"
    mv -f "$shim_temp" "$shim_dir/$BINARY" ||
        fatal "could not install executable shim"
    for legacy_binary in $LEGACY_BINARIES; do
        legacy_shim="$shim_dir/$legacy_binary"
        if [ -L "$legacy_shim" ]; then
            legacy_shim_target=$(readlink "$legacy_shim")
            case "$legacy_shim_target" in
                "$release_root"/*|"${XDG_DATA_HOME:-$HOME/.local/share}/herdr-mobile-relay"/*)
                    rm -f "$legacy_shim"
                    ;;
            esac
        fi
    done

    "$final_dir/$release_binary" activate-release "$release_root" "$final_dir" ||
        fatal "could not atomically activate the complete release"

    info "Installed ${BINARY} ${version} to $final_dir"
    info "Active release: $release_root/current"
    case ":$PATH:" in
        *":$shim_dir:"*) ;;
        *) printf 'Add %s to PATH.\n' "$shim_dir" >&2 ;;
    esac
}

main "$@"
