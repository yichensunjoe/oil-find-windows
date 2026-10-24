# 复用与踩坑日志

> 最新记录置顶；本仓库独立维护，不依赖外部工作区。

## 2026-10-24 · 内存第三轮与一条测量纪律（Oil Find for Windows）

- 场景：第三轮把 `Entry` 的两个 `Arc<str>`（dir/lower_dir）合并为单一 `SharedDir`（原始/小写拼接在同一 `String`，一个 `Arc` 指向），条目内联 80B→72B。
- 收益超估算：原估算"省 12MB 稳态"（只算内联），实测加载峰值 272→239MB（−33MB）、稳态 261→228MiB、扫描峰值 360→341MiB。除内联外还有：加载侧共享目录表从两个 `Vec<Arc<str>>` 减半；排序比较少一次指针追逐且两种形式同处一个缓存行。
- 实现要点：DST 不能有两个不定长字段，`Arc<SharedDir>{text,split}` 用 Sized 结构 + 切片访问；`DirCache` 的 HashMap value 直接存 `Arc<SharedDir>`，命中时零分配。
- 教训（测量纪律）：全量扫描会**覆盖**测量用的缓存文件，且条目数随时间增长（一小时内的 cargo 构建新增了 3.4 万个 target 文件）。对比前后必须确认测的是同一个文件/同一次扫描；本次曾把"缓存被新扫描覆盖 + 数据增长"误判为解码计数随代码变化，靠 mtime + 时间线核对才排除。测量型结论要附上条目数。
- 评估后放弃：扫描侧按上次条目数预置 Vec 容量——只对 App 有益（CLI 冷启动无历史可依），且要改 `scan()` 核心 API，收益无法用现有验收口径证明。
- 验证：77 测试 + clippy `-D warnings` + fmt + msvc target check；旧缓存加载一致。
- 适用边界：任何"每条目重复持有同一引用数据"的内存优化；先算清收益构成（内联/共享表/局部性）再决定是否动热路径。

## 2026-10-24 · 原生 Win32 界面升级：windows-sys 的六个实测要点（Oil Find for Windows）

- 场景：给 `oilfind-win` 加表头排序、进度条、空状态、右键菜单、Ctrl+C、默认按钮；开发机是 macOS，只能 `cargo check --target x86_64-pc-windows-msvc` 类型检查，交互本身无法本地运行。
- 要点 1（坐标函数在 Gdi）：`ScreenToClient` / `ClientToScreen` 在 `Win32::Graphics::Gdi` 模块（user32 转发），不在 `WindowsAndMessaging`；按后者找会直接编译不过。
- 要点 2（表头通知靠转发）：表头点击（HDN_ITEMCLICK）由 SysHeader32 发给父窗口=列表视图，列表视图再转发给自己的父窗口；在窗口过程的 WM_NOTIFY 里用 `LVM_GETHEADER` 拿表头句柄比对 `hwndFrom` 即可。HDN_ITEMCLICK 是负数通知码（u32 4294966974 / i32 -300），`NMHEADERW.iItem` 是列号。
- 要点 3（结构体 Default 不统一）：`LVITEMW`/`LVCOLUMNW` 有 `Default`，`HDITEMW`/`LVHITTESTINFO`/`NMHEADERW` 没有，必须逐字段构造；windows-sys 0.5x 的 FLAGS 类型都是 `u32` 别名（MENU_ITEM_FLAGS、TRACK_POPUP_MENU_FLAGS、LVHITTESTINFO_FLAGS…），常量可直接用、可直接按位或。
- 要点 4（纯逻辑可跨平台单测）：`cargo test -p oilfind-win` 在 macOS 上能编译并运行（实测 9 项通过），排序比较器、千分位格式化这类纯函数放进 app.rs 的 tests 模块即可在开发机验证，不必等真机。
- 要点 5（clippy 1.92 与 MSRV 冲突）：`manual_is_multiple_of` 要求 `% 3 == 0` 改 `is_multiple_of(3)`，但后者 MSRV 1.87 > 工作区声明的 1.75；倒序计数（`since_separator == 3`）可绕开且更易读，不要为过 lint 抬 MSRV。
- 要点 6（marquee 进度条）：`ICC_PROGRESS_CLASS` 要加进 `InitCommonControlsEx`；建 `msctls_progress2` 时带 `PBS_MARQUEE`(0x08)，用 `PBM_SETMARQUEE`(WM_USER+10, wParam=开关, lParam=毫秒) 启停；平时 `ShowWindow(SW_HIDE)`，布局里固定在客户区底部细条，不扰动其他控件。
- 经验：`TrackPopupMenu` + `TPM_RETURNCMD` 同步返回菜单 ID，不用自定义消息回路；右键菜单先 `LVM_HITTEST` + `LVM_SETITEMSTATE` 选中命中项，与资源管理器行为一致。空状态提示用后创建的 STATIC 覆盖在列表上（隐藏时不吃鼠标），`WM_CTLCOLORSTATIC` 里按 `lParam==控件句柄` 只给自己改灰色文字，其余静态控件必须落到 DefWindowProc。
- 验证：`cargo test --workspace` 76 项 + clippy `-D warnings` + fmt + msvc target check；界面交互属真机验收项。
- 适用边界：任何用 windows-sys 写原生界面的 Rust 项目；尤其是"跨平台开发 + 目标平台才能跑"的桌面工具。

