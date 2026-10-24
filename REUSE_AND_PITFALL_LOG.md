# 复用与踩坑日志

> 最新记录置顶，跨项目结论同步工作区根日志与索引。

## 2026-10-24 · 把 macOS 专属应用做成 Windows 本机版：平台重写与验证边界（P088-oil-find-intranet）

- 场景：上游是 Swift/AppKit + FSEvents + Apple NEON，公司却是 Windows。直接移植不可行，需要独立实现但保持同一产品边界（纯离线、无 Pro、无删除）。
- 结论：分平台入口（`Sources/` 保留 macOS、`windows/` 新写 Go + Win32），共享搜索语法而非共享代码；Windows 端用原生控件而非 WebView，避免浏览器内核依赖。
- 验证边界：开发机是 macOS，只能证明"跨编译 + 引擎测试 + PE 结构"；GUI 交互/DPI/ACL/SmartScreen 属真机验收，不能在报告里写成已运行。
- 踩坑 1：`go vet ./...` 在 Win32 回调上必然报 5 处 `possible misuse of unsafe.Pointer`（lParam、GlobalLock），`//go:nocheckptr` 不能抑制（实测）。按包拆 vet：index/cmd 全开，ui 仅 `-unsafeptr=false`，并人工审查指针生命周期，不要整体关 vet 或加 nolint。
- 踩坑 2：Windows `t.TempDir()` 落在 `%LOCALAPPDATA%\Temp`，会被"排除 AppData"的安全规则判为非法根，测试全红。正解是把 `TEMP/TMP` 指向项目内 `outputs/test-tmp`（脚本 finally 恢复），不要为测试放宽业务排除。
- 踩坑 3：缓存原子写若只在编码后检查取消，用户点"停止"仍会提交过期快照。需要 `SaveContext`：编码→Sync→Close 后再检查 ctx，再原子替换；保存失败时保留旧索引，只有明确的内存模式才允许替换。
- 踩坑 4：`json.Decode` 到切片是"先分配后校验"，超大声明条目可先吃 1GB 内存。改 `Decoder.Token` 流式逐条，超限立即拒绝；写出侧同时加体积上限，避免"能存不能读"。
- 验证：97 用例（含 race）、vet 分包、amd64/arm64 编译、`verify-pe.py` 校验 GUI 子系统与内嵌 manifest、临时样例 CLI 建索引/缓存重载/改名同步。
- 适用边界：桌面应用跨平台重写、Windows 离线工具、任何"回调指针 + 静态检查"冲突的 Go/Win32 项目。

## 2026-10-24 · 开源内网 fork 的闭源更新回流与测试隔离（P088-oil-find-intranet）

- 场景：公开 MIT 源码不含 Pro，但官方发布包/更新包包含 Pro；用户只要开源模块并计划公司内网改造。
- 结论：仅“不拉私有仓库”不能长期保证开源边界。删除 private target 自动发现与入口，正式版编译期关闭自动/手动更新、下载/安装及更新官网跳转；本分支打包仅归档自己的构建，不生成上游发布清单。许可证随应用资源分发。
- 验证：正式 release build 成功；Swift 隔离单测 30/30、公开边界 Python 2/2、脚本语法和 diff 检查通过。已有 Swift 6.4 weak capture 警告未改动。
- 测试隔离：CI 环境变量仅跳过部分真实数据测试；M6 部分测试仍写真实 Desktop，AI 设置测试可能读已有会话缓存。选择 M1/M3/M5 和新增 IntranetUpdateTests，仅临时样例或内存数据。
- 测试模式：上游 UI 测试调用 DEBUG-only helper，全量 test target 在 `-c release` 下不能编译（即使运行过滤只选少数）。正确分开 release build 与默认 debug test，不为测试强行把 DEBUG 写入正式构建。
- 平台边界：这是 macOS Apple 芯片本地文件名搜索客户端，官网是静态演示/营销/支付授权，不是共享盘搜索服务；网络卷不支持。
- 适用边界：开源核心 + 私有商业扩展项目的内网 fork、带真实 Home 副作用的原生应用测试。
