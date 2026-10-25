# HANDOFF — Oil Find for Windows

> 项目状态事实源；接力读取当前状态，轮次置顶保留历史。

## 当前状态（2026-10-25 16:55，分支 `main`）

- **rust 已合入 main**（2026-10-25，快进 8e41983..55dd481，无合并提交）：默认分支 `origin/HEAD → main` 现承载 Rust 实现；Go 版由标签 `v0.1-go`（指向 8e41983）与完整历史保留，仅作对照回退。`rust` 分支暂留且与 main 同点，确认稳定后可删。main 上 CI run 40178286284 三平台全绿。

- **内存优化第二轮 + 第三轮完成**：修复 stash 恢复后 7 个 cache_test 失败（根因：加载侧把父目录传给 `Entry::with_dir`，`stored.name` 被丢弃，提交 `24a90aa`）；叠加 lower_name 零副本快路径（89.6% 命中）与二进制缓存 8192 条/批流式转换；第三轮把目录的原始/小写两种形式合并进单一 `SharedDir` 分配（条目 80B→72B，提交 `53c08ce`）。
- **UI 体验升级完成**（提交 `64c928d`）：结果列表表头点击排序（▲▼ 指示，无箭头=相关度序）、扫描/加载缓存时底部 indeterminate 进度条、空状态引导文案（无索引/空索引/无匹配三种）、标题栏显示条目数（千分位）、Ctrl+C 复制路径、结果列表右键菜单（打开/定位/复制，右键先选中命中项）、“开始索引”设为默认按钮。USAGE.txt 已补“界面操作”说明。另按用户截图反馈加宽“修改日期”列 165→175px（提交 `c285e38`，CI run 40176120608 全绿）：日期文本约 125px，原宽度余量过小；增量由弹性路径列吸收，表总宽不变。
- **实测**（$HOME + /Applications，1.57M 条）：加载峰值 RSS 786MB → **239MB**；稳态 ~228MiB；扫描峰值 ~341MiB（最初 402MiB）；缓存 ~70MB；加载 ~0.5s、扫描 ~7.9s（热缓存）。
- **本地已验证**：`cargo test --workspace` 77 项通过；clippy `-D warnings` 干净；fmt 干净；`cargo check -p oilfind-win --target x86_64-pc-windows-msvc` 通过；全量扫描 + 加载 + 查询端到端正确（报告 149 条 / 会议纪要 ext:docx 52 条）；旧缓存（多轮前代码写入）仍可正确加载。
- **CI 三平台全绿**（run 40099275430 覆盖内存与 UI 改动；run 40101140033 覆盖 SharedDir 第三轮）：ubuntu fmt/clippy/test、macOS 构建测试、Windows x64 构建测试打包 + PE/manifest 校验 + 未签名产物。
- **未验证**：Windows 真机 UI 交互（含排序/进度条/空状态/右键菜单/DPI）、目录选择对话框、定位/剪贴板、ACL 拒绝、SmartScreen；EXE 未签名。CI 只证明构建/测试/PE 结构。
- **仓库**：公开 https://github.com/yichensunjoe/oil-find-windows （默认分支 `main` 现为 Rust 实现；Go 版见 `v0.1-go` 标签）。

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

- 2026-10-25（第七轮）：按用户指令把 `rust` 快进合入 `main` 并推送（8e41983..55dd481）；默认分支现承载 Rust 实现，Go 版保留在 `v0.1-go` 标签与历史中；main CI run 40178286284 三平台全绿。真机验收未做即合入——验收结论出来前不应声称可交付。
- 2026-10-24（第六轮）：内存第三轮——`dir`/`lower_dir` 两个 `Arc<str>` 合并为单一 `SharedDir`（原始/小写拼接在同一分配，条目 80B→72B，加载侧共享目录表减半）。实测加载峰值 272→239MB、稳态 261→228MiB、扫描峰值 360→341MiB；77 测试 + clippy + fmt + msvc 全绿。提交 `53c08ce`。评估后放弃"扫描侧按上次条目数预置容量"（只对 App 有益且需改核心 API，CLI 无法体现）。
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

**真机验收（最高优先级，合入 main 后仍必须做）**：在 Windows 10/11 上按 `rust/USAGE.txt` 实测：UI 交互（表头排序与箭头、底部进度条、空状态文案、标题计数、右键菜单、Ctrl+C、默认按钮回车）、DPI/多显示器、目录选择对话框、打开/定位/复制路径、ACL 拒绝目录、取消扫描、SmartScreen 行为。EXE 未签名，需确认签名/分发方式。验收通过前不声称可交付；通过后补图标 / NTFS USN 增量，并删除已与 main 同点的 `rust` 分支。

**内存第三轮（已完成，提交 `53c08ce`）**：`dir`/`lower_dir` 合并为单一 `SharedDir` 分配，条目 80B→72B，实测加载峰值 272→239MB、稳态 261→228MiB、扫描峰值 360→341MiB。已放弃的方向：扫描侧按上次条目数预置容量（只对 App 有益、需改 `scan()` 核心 API，CLI 无法体现收益）。

**验收口径**（每次改完都跑）：`cargo test --workspace` + `clippy -D warnings` + `fmt --check`，然后 `/usr/bin/time -l ./target/release/oilfind-cli -cache <cache> -query 报告 -limit 3` 看 peak RSS，扫描用 `-scan -root "$HOME" -root /Applications`。最新基线（1,569,136 条）：加载 ~0.5s / 峰值 239MB / 稳态 ~228MiB；扫描 ~7.9s / 峰值 ~341MiB；缓存 ~70MB。