## 2026-10-24 · Rust 缓存加载器：一个"静默丢字段"的 bug 与两种内存测量口径（Oil Find for Windows）

- 场景：内存优化第二轮（`Entry` 改 `Arc<str>` 共享目录）stash 恢复后 7 个 cache_test 失败，交接记录怀疑是 TempFile 原子替换交互。实际根因：`Entry::with_dir(dir, lower_dir, path, ...)` 被传入**父目录**而非文件路径，`file_name()` 取出目录末段，`stored.name` 被完全丢弃——所有加载条目的名字都变成所在目录名。
- 教训 1（先看断言再找原因）：7 个失败断言的 left/right 全是"名字=目录名 vs 名字=文件名"，与原子替换无关。大型 debug dump 要先截取（`sed -n`）或对单条用例重跑，别被 300KB 输出淹没后凭印象下结论。
- 教训 2（构造器参数意义变了=全调用点审查）：把 `with_dir` 从收 `&Path` 改为收 `&str`（名字），scan.rs 与 cache.rs 两个调用点都必须显式传 `file_name()`/`stored.name`；这类重构先 `grep` 全部调用点，再用真实数据往返验证。
- 教训 3（峰值 ≠ 稳态）：`/usr/bin/time -l` 只给峰值 RSS。lower_name 零副本优化在峰值上看不出（解码瞬时内存主导），"后台跑 + `ps -o rss=`"才测到稳态 363→324MiB；A/B 对照要临时改回旧实现同机测量，不要信估算。
- 复用 1（bincode 流式加载）：手工 `Deserialize` + `visit_seq` 按位置读字段；条目经 `DeserializeSeed` 分 8192 条一批，边解码边 `par_iter().map().collect::<Result<Vec<_>,_>>()` 并行转换，loader 不再同时持有解码行与成品索引（省 ~113MB 峰值）。要点：`size_hint` 返回声明长度可预分配；bincode 的 `deserialize_struct` 就是定长位置 seq；声明数超限必须先于 `with_capacity` 拒绝。
- 复用 2（名字小写快路径）：`lowercases_to_itself()` 逐字符判断 `to_lowercase()` 是否恒等（ASCII 无大写、汉字无大小写都命中）；`Option<Box<str>>` 有空指针 niche 仍占 16B，命中即省一条堆字符串。实测 89.6% 条目命中。
- 验证：72 测试 + clippy `-D warnings` + fmt + msvc target check；旧缓存（重构前代码写入）加载结果一致；全量扫描 1,532,419 条后端到端查询正确。
- 适用边界：任何"解码→转换"两阶段且数据量大的加载路径；serde/bincode 位置格式的手动 Visitor 改造。

## 2026-10-24 · Go→Rust 重写：四个只在真实 Windows 上暴露的坑（Oil Find for Windows）

