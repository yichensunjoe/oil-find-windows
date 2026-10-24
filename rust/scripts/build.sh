#!/usr/bin/env bash
# 在 macOS/Linux 上交叉编译 Windows 产物。
#
# 真正的发布包由 Windows 宿主（或 CI 的 windows-latest）上的 build.ps1 生成：
# 这里除了 MSVC 链接器，还依赖 Windows 资源编译器把 app.manifest 嵌进 exe
# （build.rs 只在 Windows 宿主上执行 embed-resource），因此允许失败，但必须
# 明确告诉调用方缺什么。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_DIR="$(cd "$RUST_DIR/.." && pwd)"
TARGET="x86_64-pc-windows-msvc"
OUTPUT_DIR="$PROJECT_DIR/outputs/windows-amd64"
RELEASE_DIR="$RUST_DIR/target/$TARGET/release"

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

cd "$RUST_DIR"

# 非 Windows 上 oilfind-win 只编译 stub main（Win32 代码仍参与类型检查），
# 因此工作区测试可以在本机跑完；真正的界面行为仍需 Windows 真机验收。
cargo test --workspace

if ! cargo build --release --target "$TARGET" -p oilfind-win -p oilfind-cli; then
    echo "" >&2
    echo "交叉编译失败：需要 Windows 工具链。" >&2
    echo "  - 链接 x86_64-pc-windows-msvc 需要 MSVC 链接器（如 cargo-xwin / xwin 提供的 lld-link）；" >&2
    echo "  - 嵌入 app.manifest 需要 Windows 资源编译器，仅在 Windows 宿主上由 build.rs 执行。" >&2
    echo "请在 Windows 上运行 rust/scripts/build.ps1 生成正式产物。" >&2
    exit 1
fi

mkdir -p "$OUTPUT_DIR"
cp "$RELEASE_DIR/oilfind-win.exe" "$OUTPUT_DIR/OilFind.exe"
cp "$RELEASE_DIR/oilfind-cli.exe" "$OUTPUT_DIR/oilfind-cli.exe"
cp "$PROJECT_DIR/LICENSE" "$OUTPUT_DIR/LICENSE"
cp "$RUST_DIR/THIRD_PARTY_NOTICES.txt" "$OUTPUT_DIR/THIRD_PARTY_NOTICES.txt"
cp "$RUST_DIR/USAGE.txt" "$OUTPUT_DIR/USAGE.txt"
rm -rf "$OUTPUT_DIR/LICENSES"
cp -R "$RUST_DIR/LICENSES" "$OUTPUT_DIR/LICENSES"

PYTHON="${PYTHON_BIN:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "未找到 $PYTHON：无法校验 PE 与嵌入清单。" >&2
    exit 1
fi

if ! "$PYTHON" "$SCRIPT_DIR/verify-pe.py" --arch amd64 "$OUTPUT_DIR/OilFind.exe"; then
    echo "" >&2
    echo "PE 校验失败：交叉编译产物通常缺少嵌入清单，因为 build.rs 只在 Windows 宿主上执行" >&2
    echo "embed-resource。请在 Windows 上用 build.ps1 打包，或改用 GitHub Actions。" >&2
    exit 1
fi

echo "交叉编译完成：$OUTPUT_DIR"
echo "注意：该产物未经 Windows 真机验收，正式发布请使用 build.ps1 的结果。"
