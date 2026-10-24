#!/bin/bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_DIR"

# Build and archive only this fork's public-source app. Never publish to the upstream website.
"$PROJECT_DIR/scripts/build-app.sh"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' Resources/Info.plist)"
ARCHIVE_PATH="$PROJECT_DIR/build/Oil-Find-$VERSION.zip"
ditto -c -k --sequesterRsrc --keepParent "build/Oil Find.app" "$ARCHIVE_PATH"

printf 'File: %s\n' "$ARCHIVE_PATH"
printf 'SHA-256: %s\n' "$(shasum -a 256 "$ARCHIVE_PATH" | cut -d ' ' -f 1)"
