# HANDOFF — Oil Find for Windows

> 项目状态事实源；接力读取当前状态，轮次置顶保留历史。

## 当前状态（2026-10-24 21:40，分支 `rust`）

- **内存优化第二轮完成**：恢复 stash 后 7 个 cache_test 失败，根因是加载侧把父目录传给 `Entry::with_dir`，`stored.name` 被丢弃（加载条目名字全部变成目录末段）；`with_dir` 改为直接收名字，20/20 通过。叠加 lower_name 零副本快路径与二进制缓存分批流式转换，提交 `24a90aa`。
- **UI 体验升级完成**（提交 `64c928d`）：结果列表表头点击排序（▲▼ 指示，无箭头=相关度序）、扫描/加载缓存时底部 indeterminate 进度条、空状态引导文案（无索引/空索引/无匹配三种）、标题栏显示条目数（千分位）、Ctrl+C 复制路径、结果列表右键菜单（打开/定位/复制，右键先选中命中项）、“开始索引”设为默认按钮。USAGE.txt 已补“界面操作”说明。
- **实测**（$HOME + /Applications，1,532,419 条）：加载峰值 RSS 786MB → **272MB**；稳态 ~261MiB；扫描峰值 ~360MiB（此前 402MiB）；缓存 69.9MB；加载 ~0.6s、扫描 ~7.2s（热缓存）。
- **本地已验证**：`cargo test --workspace` 76 项通过（含 4 个新增排序/格式化单测）；clippy `-D warnings` 干净；fmt 干净；`cargo check -p oilfind-win --target x86_64-pc-windows-msvc` 通过；全量扫描 + 加载 + 查询端到端正确（报告 149 条 / 会议纪要 ext:docx 52 条）；旧缓存（stash 前代码写入）仍可正确加载。
- **CI 三平台全绿**（run 40099275430，覆盖内存与 UI 两轮改动）：ubuntu fmt/clippy/test、macOS 构建测试、Windows x64 构建测试打包 + PE/manifest 校验 + 未签名产物。
- **未验证**：Windows 真机 UI 交互（含本轮新增的排序/进度条/空状态/右键菜单/DPI）、目录选择对话框、定位/剪贴板、ACL 拒绝、SmartScreen；EXE 未签名。CI 只证明构建/测试/PE 结构。
- **仓库**：公开 https://github.com/yichensunjoe/oil-find-windows （默认分支仍是 `main`＝Go 版）。

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

- 2026-10-24（第五轮）：Win32 界面升级（提交 `64c928d`）：表头点击排序（▲▼，无箭头=相关度）、扫描/加载底部 marquee 进度条、空状态引导、标题条目数千分位、Ctrl+C 复制、结果列表右键菜单（命中项先选中）、“开始索引”默认按钮。76 测试 + clippy + fmt + msvc 检查全绿；真机交互未验收。
- 2026-10-24（第四轮）：恢复第二轮 stash 后定位 7 个 cache_test 失败根因——加载侧丢弃 `stored.name`（传给 `with_dir` 的是父目录而非名字），非 TempFile 交互；修复后叠加 lower_name 零副本快路径（89.6% 条目命中）与二进制缓存 8192 条/批流式转换。实测加载峰值 786MB → 272MB、稳态 ~261MiB、扫描峰值 ~360MiB；72 测试 + clippy + fmt + msvc 检查全绿，旧缓存兼容。提交 `24a90aa`，未推真机验收。
- 2026-10-24（第三轮）：派独立 agent 只读审查约 2200 行生产代码；无阻断缺陷，但确认 3 项 HIGH（长路径、黑名单过宽、GC 指针）与 7 项 MEDIUM/LOW，已全部修复并补回归；双架构重新打包与 PE 校验通过。真机项仍未验收。
- 2026-10-24（第二轮）：完成 Windows 本机文件搜索客户端与离线包；97 用例 + race + vet + PE 校验通过；缓存提交改为可取消（SaveContext），保存失败保留旧索引；引擎加固 Load 流式限额 / Save 体积上限 / CLI 取消不提交；README、AGENTS、需求/架构/任务、CI 同步双平台。未在 Windows 真机运行。
- 2026-10-24（第一轮）：完成公开源码克隆、纯开源构建/更新边界、macOS 离线打包与项目治理；release 编译及 30+2 项隔离测试通过。未安装/部署，未扫描真实文件，未获取 Pro。

## 历史状态（2026-10-24，macOS 基线）

- 已完成：完整克隆、保留 Git 历史与 MIT；移除私有 target 与入口；应用库固定离线构建并禁用上游更新；离线打包不依赖 Pro/官网；CI 只检查公开代码与隔离样例。
- 已验证：`swift build -c release`；Swift debug 白名单 30/30；`test_open_core.py` 2/2；`bash -n`、manifest 无外部依赖/Pro、`git diff --check`。
- 已知限制：上游部分全量测试访问真实个人目录；release 测试因上游 DEBUG-only helper 不能编译，故 release build 与 debug test 分开；存在上游 weak capture 警告。
- 未验证：macOS 应用打包/签名/安装/启动、真实文件扫描、完整测试集。

---

## 下一步（真机验收优先，内存第三轮可选）

**真机验收（最高优先级）**：在 Windows 10/11 上按 `rust/USAGE.txt` 实测：UI 交互（表头排序与箭头、底部进度条、空状态文案、标题计数、右键菜单、Ctrl+C、默认按钮回车）、DPI/多显示器、目录选择对话框、打开/定位/复制路径、ACL 拒绝目录、取消扫描、SmartScreen 行为。EXE 未签名，需确认签名/分发方式。通过后再把 `rust` 合入 `main` 并切换默认分支，最后补图标 / NTFS USN 增量。

**内存第三轮（可选，按预期收益排序）**：
1. 扫描侧 `entries` Vec 倍增扩容：1.53M 条目最终容量 2M × 80B ≈ 167MB，是扫描峰值（~360MiB）高于加载峰值（259MiB）的主因；可按上次条目数预置容量，或接受现状。
2. `dir`/`lower_dir` 两个 `Arc<str>`（16B/条目）合并为单个共享结构，约省 12MB 稳态；收益递减，谨慎评估。

**验收口径**（每次改完都跑）：`cargo test --workspace` + `clippy -D warnings` + `fmt --check`，然后 `/usr/bin/time -l ./target/release/oilfind-cli -cache <cache> -query 报告 -limit 3` 看 peak RSS，扫描用 `-scan -root "$HOME" -root /Applications`。新基线：1,532,419 条 / 7.2s / 69.9MB / 加载峰值 272MB、扫描峰值 ~360MiB、稳态 ~261MiB。
