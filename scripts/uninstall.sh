#!/bin/bash
set -euo pipefail

BUNDLE_ID='com.reasonix.oilfind.intranet'
APP_NAME='Oil Find Intranet'
APP_EXECUTABLE='OilFindIntranet'

is_managed_app() {
    local app="$1" plist="$1/Contents/Info.plist" executable="$1/Contents/MacOS/$APP_EXECUTABLE"
    [[ -d "$app" && ! -L "$app" && ! -L "$app/Contents" && -f "$plist" && ! -L "$plist" \
       && -d "$app/Contents/MacOS" && ! -L "$app/Contents/MacOS" && -x "$executable" && ! -L "$executable" ]] || return 1
    [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist" 2>/dev/null)" == "$BUNDLE_ID" \
       && "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist" 2>/dev/null)" == "$APP_EXECUTABLE" \
       && "$(/usr/libexec/PlistBuddy -c 'Print :CFBundlePackageType' "$plist" 2>/dev/null)" == 'APPL' \
       && "$(/usr/libexec/PlistBuddy -c 'Print :OilFindIntranetManaged' "$plist" 2>/dev/null)" == 'true' ]]
}

target_is_running() {
    local executable="$1/Contents/MacOS/$APP_EXECUTABLE" pids status pid process
    if pids="$(pgrep -x "$APP_EXECUTABLE")"; then
        while IFS= read -r pid; do
            [[ "$pid" =~ ^[0-9]+$ ]] || return 2
            if ! process="$(ps -p "$pid" -o command=)"; then return 2; fi
            process="${process#"${process%%[![:space:]]*}"}"
            if [[ "$process" == "$executable" || "$process" == "$executable "* ]]; then return 0; fi
        done <<< "$pids"
        return 1
    else
        status=$?
        [[ "$status" -eq 1 ]] && return 1
        return 2
    fi
}

uninstall_app() {
    local target="$1" confirmation status
    if [[ ! -e "$target" && ! -L "$target" ]]; then
        printf 'No isolated application exists at %s. No data was removed.\n' "$target"
        return 0
    fi
    if ! is_managed_app "$target"; then
        printf 'Refusing to remove an unrecognized application or path: %s\n' "$target" >&2
        return 1
    fi
    if target_is_running "$target"; then
        printf 'Quit %s before uninstalling it.\n' "$APP_NAME" >&2
        return 1
    else
        status=$?
        if [[ "$status" -ne 1 ]]; then
            printf 'Cannot determine whether %s is running; nothing was removed.\n' "$APP_NAME" >&2
            return 1
        fi
    fi
    printf 'Only this application will be removed: %s\n' "$target"
    printf '%s\n' 'All data, preferences and keychain items will be retained.'
    read -r -p 'Continue? [y/N] ' confirmation
    [[ "$confirmation" == y ]] || return 0
    if ! is_managed_app "$target"; then
        printf 'The target changed and is no longer a verified isolated application: %s\n' "$target" >&2
        return 1
    fi
    rm -rf -- "$target"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    if [[ $# -ne 0 ]]; then
        printf '%s\n' 'Usage: scripts/uninstall.sh (data and preferences are always retained; --purge is disabled)' >&2
        exit 2
    fi
    uninstall_app "/Applications/$APP_NAME.app"
fi
