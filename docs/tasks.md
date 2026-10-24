# 任务

## 当前

### macOS 分支（已完成基线）

- [x] 完整克隆 MIT 公开源码和历史，建立 P088 独立仓库。
- [x] 固定公开 target，移除 Pro 入口组装。
- [x] 编译期禁用上游在线更新，设置中解释原因。
- [x] 改为本分支离线打包、携带许可证、不发布至上游。
- [x] 收口正式编译、隔离测试和交接。
- [x] 工作区检查结果补入交接：P088 无 error，全库 19 errors/63 warnings 为其他项目/既存治理项。

### Windows 分支（第二阶段，进行中）

- [x] 确认公司以 Windows 为主、首版只做员工本机搜索。
- [x] Go 索引/搜索/缓存实现：显式范围、跳过 reparse/系统/依赖/私人目录、拒绝网络路径。
- [x] 原生 Win32 界面：目录管理、去抖搜索、结果列表、打开/定位/复制、状态与进度。
- [x] 隔离测试（临时样例）与 CLI 端到端冒烟。
- [x] 便携 Go 工具链（官方 1.27.3，SHA256 核验）+ 项目内缓存。
- [x] 缓存提交语义（SaveContext 接入、保存失败保留旧索引）与 PerMonitorV2 DPI。
- [x] 引擎加固：Load 流式限额、Save 体积上限、CLI 取消不提交。
- [x] 构建脚本与 PE 校验、离线包产出（`outputs/windows-amd64`、`windows-arm64` + preview zip）。
- [x] 独立 agent 只读审查（约 2200 行生产代码）并按结论修复 3 项 HIGH + 7 项 MEDIUM/LOW。
- [ ] 提交本轮改动（尚未提交/推送）。

## 验证

- [x] `swift build -c release`。
- [x] `CI=true swift test --filter '^OilFindCoreTests\.(M1Tests|M3Tests|M5Tests)/|^OilFindUITests\.IntranetUpdateTests/'`：30/30。
- [x] `python3 Tests/Scripts/test_open_core.py`：2/2；构建/打包脚本语法检查通过。
- [x] Windows `go test ./...`（索引/搜索/缓存，仅临时样例）：97 用例通过，含 `-race`、`vet`。
- [x] Windows `GOOS=windows GOARCH=amd64/arm64 go build ./cmd/oilfind`（跨编译）。
- [x] Windows 离线包 + `verify-pe.py`：amd64 与 arm64 均通过（GUI 子系统、asInvoker、Common-Controls v6、PerMonitorV2）。
- [x] 独立审查后的回归：97 用例、`-race`、vet 分包、CLI 冒烟（字面 `[`、嵌套 windows/tmp、WPS、空引号）。
- [ ] 记录未验证项和原因；不运行真实扫描、安装或未隔离测试。

## 后续

- [ ] Windows 真机验收：目录选择、双击打开/定位/复制、不同 DPI 与多显示器、ACL 拒绝、取消与重启、SmartScreen/杀软。
- [ ] 公司管理员确认代码签名与离线分发策略后试点；未授权不得安装或部署。
- [ ] 视需要改独立应用名/AppId/缓存路径；评估是否加 NTFS USN 增量索引。
- [ ] 如需要 SMB/NAS 共享检索：单独设计服务端与权限模型，不复用本机索引假设。
- [ ] 用户决定是否配置公司私有远端及异机备份。
