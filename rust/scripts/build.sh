#!/usr/bin/env bash
# Cross-build the Windows executable on macOS or Linux.
#
# Native Windows builds use build.ps1. Cross-builds need an MSVC linker
# and a Windows resource compiler. Only a verified executable is published locally.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_DIR="$(cd "$RUST_DIR/.." && pwd)"
TARGET="x86_64-pc-windows-msvc"
OUTPUT_DIR="$PROJECT_DIR/outputs/windows-amd64"
TARGET_DIR="${CARGO_TARGET_DIR:-$RUST_DIR/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$RUST_DIR/$TARGET_DIR" ;; esac
RELEASE_DIR="$TARGET_DIR/$TARGET/release"

cd "$RUST_DIR"

for source in "$PROJECT_DIR/LICENSE" "$RUST_DIR/THIRD_PARTY_NOTICES.txt" "$RUST_DIR/USAGE.txt"; do
    if [ ! -f "$source" ]; then
        echo "缺少分发文件：$source" >&2
        exit 1
    fi
done

if ! command -v cargo >/dev/null 2>&1; then
    echo "未找到 cargo：请先安装 Rust 工具链。" >&2
    exit 1
fi

if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
    echo "未安装 Windows 目标：请先执行" >&2
    echo "    rustup target add $TARGET" >&2
    echo "交叉链接还需要 MSVC 链接器（Windows 宿主、llvm-mingw 或 cargo-xwin）。" >&2
    exit 1
fi

# 非 Windows 上 oilfind-win 只编译 stub main（Win32 代码仍参与类型检查），
# 因此工作区测试可以在本机跑完；真正的界面行为仍需 Windows 真机验收。
cargo test --workspace --target-dir "$TARGET_DIR"

if ! cargo build --release --target "$TARGET" --target-dir "$TARGET_DIR" -p oilfind-win; then
    echo "" >&2
    echo "交叉编译失败：需要 Windows 工具链。" >&2
    echo "  - 链接 x86_64-pc-windows-msvc 需要 MSVC 链接器（如 cargo-xwin / xwin 提供的 lld-link）；" >&2
    echo "  - 嵌入 app.manifest 需要 Windows 资源编译器。" >&2
    echo "请在 Windows 上运行 rust/scripts/build.ps1 生成正式产物。" >&2
    exit 1
fi

PYTHON="${PYTHON_BIN:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "未找到 $PYTHON：无法校验 PE 与嵌入清单。" >&2
    exit 1
fi

if ! "$PYTHON" "$SCRIPT_DIR/package-windows.py" "$RELEASE_DIR/oilfind-win.exe" "$OUTPUT_DIR"; then
    echo "打包失败。若产物缺少嵌入清单，请检查资源编译器或在 Windows 上运行 build.ps1。" >&2
    exit 1
fi

echo "交叉编译完成：$OUTPUT_DIR/OilFind.exe（单文件）"
echo "注意：该产物未经 Windows 真机验收，正式发布请使用 build.ps1 的结果。"