- 场景：把已跑通的 Go + Win32 客户端重写为 Rust（windows-sys 原生控件），开发机是 macOS，只能靠 `cargo check --target x86_64-pc-windows-msvc` 与 GitHub Actions 的 windows-latest 验证。
- 坑 1（**verbatim 路径**）：`std::fs::canonicalize` 在 Windows 返回 `\\?\C:\...`，被“拒绝 UNC/设备路径”的规则当成非法根，所有扫描测试失败。正解：校验前剥掉 `\\?\`（`\\?\UNC\` 还原成 `\\server\share` 再拒），并让测试夹具也返回去前缀路径，否则断言永远不相等。
- 坑 2（**测试临时目录**）：Windows 的 `%TEMP%` 在 AppData 下，会被“排除 AppData”策略拒绝。正解：集成测试用 `env!("CARGO_TARGET_TMPDIR")`（项目内 `target/tmp`），不要放宽业务排除。
- 坑 3（**JSON 里的 Windows 路径**）：手写 JSON 夹具直接拼 `C:\...`，反斜杠是非法转义，解析失败；Unix 上永远发现不了。正解：统一 `json_escape()`。
- 坑 4（**rc.exe 的路径转义**）：`embed_resource::compile` 接收的是 `.rc` 脚本而不是 manifest；且 rc 字符串里的 `\a` 会被当转义序列，`RC.EXE failed to compile`。正解：生成 `1 24 "<manifest>"` 的 .rc，并把路径里的 `\` 换成 `/`。
- 另：`HWND`（`*mut c_void`）不是 `Send`，后台线程只能捕获 `isize` 或专用 `SendHwnd` 包装，且只允许 `PostMessage`。
- 时区：`dm:today` 必须用本地时区（chrono），UTC 会在 UTC+8 的凌晨差一天。
- 验证：本地 60 项测试 + clippy `-D warnings` + fmt；CI（windows-latest）fmt/clippy/test/打包/PE 校验全绿。
- 适用边界：任何“在 A 平台交叉编译、到 B 平台运行”的 Rust 桌面工具；尤其是路径、临时目录、rc.exe 与线程 Send 边界。

## 2026-10-24 · 独立审查抓到的 4 类“编译通过但真机会出错”的缺陷（Oil Find for Windows）

- 场景：Windows 客户端在 macOS 上只能跨编译，无法运行；请独立 agent 只读审查 2200 行生产代码，结论“无阻断、有 3 项 HIGH”。
- 结论 1（长路径）：`syscall.GetFileAttributesW` **不做** `\\?\` 转换（`os.Lstat` 会做）。用它判断重解析点，≥260 字符的条目一律报错并被当作链接跳过，根目录含长路径祖先还会误报“不能索引符号链接”。正解：用 `os.Lstat` + `Win32FileAttributeData.FileAttributes&0x400`；`GetVolumePathNameW` 同理，长路径时改用盘符根判断。
- 结论 2（黑名单过宽）：按裸目录名在任意深度排除 `windows`/`tmp`/`program files`，会把 `D:\code\myproject\windows`、`D:\work\tmp` 整棵隐藏——文件搜索工具静默漏数据最伤信任。正解：系统目录只在磁盘根一级排除，依赖/私人运行器目录（node_modules、.git、.claude…）才任意层级排除；测试要同时断言“嵌套同名目录可搜”。
- 结论 3（GC 生命周期）：`uintptr(unsafe.Pointer(wide(s)))` 内联传参时，UTF-16 缓冲只以 uintptr 存在，GC 可在 syscall 读取前回收，表现为窗口创建失败/标题乱码，概率低但真实。正解：先存局部变量，调用后 `runtime.KeepAlive`；列表项 `LVITEM.pszText` 同样处理。
- 结论 4（校验脚本自身错）：PE 节区 RVA→文件偏移用 `max(VirtualSize, SizeOfRawData)` 会与下一节区间重叠，可能“校验通过”但读错数据。正解：用 `SizeOfRawData` 并校验不越过文件末尾。
- 其他修复：UI 线程阻塞发送（改丢弃+重试标记）、扫描失败仍每 30s 重扫（失败即停自动同步）、`[` 被当通配符导致 `会议[1].docx` 搜不到、kind:doc 缺 WPS/OFD、缓存写进漫游目录（改 `%LOCALAPPDATA%`）、arm64 缺 manifest（构建脚本按 ARCH 生成对应 syso）。
- 验证：97 用例 + race + vet 分包 + amd64/arm64 打包与 PE 校验 + CLI 冒烟（字面 `[`、嵌套 windows/tmp、WPS、空引号报错）。
- 适用边界：所有“在 A 平台交叉编译、到 B 平台运行”的桌面工具；尤其是长路径、GC+syscall、目录排除策略与自制二进制校验脚本。

## 2026-10-24 · 把 macOS 专属应用做成 Windows 本机版：平台重写与验证边界（Oil Find for Windows）

- 场景：上游是 Swift/AppKit + FSEvents + Apple NEON，目标平台却是 Windows。直接移植不可行，需要独立实现但保持同一产品边界（纯离线、无 Pro、无删除）。
- 结论：分平台入口（`Sources/` 保留 macOS、`windows/` 新写 Go + Win32），共享搜索语法而非共享代码；Windows 端用原生控件而非 WebView，避免浏览器内核依赖。
- 验证边界：开发机是 macOS，只能证明"跨编译 + 引擎测试 + PE 结构"；GUI 交互/DPI/ACL/SmartScreen 属真机验收，不能在报告里写成已运行。
- 踩坑 1：`go vet ./...` 在 Win32 回调上必然报 5 处 `possible misuse of unsafe.Pointer`（lParam、GlobalLock），`//go:nocheckptr` 不能抑制（实测）。按包拆 vet：index/cmd 全开，ui 仅 `-unsafeptr=false`，并人工审查指针生命周期，不要整体关 vet 或加 nolint。
- 踩坑 2：Windows `t.TempDir()` 落在 `%LOCALAPPDATA%\Temp`，会被"排除 AppData"的安全规则判为非法根，测试全红。正解是把 `TEMP/TMP` 指向项目内 `outputs/test-tmp`（脚本 finally 恢复），不要为测试放宽业务排除。
- 踩坑 3：缓存原子写若只在编码后检查取消，用户点"停止"仍会提交过期快照。需要 `SaveContext`：编码→Sync→Close 后再检查 ctx，再原子替换；保存失败时保留旧索引，只有明确的内存模式才允许替换。
- 踩坑 4：`json.Decode` 到切片是"先分配后校验"，超大声明条目可先吃 1GB 内存。改 `Decoder.Token` 流式逐条，超限立即拒绝；写出侧同时加体积上限，避免"能存不能读"。
- 验证：97 用例（含 race）、vet 分包、amd64/arm64 编译、`verify-pe.py` 校验 GUI 子系统与内嵌 manifest、临时样例 CLI 建索引/缓存重载/改名同步。
- 适用边界：桌面应用跨平台重写、Windows 离线工具、任何"回调指针 + 静态检查"冲突的 Go/Win32 项目。

## 2026-10-24 · 开源 fork 的闭源更新回流与测试隔离（Oil Find for Windows）

- 场景：公开 MIT 源码不含 Pro，但官方发布包/更新包包含 Pro；只使用公开开源模块。
- 结论：仅“不拉私有仓库”不能长期保证开源边界。删除 private target 自动发现与入口，正式版编译期关闭自动/手动更新、下载/安装及更新官网跳转；本分支打包仅归档自己的构建，不生成上游发布清单。许可证随应用资源分发。
- 验证：正式 release build 成功；Swift 隔离单测 30/30、公开边界 Python 2/2、脚本语法和 diff 检查通过。已有 Swift 6.4 weak capture 警告未改动。
- 测试隔离：CI 环境变量仅跳过部分真实数据测试；M6 部分测试仍写真实 Desktop，AI 设置测试可能读已有会话缓存。选择 M1/M3/M5 和新增 OfflineUpdateTests，仅临时样例或内存数据。
- 测试模式：上游 UI 测试调用 DEBUG-only helper，全量 test target 在 `-c release` 下不能编译（即使运行过滤只选少数）。正确分开 release build 与默认 debug test，不为测试强行把 DEBUG 写入正式构建。
- 平台边界：这是 macOS Apple 芯片本地文件名搜索客户端，官网是静态演示/营销/支付授权，不是共享盘搜索服务；网络卷不支持。
- 适用边界：开源核心 + 私有商业扩展项目的二次开发、带真实 Home 副作用的原生应用测试。
