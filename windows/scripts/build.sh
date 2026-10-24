#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MODULE_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
PROJECT_DIR="$(CDPATH= cd -- "$MODULE_DIR/.." && pwd)"
GO_BIN="${1:-${GO_BIN:-go}}"
ARCH="${ARCH:-amd64}"
if [[ "$ARCH" != "amd64" && "$ARCH" != "arm64" ]]; then
    printf 'ARCH must be amd64 or arm64, got: %s\n' "$ARCH" >&2
    exit 1
fi
OUTPUT_DIR="$PROJECT_DIR/outputs/windows-$ARCH"
TEST_TMP_DIR="$PROJECT_DIR/outputs/test-tmp"
MANIFEST_SYSO="$MODULE_DIR/cmd/oilfind/resource_windows_$ARCH.syso"
GUI_EXE="$OUTPUT_DIR/OilFindIntranet.exe"
CLI_EXE="$OUTPUT_DIR/oilfind-cli.exe"

if ! command -v "$GO_BIN" >/dev/null 2>&1; then
    printf 'Go executable not found: %s\nSet GO_BIN or pass the Go executable as the first argument.\n' "$GO_BIN" >&2
    exit 1
fi

if [[ ! -f "$PROJECT_DIR/LICENSE" || ! -f "$MODULE_DIR/THIRD_PARTY_NOTICES.txt" || ! -f "$MODULE_DIR/USAGE.txt" ]]; then
    printf 'Required LICENSE, THIRD_PARTY_NOTICES.txt or USAGE.txt is missing.\n' >&2
    exit 1
fi
PYTHON_BIN="${PYTHON_BIN:-python3}"
if ! command -v "$PYTHON_BIN" >/dev/null 2>&1; then
    printf 'Python is required only to embed and verify the Windows application manifest.\n' >&2
    exit 1
fi

mkdir -p "$OUTPUT_DIR" "$TEST_TMP_DIR" "$PROJECT_DIR/outputs/go-cache" "$PROJECT_DIR/outputs/go-mod-cache"
export GOCACHE="$PROJECT_DIR/outputs/go-cache"
export GOMODCACHE="$PROJECT_DIR/outputs/go-mod-cache"
export GOPATH="$PROJECT_DIR/outputs/go-path"
export CGO_ENABLED=0
export GOTOOLCHAIN=local

cd -- "$MODULE_DIR"
unset GOOS GOARCH

# The engine refuses roots under AppData on Windows, and the default
# %LOCALAPPDATA%\Temp would make every t.TempDir() fixture an illegal root.
# Tests therefore run in a subshell with a project-local TEMP/TMP so the
# caller's environment is restored when the subshell exits. The engine's
# exclusion rules are intentionally not relaxed for tests.
(
    export TEMP="$TEST_TMP_DIR"
    export TMP="$TEST_TMP_DIR"
    export TMPDIR="$TEST_TMP_DIR"
    "$GO_BIN" test ./...
)

# TestLoadRejectsStreamOverEntryLimit (MaxEntries+1 streamed entries, about
# 950MB RSS) is skipped by default so packaging stays light. Run it on purpose
# after touching the cache format; the manual CI job windows-heavy-cache-limit
# does the same:
#   OILFIND_HEAVY_TESTS=1 "$GO_BIN" test ./internal/index -run TestLoadRejectsStreamOverEntryLimit

# vet is split by package on purpose. The Win32 UI receives system pointers
# (WM_NOTIFY / WM_DPICHANGED / WM_GETMINMAXINFO lParam, GlobalLock results) that
# unsafeptr cannot model statically; those five conversions were reviewed by hand
# and stay valid only for the message / GlobalLock scope. Only that single
# analyzer is disabled for internal/ui; every other analyzer still runs there.
# Do not widen this to `vet ./...`, and do not add nolint/nocheckptr to sources.
"$GO_BIN" vet ./internal/index ./cmd/...
"$GO_BIN" vet -unsafeptr=false ./internal/ui

# The .syso is generated (and git-ignored) so the GUI binary carries the
# asInvoker/Common-Controls/PerMonitorV2 manifest without cgo or external tools.
"$PYTHON_BIN" "$SCRIPT_DIR/make-resource.py" --arch "$ARCH" --manifest "$MODULE_DIR/assets/app.manifest" --output "$MANIFEST_SYSO"

# GUI binary: Windows GUI subsystem. CLI binary: console subsystem, no -H.
GOOS=windows GOARCH="$ARCH" "$GO_BIN" build -trimpath -ldflags '-H windowsgui -s -w' -o "$GUI_EXE" ./cmd/oilfind
if [[ -d "$MODULE_DIR/cmd/oilfind-cli" ]]; then
    GOOS=windows GOARCH="$ARCH" "$GO_BIN" build -trimpath -ldflags '-s -w' -o "$CLI_EXE" ./cmd/oilfind-cli
fi

"$PYTHON_BIN" "$SCRIPT_DIR/verify-pe.py" --arch "$ARCH" "$GUI_EXE"

cp -- "$PROJECT_DIR/LICENSE" "$OUTPUT_DIR/LICENSE"
cp -- "$MODULE_DIR/THIRD_PARTY_NOTICES.txt" "$OUTPUT_DIR/THIRD_PARTY_NOTICES.txt"
cp -- "$MODULE_DIR/USAGE.txt" "$OUTPUT_DIR/USAGE.txt"

printf 'Local Windows build completed: %s\nNo installation, publication, or runtime dependency download was performed.\n' "$OUTPUT_DIR"
