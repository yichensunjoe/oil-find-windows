# Oil Find for Windows · 本机文件搜索

> 私有仓库。CI（只跑 Windows）在 GitHub Actions 中运行，需登录仓库后查看。

Windows 10/11 x64 的本机文件名搜索工具：显式选择目录后建立本地元数据索引，按中文、拼音或语法筛选即时查找，双击打开、定位或复制路径。

- **纯离线**：无网络请求、无遥测、无账号、无在线更新。
- **只读**：只读取文件名、路径、大小、修改时间，不读文件正文，不删除/移动/重命名文件。
- **不含闭源模块**：仅使用上游公开的 MIT 代码与自研 Windows 实现，不包含任何 Pro 或私有二进制。
- **无管理员权限要求**：以当前用户身份运行（manifest `asInvoker`）。

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

需要 Go 1.24+ 与 Python 3（仅用于生成并校验内嵌 manifest）。

```powershell
# Windows 原生构建（amd64）
powershell -ExecutionPolicy Bypass -File windows\scripts\build.ps1 -GoBin C:\Go\bin\go.exe
```

```sh
# 在 macOS/Linux 上交叉编译；ARCH 可选 amd64（默认）或 arm64
cd windows
bash scripts/build.sh /path/to/go
ARCH=arm64 bash scripts/build.sh /path/to/go
```

产物在 `outputs/windows-<arch>/`：`OilFind.exe`、`oilfind-cli.exe`、`LICENSE`、`THIRD_PARTY_NOTICES.txt`、`USAGE.txt`。

## 测试与校验

```sh
cd windows
go test ./...                                   # 索引/搜索/缓存，仅临时样例
GOOS=windows GOARCH=amd64 CGO_ENABLED=0 go build -o /dev/null ./cmd/oilfind
python3 scripts/verify-pe.py ../outputs/windows-amd64/OilFind.exe --arch amd64
```

CI 只跑 Windows（`.github/workflows/ci.yml`）：`windows-latest` 上执行测试、打包、PE 校验并上传未签名产物；`heavy-cache-limit` 为手动触发的重负载检查。开发机不是 Windows 时，CI 是唯一能真实执行 Windows 原生测试的通道。

## 目录

| 路径 | 内容 |
|---|---|
| `windows/internal/index` | 扫描、索引、查询、缓存 |
| `windows/internal/ui` | Win32 原生界面（user32/comctl32/shell32） |
| `windows/cmd/` | GUI 与 CLI 入口 |
| `windows/scripts/` | 构建、manifest 生成、PE 校验 |
| `windows/USAGE.txt` | 终端用户说明 |
| `Sources/`、`Package.swift` | 上游 macOS 基线源码（保留，见下） |
| `site/` | 上游官网参考代码（未启用、未部署） |

## 上游来源与许可

Windows 客户端是基于 [oil-oil/oil-find](https://github.com/oil-oil/oil-find) 公开部分的产品边界与搜索语法重新实现的独立平台版本，不包含上游闭源 Pro 模块。

仓库同时保留上游 macOS 源码（`Sources/`、`Package.swift`）作为公开基线，已固定为纯开源构建并禁用上游在线更新；它不是本项目的交付目标，构建方式见 `docs/ARCHITECTURE.md`。

代码采用 MIT 许可证，见 `LICENSE`；第三方组件声明见 `windows/THIRD_PARTY_NOTICES.txt`。

## 已知限制

- 索引同步为用户启动后每 30 秒全量快照，不是 NTFS USN 实时索引。
- 索引常驻内存，上限 200 万条；大目录首次索引较慢。
- 结果最多显示 2000 条。
- 可执行文件未签名，Windows SmartScreen 或安全软件可能提示未知发布者。
- 界面交互、DPI/多显示器、ACL 拒绝等仍需在 Windows 真机验收。
