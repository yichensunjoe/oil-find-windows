# 复用与踩坑日志

> 最新记录置顶；本仓库独立维护，不依赖外部工作区。

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
