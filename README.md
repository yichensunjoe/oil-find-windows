# P088-oil-find-intranet

> 私有仓库。CI（只跑 Windows）在 GitHub Actions 中运行，需登录仓库后查看。

## 用途

基于 [oil-oil/oil-find](https://github.com/oil-oil/oil-find) 公开源码建立的企业内部内网改造项目。仅使用 MIT 开源部分，不包含、不加载上游未开源 Pro 模块。

当前提供两个平台入口，共用同一公开功能边界（文件名搜索、纯本地、无 Pro）：

- `Sources/` + `Package.swift`：上游 macOS 原生客户端（保留，固定开源构建）。
- `windows/`：面向公司 Windows 员工的独立本机文件搜索客户端（Go + Win32 原生界面，离线、免安装）。

## 状态

2026-10-24：macOS 分支已固定开源构建并禁用上游在线更新；Windows 客户端首版（本机文件名/拼音搜索）源码与测试完成，CI 在 Windows runner 上通过（测试 + 打包 + PE 校验 + 产物上传），待 Windows 真机交互验收。详见 `HANDOFF.md`。

## 技术栈

- macOS：Apple 芯片，macOS 14+，Swift 5.10+（本机 Swift 6.4，Swift 5 语言模式），SwiftPM、AppKit、SwiftUI、C；无第三方 Swift 包。
- Windows：Windows 10/11 x64，Go 1.24+（构建用 1.27.3），原生 Win32（user32/comctl32/shell32），无第三方 GUI 框架；唯一第三方库是 MIT 的 `go-pinyin`（中文全拼/首字母）。
- `site/` 是上游 Next.js 营销官网和静态搜索演示，不是内网文件搜索服务，不需要安装其依赖。

## 快速开始

在本项目目录执行；不需要拉取 Pro 或安装 Node.js 网站依赖：

```sh
swift build -c release
CI=true swift test --filter '^OilFindCoreTests\.(M1Tests|M3Tests|M5Tests)/|^OilFindUITests\.IntranetUpdateTests/'
```

这两个命令不安装/启动应用，不扫描真实个人文件。应用打包可用 `scripts/build-app.sh`；正式安装前须先隔离应用标识与数据目录。

## Windows 客户端

面向公司 Windows 员工的本机文件搜索，默认不扫描任何目录：员工显式添加范围后点击“开始索引”。

能力与边界：

- 文件名/路径搜索，中文、全拼、首字母；空格 AND、`|` OR、`!` 排除、通配符、`regex:`。
- 筛选：`ext:`、`kind:`（doc/image/video/audio/code/archive/app/folder）、`file:`/`folder:`、`size:`、`dm:`、`path:`。
- 只读取文件名、路径、大小、修改时间，不读文件正文；不删除/移动/重命名文件。
- 跳过依赖/版本库与私人运行器目录（任意层级）；`C:\Windows`、`C:\Program Files` 等系统目录只在磁盘根一级跳过，`D:\code\myproject\windows` 这类普通目录仍可搜索；拒绝 UNC/网络映射盘；不支持共享盘与正文检索。
- 缓存与配置在 `%LOCALAPPDATA%\OilFindIntranet`（不用漫游目录，避免大索引被同步到域服务器），与上游 Mac 应用隔离；索引上限 200 万条，结果最多显示 2000 条并显示总命中。
- 首版为定期目录快照同步（用户启动索引后每 30 秒），不是 NTFS USN 实时索引；不承诺达到上游 SIMD 性能。

构建（macOS 或 Windows 均可，交叉编译到 Windows）：

```sh
# 在 windows/ 目录；GO_BIN 指向 Go 1.24+，PYTHON_BIN 仅用于生成/校验嵌入 manifest
bash scripts/build.sh /path/to/go              # amd64
ARCH=arm64 bash scripts/build.sh /path/to/go   # arm64（各自生成对应架构的 manifest）
```

Windows 原生构建：`powershell -ExecutionPolicy Bypass -File windows\scripts\build.ps1 -GoBin C:\Go\bin\go.exe`（amd64）。

产物在 `outputs/windows-amd64/`：`OilFindIntranet.exe`（GUI）、`oilfind-cli.exe`、`LICENSE`、`THIRD_PARTY_NOTICES.txt`、`USAGE.txt`。终端用户使用说明见 `windows/USAGE.txt`。

验证：

```sh
cd windows
go test ./...                      # 索引/搜索/缓存，仅临时样例
GOOS=windows GOARCH=amd64 CGO_ENABLED=0 go build -o /dev/null ./cmd/oilfind
python3 scripts/verify-pe.py ../outputs/windows-amd64/OilFindIntranet.exe --arch amd64
```

Windows 真机仍需验收：目录选择、列表双击/定位/剪贴板、不同 DPI 与多显示器、ACL 拒绝、取消与重启、SmartScreen/杀软与代码签名。当前 EXE 未签名。

CI 只跑 Windows（`.github/workflows/ci.yml`）：`windows-latest` 上执行测试、打包、PE 校验并上传未签名产物；`heavy-cache-limit` 为手动触发的重负载检查。macOS 侧验证需在本地按上面的 Swift 命令执行。

## 命令

| 目的 | 命令 |
|---|---|
| 编译 | `swift build -c release` |
| 隔离验证 | 上方 `CI=true swift test --filter ...` 白名单 |
| 开源边界 | `python3 Tests/Scripts/test_open_core.py` |
| 脚本语法 | `bash -n scripts/build-app.sh scripts/package.sh` |
| 调试应用包 | `scripts/build-app.sh --debug` |
| 正式应用包 | `scripts/build-app.sh` |
| 离线分发包 | `scripts/package.sh`，仅生成本分支 ZIP，不发布 |

不要直接运行全量 `swift test`：上游部分测试会读写真实 Desktop、读取用户 AI 会话缓存或进行挂载测试；`CI=true` 不能完全隔离这些副作用。

## 项目结构

- `Sources/COilFind/`：C 批量目录读取、名称比较、评分。
- `Sources/OilFindCore/`：扫描、索引、查询、FSEvents 增量同步与持久化。
- `Sources/OilFindApp/`：原生搜索 UI 与设置；本分支禁用在线更新。
- `Sources/OilFind/main.swift`：仅公开应用入口。
- `Sources/oilfind-cli/`：命令行工具；不要未经批准对真实文件树执行扫描。
- `Tests/`：上游测试及内网更新策略测试。
- `docs/`：需求、架构、任务和上游架构参考。
- `site/`：上游官网参考，含营销/支付/授权相关代码，不部署。
- `windows/`：Windows 客户端（`internal/index` 索引与查询、`internal/ui` 原生界面、`cmd/` 入口、`scripts/` 构建与 PE 校验、`USAGE.txt` 用户说明）。

## 配置

当前核心构建不需要 `.env` 或 API Key，`.env.example` 仅说明此事实。`Package.swift` 已去掉私有模块自动发现，不再依赖 `OILFIND_FREE=1` 才保证开源构建；应用库固定定义 `OILFIND_INTRANET_BUILD`。

自动/手动在线更新、更新包下载和安装及更新官网跳转已禁用，避免从官方更新通道引入含闭源 Pro 的安装包。核心更新工具类仍保留供参考，不代表提供了公司更新服务。

## 版本与备份

- 上游基线：`e63c7150ccbd401a1375c08215d1386b56dd9471`（1.7.1），完整 Git 历史保留。
- Git 远端：`upstream=https://github.com/oil-oil/oil-find.git`，仅用于追踪上游；推送地址禁用。尚未配置公司 `origin`。
- 自有修改：仅本机，尚未备份。上游远端不保存本分支修改，源码 ZIP 也不是异机备份。
- Git 外资产：尚无公司数据；构建产物可重建。恢复本分支需保留此本地仓库或未来配置的公司远端。

## 限制

- macOS 客户端不支持 Windows 或网络共享卷；Windows 客户端覆盖员工本机搜索，但**不支持 SMB/NAS 共享盘、多人浏览器搜索或文件正文检索**。如公司后续需要共享盘检索，需另行设计服务端与权限模型。
- 不提供 Pro 的图片/文档正文搜索和离线外接盘能力；要补类似功能应从公开技术自行实现，不提取私有模块。
- 两平台尚未安装/部署到公司环境，未接入公司数据；Windows 真机验收未完成，EXE 未签名。
- Bundle ID `com.oiloil.find`、应用名、数据目录及快捷键仍沿用上游；部署前必须改成公司独立标识，避免与原版冲突。AI 话题读取功能应按公司隐私要求决定是否关闭。
- 保留原 `LICENSE`（MIT）和版权声明；分发源码或应用副本时一并携带许可证。应用打包脚本已复制许可证到包内资源。

---

## 上游 README（来源参考；其中 Pro、下载和官网说明不适用于本分支）

<p align="center">
  <img src="./assets/readme/hero.en.svg" width="100%" alt="Oil Find: Everything, for the Mac. Press ⇧⌘F and results appear as you type.">
</p>

<p align="center">
  <a href="https://github.com/oil-oil/oil-find/releases/latest"><b>Download</b></a>
  &nbsp;·&nbsp;
  <a href="https://find.oiloil.org/en">Website</a>
  &nbsp;·&nbsp;
  <a href="./README.zh-CN.md">简体中文</a>
</p>

<p align="center">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="./assets/readme/search-en-dark.png">
  <img src="./assets/readme/search-en-light.png" width="100%" alt="The Oil Find search panel: readme is typed, eight results are ranked by relevance with the matching part in blue, and the footer shows the search took 0.2 milliseconds.">
</picture>
</p>

Oil Find is a file search tool for macOS that works like Everything on Windows: it keeps its own in-memory index of every file name on your disk instead of relying on Spotlight. Press ⇧⌘F, a search field appears in the middle of the screen, and results update with every keystroke. When files are created, renamed or deleted, the index follows along in real time.

File-name search, connected external drives, and local Claude Code, Codex, and WorkBuddy topic search are free and open source. All searches files and topics together; AI Topics shows conversations only. Searching inside images and documents, and unplugged drives, is part of the paid [Pro](#oil-find-pro) add-on.

## Why it's fast

- **File names live in one contiguous block of memory.** Hundreds of thousands of names are stored column by column and scanned front to back, with no per-file objects.
- **Matching runs in C with NEON.** Character comparisons use Apple silicon's vector instructions in batches.
- **Each keystroke narrows the last result.** Typing one more character only searches what already matched.
- **Only changes get updated.** FSEvents reports what changed and the index is updated incrementally. After a restart, Oil Find catches up from where it left off.

Measured on an Apple M5 with 24 GB of memory, using the command-line tool in this repository. Each query ran 20 times; the table shows the median.

| Index scope | Files | Indexing | Resident memory | `readme` | `wd` (pinyin) | `kind:image` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Default | 850K | 3.0 s | 45 MB | 0.6 ms | 0.3 ms | 3.9 ms |
| Whole disk | 7.5M | 27.2 s | 343 MB | 8.1 ms | 3.4 ms | 3.8 ms |

The default scope leaves out dependency folders, app and package internals, Library and system folders. You can turn each of them on in Settings. A single letter matches the most files and is the slowest case: about 11 ms in the default scope and about 58 ms for the whole disk.

## Search syntax

| Type | Meaning |
| --- | --- |
| `wd`, `wendang` | Pinyin initials or full pinyin, finds 文档 |
| `foo bar` | Contains both foo and bar |
| `foo \| bar` | Contains foo or bar |
| `readme !node_modules` | Contains readme, not inside node_modules |
| `*.png` | Wildcards match the whole name |
| `~/Desktop/ png` | Only under the Desktop |
| `ext:pdf;docx` | By extension |
| `kind:image` | By kind: folder, app, doc, image, video, audio, code, archive |
| `file:`, `folder:` | Files only, or folders only |
| `size:>10mb`, `size:1mb..5mb` | By size |
| `dm:today`, `dm:week`, `dm:2026-10-01` | By modification date |
| `regex:^IMG_\d+` | Regular expressions |

You can paste paths straight from a terminal or editor: `file://` URLs, quoted or escaped paths and paths ending in `:line:column` all work. Press ⌘/ in the search field for a syntax reference.

## Keyboard

| Key | Action |
| --- | --- |
| <kbd>⇧</kbd><kbd>⌘</kbd><kbd>F</kbd> | Open search (configurable in Settings) |
| <kbd>Return</kbd> | Open |
| <kbd>⌘</kbd><kbd>Return</kbd> | Show in Finder |
| <kbd>Space</kbd> / <kbd>⌘</kbd><kbd>Y</kbd> | Quick Look (Space after selecting a result) |
| <kbd>⌘</kbd><kbd>C</kbd> / <kbd>⌥</kbd><kbd>⌘</kbd><kbd>C</kbd> | Copy path, copy name |
| <kbd>⌘</kbd><kbd>⌫</kbd> | Move to Trash |
| <kbd>Tab</kbd> / <kbd>⌘</kbd><kbd>1</kbd>…<kbd>9</kbd> | Switch kind |
| <kbd>⌘</kbd><kbd>/</kbd> | Syntax reference |

You can also drag results into other apps.

## Oil Find Pro

The free version is open source and free forever. Pro is a closed-source paid add-on on top of it: pay once, with a 7-day free trial.

| | Free | Pro |
| --- | :---: | :---: |
| Search by file name: pinyin, wildcards, regex, filters by kind, size and date | ✓ | ✓ |
| Local AI topics: Claude Code, desktop Code sessions, Codex, and WorkBuddy | ✓ | ✓ |
| Live updates, Quick Look, drag and drop, full keyboard control | ✓ | ✓ |
| Excluded folders and file types, in-app updates | ✓ | ✓ |
| External drives: indexed when plugged in, kept up to date | ✓ | ✓ |
| External drives stay searchable when unplugged | | ✓ |
| Image content: text and objects in screenshots and photos | | ✓ |
| Document content: PDF, Word, PowerPoint, Excel, Pages, Markdown and more | | ✓ |
| Source code | Open source, MIT | Closed source |
| Price | Free | $9.99 once (¥49 in mainland China) |

Pro and the free version are the same download:

- After downloading from Releases or the website, every free feature works right away and never expires, with no activation.
- To use Pro, open the Oil Find Pro section in Settings and click Start 7-Day Free Trial, or enter a license key. When a trial ends, only Pro features turn off.
- Building from source gives you the free version, without Pro.
- Images and documents are processed on your Mac and never uploaded.

Buy it or read more at [find.oiloil.org/en/pro](https://find.oiloil.org/en/pro).

## Install

1. Download `Oil-Find.zip` from [Releases](https://github.com/oil-oil/oil-find/releases/latest), unzip it and drag Oil Find into Applications.
2. The first time you open it, macOS blocks it because it isn't notarized by Apple. Go to System Settings → Privacy & Security and click Open Anyway near the bottom.
3. Press ⇧⌘F and start typing.
4. Optional: grant Full Disk Access to also search mail attachments and other apps' data. Oil Find works fine without it.

When a new version is out, Oil Find tells you; click Update and Restart. You can turn off automatic checks in Settings.

The download includes Pro, but you can keep using Oil Find for free without activating it. See [Oil Find Pro](#oil-find-pro) above for the difference.

To uninstall, quit Oil Find and delete Oil Find from Applications and `~/Library/Application Support/Oil Find`.

## Build from source

Requires macOS 14 and Swift 5.10 or later (Xcode 15.3 or newer).

```sh
git clone https://github.com/oil-oil/oil-find.git
cd oil-find
swift test
scripts/build-app.sh   # builds build/Oil Find.app
scripts/install.sh     # builds and installs into Applications
```

Building from source gives you the free version, without Pro. Your own builds are ad-hoc signed, so you need to grant Full Disk Access again after each rebuild.

## Privacy

- File-name search reads names, sizes, and modification dates. Local AI topic search reads locally saved titles and conversation text from Claude Code, Codex, and WorkBuddy, without uploading them. Pro image or document search reads those files on your Mac and keeps the results there too.
- The index stays on your Mac in `~/Library/Application Support/Oil Find/`.
- A daily update check fetches one version file and sends no device or usage data. You can turn it off in Settings.
- With Pro, the license is verified online once a day. Only the license key, a device identifier and the device name are sent.

## Current limits

- Apple silicon and macOS 14 or later only.
- Network volumes aren't supported.
- After you unplug an external drive, the free version can't search its files; Pro can.
- AI topic search covers locally saved Claude Code, Claude desktop Code sessions, Codex, and WorkBuddy conversations. Regular Claude web chats, WorkBuddy cloud content not saved locally, and archived or deleted WorkBuddy sessions, temporary WorkBuddy Quick Asks, and WorkBuddy subagent sessions are excluded. Pro also searches text inside images and documents.

## Project layout

| Folder | Contents |
| --- | --- |
| `Sources/COilFind` | C: batched directory reads, name comparison, search scoring |
| `Sources/OilFindCore` | Scanning, indexing, queries, live updates, persistence, app updates |
| `Sources/OilFindApp` | The AppKit app and Settings |
| `Sources/OilFind` | The app's entry point |
| `Sources/oilfind-cli` | Command-line tool for benchmarks and troubleshooting |
| `site` | The website, [find.oiloil.org](https://find.oiloil.org/en) |

See [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) for the index design and [AGENTS.md](./AGENTS.md) for development conventions.

## Contributing

Issues and pull requests are welcome. The project does one thing, file search, so improvements to matching, ranking, indexing, performance and the interface are all in scope. Pro features (image and document content, searching unplugged drives) are maintained in a private repository, and pull requests that implement them won't be merged.

## License

The code in this repository is licensed under [MIT](./LICENSE). The Pro module isn't in this repository and isn't open source; the builds on Releases and the website include it.
