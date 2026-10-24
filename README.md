# Oil Find for Windows · 本机文件搜索

Windows 10/11 x64 的本机文件名搜索工具：显式选择目录后建立本地元数据索引，按中文、拼音或语法筛选即时查找，双击打开、定位或复制路径。

- **纯离线**：无网络请求、无遥测、无账号、无在线更新。
- **只读**：只读取文件名、路径、大小、修改时间，不读文件正文，不删除/移动/重命名文件。
- **不含闭源模块**：仅使用上游公开的 MIT 代码与自研实现，不包含任何 Pro 或私有二进制。
- **无管理员权限要求**：以当前用户身份运行（manifest `asInvoker`）。

> 实现语言：**Rust**（1.92，原生 Win32，无 GC）。仓库的 `main` 分支保留同功能的 Go 实现（标签 `v0.1-go`），用于对照与回退。

## 功能

- 文件名与路径搜索，支持中文、全拼、首字母。
- 语法：空格 AND、`|` OR、`!` 排除、`"带 空格"`、`*.pdf` 通配符、`regex:`。
- 筛选：`ext:pdf;docx`、`kind:image|doc|video|audio|code|archive|app|folder`、`file:`/`folder:`、`size:>10mb`、`dm:today`、`path:设计`。
- 结果列表：文件名 / 完整路径 / 大小 / 修改日期，最多显示 2000 条并显示总命中。
- 索引范围由用户显式添加；删除范围只改配置，不动任何文件。

## 安全与边界

- 默认不扫描任何目录，必须手动选择并点击“开始索引”。
- 跳过依赖与版本库目录（`node_modules`、`.git` 等）和私人运行器目录（`.claude`、`.codex`、`.workbuddy` 等），任意层级生效。
- `C:\Windows`、`C:\Program Files`、`C:\ProgramData`、`$RECYCLE.BIN` 等系统目录只在磁盘根一级跳过；`D:\code\myproject\windows` 这类普通项目目录仍可搜索。
- 拒绝 UNC、设备路径与网络映射盘；跳过符号链接与目录联接。
- 索引与配置存放在 `%LOCALAPPDATA%\OilFind`（不使用漫游目录）。
- 不支持：SMB/NAS 共享盘检索、多人服务、文件正文检索、删除操作。

## 构建

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

产物在 `outputs/windows-amd64/`：`OilFind.exe`、`oilfind-cli.exe`、`LICENSE`、`THIRD_PARTY_NOTICES.txt`、`USAGE.txt`。

## 测试与校验

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
| lint-and-test | ubuntu-latest | `cargo fmt --check`、`clippy -D warnings`、`cargo test --workspace` |
| windows | windows-latest | 打包 `OilFind.exe`、PE/内嵌 manifest 校验、上传未签名产物 |
| macos | macos-latest | 构建并打包 `Oil Find Rust.app`、上传未签名预览包 |

`Sources/` 里的 Swift macOS 基线不在 CI 中运行，仍由本机验收。

## 目录

| 路径 | 内容 |
|---|---|
| `rust/crates/oilfind-core` | 扫描、索引、查询、缓存（平台无关） |
| `rust/crates/oilfind-win` | Win32 原生界面 |
| `rust/crates/oilfind-cli` | 命令行诊断入口 |
| `rust/scripts/` | 构建、manifest 嵌入、PE 校验 |
| `rust/USAGE.txt` | 终端用户说明 |
| `Sources/`、`Package.swift` | macOS 基线源码（本机验收） |
| `site/` | 上游官网参考代码（未启用、未部署） |

## 上游来源与许可

Windows 客户端基于 [oil-oil/oil-find](https://github.com/oil-oil/oil-find) 公开部分的产品边界与搜索语法重新实现，不包含上游闭源 Pro 模块；macOS 源码作为公开基线保留，已固定为纯开源构建并禁用上游在线更新。

代码采用 MIT 许可证，见 `LICENSE`；第三方组件声明见 `rust/THIRD_PARTY_NOTICES.txt`。

## 已知限制

- 索引同步为用户启动后每 30 秒全量快照，不是 NTFS USN 实时索引。
- 索引常驻内存，上限 200 万条；大目录首次索引较慢。
- 结果最多显示 2000 条。
- 可执行文件未签名，Windows SmartScreen 或安全软件可能提示未知发布者。
- 界面交互、DPI/多显示器、ACL 拒绝等仍需在 Windows 真机验收。
