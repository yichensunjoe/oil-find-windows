#!/bin/bash
set -euo pipefail
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$PROJECT_DIR/scripts/install.sh"
cd "$PROJECT_DIR"
CONFIGURATION=release
APP_PATH="$PROJECT_DIR/build/$APP_NAME.app"
usage() { printf '%s\n' 'Usage: scripts/build-app.sh [--debug]' >&2; exit 2; }
case "${1:-}" in
    '') [[ $# -eq 0 ]] || usage ;;
    --debug) [[ $# -eq 1 ]] || usage; CONFIGURATION=debug; APP_PATH="$PROJECT_DIR/build/debug/$APP_NAME.app" ;;
    *) usage ;;
esac
if [[ -e "$APP_PATH" || -L "$APP_PATH" ]]; then
    if ! is_managed_app "$APP_PATH"; then
        printf 'Refusing to replace an unrecognized build target: %s\n' "$APP_PATH" >&2
        exit 1
    fi
fi
swift build -c "$CONFIGURATION"
BINARY_DIR="$(swift build -c "$CONFIGURATION" --show-bin-path)"
if [[ -e "$APP_PATH" || -L "$APP_PATH" ]]; then
    if ! is_managed_app "$APP_PATH"; then
        printf 'Build target changed and is no longer a verified isolated application: %s\n' "$APP_PATH" >&2
        exit 1
    fi
    rm -rf -- "$APP_PATH"
fi
mkdir -p "$APP_PATH/Contents/MacOS" "$APP_PATH/Contents/Resources"
cp "$BINARY_DIR/OilFind" "$APP_PATH/Contents/MacOS/$APP_EXECUTABLE"
if [[ "$CONFIGURATION" == release ]]; then
    # Release bundles omit debug symbols containing local source paths.
    strip -S "$APP_PATH/Contents/MacOS/$APP_EXECUTABLE"
fi
cp "Resources/Info.plist" "$APP_PATH/Contents/Info.plist"
cp "LICENSE" "$APP_PATH/Contents/Resources/LICENSE"
if [[ -f "Resources/AppIcon.icns" ]]; then
    cp "Resources/AppIcon.icns" "$APP_PATH/Contents/Resources/AppIcon.icns"
fi
# Copy only resource bundles declared by the active package, including optional targets.
swift package describe --type json | python3 -c '
import json, pathlib, shutil, sys
package = json.load(sys.stdin)
binary, resources = map(pathlib.Path, sys.argv[1:])
for old in resources.glob(package["name"] + "_*.bundle"):
    shutil.rmtree(old)
for target in package["targets"]:
    if target.get("resources"):
        name = package["name"] + "_" + target["c99name"] + ".bundle"
        shutil.copytree(binary / name, resources / name)
' "$BINARY_DIR" "$APP_PATH/Contents/Resources"
if [[ -n "${OILFIND_SIGN_IDENTITY:-}" ]]; then
    codesign --force --sign "$OILFIND_SIGN_IDENTITY" --identifier "$BUNDLE_ID" --options runtime --timestamp=none "$APP_PATH"
else
    codesign --force --sign - --identifier "$BUNDLE_ID" "$APP_PATH"
fi
printf '%s\n' "$APP_PATH"
