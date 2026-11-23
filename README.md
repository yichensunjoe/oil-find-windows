# Oil Find for Windows · 本机文件搜索

Windows 10/11 x64 的本机文件搜索工具，支持中文文件名、拼音和可单独开启的正文搜索。

- **离线索引**：索引和查询不联网，无遥测、账号或在线更新；打开文件时由系统关联程序处理，外部程序可能联网。
- **只读源文件**。文件名索引只读文件名、路径、大小和修改时间。正文索引在用户选择并确认的目录中读取受支持的文件。CLI 手动指定 `-cache` 时会替换该路径的已有普通文件。
- **不含闭源模块**：仅使用上游公开的 MIT 代码与自研实现，不包含任何 Pro 或私有二进制。
- **无管理员权限要求**：以当前用户身份运行（manifest `asInvoker`）。

> 实现语言：**Rust**（1.92，原生 Win32，无 GC）。此前的 Go 实现保留在标签 `v0.1-go`，用于对照与回退。

## 用途

显式选择目录后，可按中文、拼音或语法筛选文件名与路径，双击打开文件。开启正文索引后，可以查找文档中的原词并定位到命中处。索引和查询全程离线，无需管理员权限。

## 当前状态

Rust 版是主线（Go 版保留在 `v0.1-go` 标签对照/回退），配置了三平台 CI；界面交互、DPI/多显示器与 ACL 拒绝场景仍待 Windows 真机验收，详见[已知限制](#已知限制)。

## 技术栈

- Rust 1.92（`rust/rust-toolchain.toml` 固定），核心引擎与平台层拆成 workspace crate。
- Windows 原生 Win32 界面（无 Electron/Tauri）；macOS 预览壳用 egui，复用同一核心。
- 元数据和正文分别建立索引，缓存保存在 `%LOCALAPPDATA%\OilFind`。
- 构建：Cargo + PowerShell/Bash 脚本（见[快速开始](#快速开始)）。

## 功能

- 文件名与路径搜索，支持中文、全拼、首字母。
- 语法：空格 AND、`|` OR、`!` 排除、`"带 空格"`、`*.pdf` 通配符、`regex:`。
- 筛选：`ext:pdf;docx`、`kind:image|doc|video|audio|code|archive|app|folder`、`file:`/`folder:`、`size:>10mb`、`dm:today`、`path:设计`。
- 结果列表：文件名 / 完整路径 / 大小 / 修改日期，最多显示 2000 条并显示总命中。
- 索引范围由用户显式添加；删除范围只改配置，不动任何文件。
- 正文搜索支持文字 PDF、Word `.docx`、`.txt` 和 Markdown。结果在文件名下显示高亮的正文片段，旁边有位置和次数。点击结果查看命中，用「上一处」「下一处」切换，返回时保留选中项和列表位置。
- PDF 原页在应用内离线渲染，能核对到原页字形时标出命中。读者也可以切换到只读文本预览。

## 安全与边界

- Windows 首次预选本机固定盘，由用户确认范围并点击“开始索引”。按 `Ctrl+Alt+Space` 唤出快捷搜索，范围和刷新间隔在“索引设置”里调整。
- 正文索引默认关闭。在「索引设置 → 正文索引」中添加文件夹，再点击「开始正文索引」。回到搜索窗后点「搜正文」。正文按原词连续匹配，默认手动刷新，也可在正文索引页单独设置刷新间隔。
- 刷新会复用核验后未变化的文档。通过「索引详情」可查看上次成功时间、复用数量和分类跳过数。
- Rust macOS 预览版由用户选择范围并点击“开始索引”。旧配置无法确认授权时会保留原文件，并要求重新选择范围。
- 跳过依赖与版本库目录（`node_modules`、`.git` 等）和私人运行器目录（`.claude`、`.codex`、`.workbuddy` 等），任意层级生效。
- `C:\Windows`、`C:\Program Files`、`C:\ProgramData`、`$RECYCLE.BIN` 等系统目录只在磁盘根一级跳过；`D:\code\myproject\windows` 这类普通项目目录仍可搜索。
- 拒绝 UNC、设备路径与网络映射盘；跳过符号链接与目录联接。
- 索引与配置存放在 `%LOCALAPPDATA%\OilFind`（不使用漫游目录）。
- 索引范围限本机。扫描版 PDF 需先有可提取的文字层。Word 命中定位到正文段落。

## 快速开始

需要 Rust 1.92（`rust/rust-toolchain.toml` 已固定）。

```powershell
# Windows 原生构建（amd64），含测试、打包与 PE 校验
powershell -ExecutionPolicy Bypass -File rust\scripts\build.ps1
```

```sh
# macOS/Linux 上交叉编译 Windows 版（需 rustup target add x86_64-pc-windows-msvc）
cd rust
bash scripts/build.sh

# macOS 预览版（egui 界面，复用同一核心）
bash scripts/package-mac.sh   # 产出 build/Oil Find Rust.app 与 outputs/OilFind-Rust-macOS-preview.zip
```

Windows 产物是单文件 `outputs/windows-amd64/OilFind.exe`。它静态链接 C 运行库，不依赖 VC++ 运行库或同目录 DLL。MIT 许可与第三方声明编入 exe，双击即可运行。构建脚本在 PE 校验通过后替换 EXE，并清理旧版打包生成的 CLI 和文档。

### macOS Swift 基线

`./scripts/build-app.sh` 仅构建 `build/Oil Find Intranet.app`，`./scripts/package.sh` 将其归档到 `build/Oil-Find-Intranet-<版本>.zip`。副本使用独立的 Bundle ID、偏好、数据目录和默认快捷键 `⌃⌥⇧F`，不迁移或清理原版数据。首次默认关闭外置卷索引，可先设置排除项再手动开启。安装和启动需单独授权并验收；安装脚本拒绝覆盖已有目标，暂不支持原地更新，卸载只移除已验证的隔离应用并保留数据和偏好。

Swift 基线保留上游 AI 话题功能，来源可能按本机安装情况默认启用并读取本机对话，详见 [AI 话题功能](docs/AI_TOPICS.md)。

## 常用命令

```sh
cd rust
cargo test --workspace                 # 核心引擎，仅临时样例
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo check -p oilfind-win --target x86_64-pc-windows-msvc   # Windows 分支类型检查（不链接）
```

CI 同时覆盖三个平台（`.github/workflows/ci.yml`）：

| 任务 | 运行器 | 内容 |
|---|---|---|
| lint-and-test | ubuntu-latest | `cargo fmt --check`、`clippy -D warnings`、`cargo test --workspace`、PE 与构建脚本回归 |
| windows | windows-latest | 打包 `OilFind.exe`、PE/内嵌 manifest 校验、PowerShell 打包回归、上传未签名产物 |
| macos | macos-latest | 构建并打包 `Oil Find Rust.app`、上传未签名预览包 |

`Sources/` 里的 Swift macOS 基线不在 CI 中运行，仍由本机验收。

## 项目结构

| 路径 | 内容 |
|---|---|
| `rust/crates/oilfind-core` | 扫描、索引、查询、缓存（平台无关） |
| `rust/crates/oilfind-win` | Win32 原生界面 |
| `rust/crates/oilfind-cli` | 命令行诊断入口 |
| `rust/scripts/` | 构建、manifest 嵌入、PE 校验 |
| `rust/USAGE.txt` | 终端用户说明 |
| `Sources/`、`Package.swift` | macOS 基线源码（本机验收） |
| `site/` | 上游官网参考代码（未启用、未部署） |

## 配置

- Windows 的索引范围和缓存默认保存在 `%LOCALAPPDATA%\OilFind`；排除目录由程序内置，列表排序不跨启动保存。
- 删除范围不删除用户文件；同步暂停，旧结果清空，剩余范围需要重新点击“开始索引”。
- CLI 的 `-cache` 可指定本机缓存文件，不接受网络路径、设备路径或链接；它会替换目标路径已有的普通文件，请勿指定个人文档。
- 无需额外配置环境变量或账号；manifest 为 `asInvoker`，不请求管理员权限。

## 上游来源与许可

Windows 客户端基于 [oil-oil/oil-find](https://github.com/oil-oil/oil-find) 公开部分的产品边界与搜索语法重新实现，不包含上游闭源 Pro 模块；macOS 源码作为公开基线保留，已固定为纯开源构建并禁用上游在线更新。

代码采用 MIT 许可证，见 `LICENSE`；第三方组件声明见 `rust/THIRD_PARTY_NOTICES.txt`。

## 已知限制

- Windows 索引按设定间隔重建全量快照，默认每小时一次。可选每 15 分钟至每天，也可使用仅手动模式。
- 索引常驻内存，上限 200 万条；大目录首次索引较慢。
- 结果最多显示 2000 条。
- 可执行文件未签名，Windows SmartScreen 或安全软件可能提示未知发布者。
- 界面交互、DPI/多显示器、ACL 拒绝等仍需在 Windows 真机验收。
- 路径逐级检查与随后打开/遍历之间仍有竞态，不应将其视为针对恶意本地进程的无竞态沙箱。
- Swift 隔离副本的实际安装和 GUI 尚未验收；旧的在线更新收据启动测试已停用。
