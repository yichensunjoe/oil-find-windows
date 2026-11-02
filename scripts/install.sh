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

destination_available() {
    local target="$1"
    if [[ -e "$target" || -L "$target" ]]; then
        if is_managed_app "$target"; then
            printf 'Refusing to overwrite %s. Uninstall the isolated copy first.\n' "$target" >&2
        else
            printf 'Refusing to overwrite an unrecognized application or path: %s\n' "$target" >&2
        fi
        return 1
    fi
}

install_app() {
    local source="$1" target="$2"
    if ! is_managed_app "$source"; then
        printf 'The built application is not a verified %s bundle: %s\n' "$APP_NAME" "$source" >&2
        return 1
    fi
    destination_available "$target" || return 1
    # Claim a new path atomically instead of merging into a pre-existing application.
    if ! mkdir "$target"; then
        printf 'Cannot create a new installation at %s; nothing was overwritten.\n' "$target" >&2
        return 1
    fi
    if ! ditto "$source/Contents" "$target/Contents"; then
        printf 'Copy failed. Inspect the incomplete isolated path before removing it: %s\n' "$target" >&2
        return 1
    fi
    printf 'Installed %s. The application was not started.\n' "$target"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    if [[ $# -ne 0 ]]; then
        printf '%s\n' 'Usage: scripts/install.sh' >&2
        exit 2
    fi
    if [[ -n "${OILFIND_SIGN_IDENTITY:-}" ]]; then
        printf '%s\n' 'The installer uses ad-hoc signing only. Unset OILFIND_SIGN_IDENTITY to proceed; explicit identities remain available for building packages.' >&2
        exit 2
    fi
    target="/Applications/$APP_NAME.app"
    destination_available "$target"
    PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    # Installation never selects a keychain identity inherited from the environment.
    ( unset OILFIND_SIGN_IDENTITY; "$PROJECT_DIR/scripts/build-app.sh" )
    install_app "$PROJECT_DIR/build/$APP_NAME.app" "$target"
fi
