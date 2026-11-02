#!/usr/bin/env bash
# Builds the macOS preview bundle. Used by CI and by hand; no signing identity
# is required beyond the ad-hoc signature macOS needs for a local bundle.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
RUST_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
PROJECT_DIR="$(cd -- "$RUST_DIR/.." && pwd)"
APP="$PROJECT_DIR/build/Oil Find Rust.app"
OUTPUT="$PROJECT_DIR/outputs/OilFind-Rust-macOS-preview.zip"

cd "$RUST_DIR"
TARGET_DIR="${CARGO_TARGET_DIR:-$RUST_DIR/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$RUST_DIR/$TARGET_DIR" ;; esac
HOST_TARGET="$(rustc -vV | grep '^host: ' | cut -d ' ' -f 2)"
cargo build --release --target "$HOST_TARGET" --target-dir "$TARGET_DIR" -p oilfind-mac

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$RUST_DIR/crates/oilfind-mac/Info.plist" "$APP/Contents/Info.plist"
cp "$TARGET_DIR/$HOST_TARGET/release/oilfind-mac" "$APP/Contents/MacOS/oilfind-mac"
codesign --force --sign - "$APP"

mkdir -p "$(dirname "$OUTPUT")"
rm -f "$OUTPUT"
ditto -c -k --keepParent "$APP" "$OUTPUT"
printf 'macOS preview: %s\n' "$OUTPUT"
