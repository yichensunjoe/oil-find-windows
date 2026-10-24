# HANDOFF — Oil Find for Windows

> 项目状态事实源；接力读取当前状态，轮次置顶保留历史。

## 当前状态（2026-10-24 14:00，分支 `rust`：Rust 重写进行中）

- **进度保留**：Go 实现完整留在 `main`，并打标签 `v0.1-go`（提交 `bbb4e43` 之后）。回退方式：`git checkout main`。
- **重写分支**：`rust`。已删除 `windows/`（Go），新增 `rust/` Cargo workspace：`crates/oilfind-core`（扫描/索引/查询/缓存）、`crates/oilfind-win`（Win32 UI）、`crates/oilfind-cli`；`Sources/`（macOS Swift）保留，仅本机验收。
- **工具链**：Rust 1.92 已固定于 `rust/rust-toolchain.toml`；本地已装 `x86_64-pc-windows-msvc` 目标用于类型检查（不链接）。
- **CI**：改为 Windows-only（fmt → clippy → test → 打包 → PE 校验 → 上传产物）；仓库已重新设为 **public** 以使用免费 Windows runner。
- 状态：核心引擎与 Win32 界面由两个 agent 并行实现中；尚未集成、未跑通 CI、未产出 Rust 二进制。
- 下一步：合并两份实现 → `cargo test/clippy/fmt` → `cargo check --target x86_64-pc-windows-msvc` → 推送分支 → CI 绿 → 补文档与实测数据。

## 历史状态（2026-10-24 11:55，Go 版，已冻结在 `main`）

- 已完成（macOS 基线）：完整克隆 `oil-oil/oil-find` 1.7.1（`e63c7150`），保留历史与 MIT；固定纯公开 target，编译期禁用上游在线更新，离线打包不依赖 Pro/官网。
- 已完成（Windows 首版）：产品名 Oil Find for Windows，`windows/` 独立客户端——Go 1.24+、原生 Win32、无 WebView/网络调用；显式目录范围 + 元数据索引 + 中文/拼音搜索 + 语法筛选 + 打开/定位/复制；缓存/配置在 `%LOCALAPPDATA%\OilFind`。
- 已完成（独立代码审查 + 修复）：另一 agent 只读审查后修复 10 项：长路径误判为链接、系统目录黑名单过宽（现仅磁盘根一级）、Win32 调用指针 GC 生命周期、UI 线程阻塞发送、扫描失败无退避、`[` 被误当通配符、kind:doc 缺 WPS/OFD、缓存目录落 Roaming、verify-pe 节区映射、arm64 缺 manifest。
- 已验证（本地）：`go test ./...` 97 用例全通过（含 `-race`）；`vet`（index/cmd 全开；ui 仅关 unsafeptr，5 处 Win32 回调指针已人工审查）；amd64 与 arm64 打包 + `verify-pe.py`；CLI 冒烟覆盖字面 `[`、嵌套 `windows`/`tmp`、WPS、空引号报错。
- 已验证（GitHub Actions，windows-latest）：run 40063211103 全绿——`internal/index` 与 `internal/ui` 测试**在真实 Windows 上执行通过**（含原生结构体布局/路径/格式化用例，此前只能编译），打包 + PE 校验 + 未签名产物上传（2.76MB）。
- 交付物：`outputs/windows-amd64/`、`outputs/windows-arm64/`；`OilFind-Windows-amd64-preview.zip` 3.3MB SHA256 `e41e35090fbc78c75e85243a0e8f7d1dd6997add7acbfbf5939a83258e549b7e`；`OilFind-Windows-arm64-preview.zip` 3.0MB SHA256 `ee98a63833bd92b6aa46001e47d71d27f28ba5a59024446c64ce1ca9a072b656`。
- macOS 侧：`swift build -c release` 与 30 项 Swift + 2 项 Python 隔离测试通过（上一轮）。
- 未验证：Windows 真机 GUI 交互、DPI/多显示器、ACL 拒绝、取消/重启、SmartScreen/杀软、OneDrive 占位文件；EXE 未代码签名；PowerShell 脚本未在本机执行（无 pwsh）。
- 已知限制：30 秒全量快照同步（非 USN 实时）；无共享盘/正文检索；无删除入口；无应用图标；索引常驻内存、上限 200 万条；结果最多显示 2000 条；`regex:` 依赖 RE2，无回溯爆炸但语法与 PCRE 不同。
- 版本与备份：**私有**仓库 `https://github.com/yichensunjoe/oil-find-windows`（origin，main，2026-10-24 由公开改为私有），CI 只跑 Windows；`upstream` 仅追踪来源、push URL 禁用。公开前已清理个人路径与工作区指向，未含密钥/构建产物。异机备份即 GitHub 远端。
- 下一步：在 Windows 10/11 上按 `windows/USAGE.txt` 实测验收 → 确认签名/分发方式 → 视需要加图标、NTFS USN 增量、独立 AppId。

## 近期轮次（最新在上，保留全部）

- 2026-10-24（第三轮）：派独立 agent 只读审查约 2200 行生产代码；无阻断缺陷，但确认 3 项 HIGH（长路径、黑名单过宽、GC 指针）与 7 项 MEDIUM/LOW，已全部修复并补回归；双架构重新打包与 PE 校验通过。真机项仍未验收。
- 2026-10-24（第二轮）：完成 Windows 本机文件搜索客户端与离线包；97 用例 + race + vet + PE 校验通过；缓存提交改为可取消（SaveContext），保存失败保留旧索引；引擎加固 Load 流式限额 / Save 体积上限 / CLI 取消不提交；README、AGENTS、需求/架构/任务、CI 同步双平台。未在 Windows 真机运行。
- 2026-10-24（第一轮）：完成公开源码克隆、纯开源构建/更新边界、macOS 离线打包与项目治理；release 编译及 30+2 项隔离测试通过。未安装/部署，未扫描真实文件，未获取 Pro。

## 历史状态（2026-10-24，macOS 基线）

- 已完成：完整克隆、保留 Git 历史与 MIT；移除私有 target 与入口；应用库固定离线构建并禁用上游更新；离线打包不依赖 Pro/官网；CI 只检查公开代码与隔离样例。
- 已验证：`swift build -c release`；Swift debug 白名单 30/30；`test_open_core.py` 2/2；`bash -n`、manifest 无外部依赖/Pro、`git diff --check`。
- 已知限制：上游部分全量测试访问真实个人目录；release 测试因上游 DEBUG-only helper 不能编译，故 release build 与 debug test 分开；存在上游 weak capture 警告。
- 未验证：macOS 应用打包/签名/安装/启动、真实文件扫描、完整测试集。
