# AGENTS.md — P088-oil-find-intranet 项目级规则

本项目继承工作区根 `../../../AGENTS.md` 的共享安全、授权和现场保护底线；单独克隆时以本文件为准。

## 会话开始

1. 接力或实质修改读取 `HANDOFF.md` 当前状态及相关实现；简单问答不全读历史。
2. 经验按 `REUSE_AND_PITFALL_LOG.md` 的索引/主题读取，已读未变内容不重复加载。
3. 用户本轮要求建立开源改造底座，不代表已授权部署、推送、安装或扫描真实个人/公司文件。

## 项目约束

- 来源：`https://github.com/oil-oil/oil-find`，基线 `e63c7150ccbd401a1375c08215d1386b56dd9471`（1.7.1）。保留完整历史、MIT 许可证和原作者声明。
- 两个平台入口并存，共享同一公开功能边界：
  - macOS：`Sources/` + `Package.swift`，Swift 5 语言模式 / SwiftPM 5.10+、AppKit、SwiftUI、C；Apple 芯片，macOS 14+；无第三方 Swift 包。
  - Windows：`windows/`，Go 1.24+、原生 Win32（user32/comctl32/shell32）；唯一第三方库 `github.com/mozillazg/go-pinyin`（MIT）。仅 Windows 10/11 x64。
- 永久只构建公开模块；不要恢复私有 target 自动检测，不导入 Pro，也不下载或分发上游含 Pro 的安装包。
- `OILFIND_INTRANET_BUILD` 固定用于 macOS 应用库；在线更新检查、下载安装、更新官网跳转均禁用，管理员自行分发。Windows 版不实现任何在线更新/遥测。
- 保留中性 `ApplicationExtension`，不要把它误当 Pro 实现。上游官网 `site/` 仅作来源参考，不安装/部署其支付、授权或分析服务。
- macOS 热路径保持 Unsafe 指针 / SoA 设计；代码注释用英文；界面文案维护 `L10n.swift` 的中英文对应项。
- Windows 版硬边界：只读元数据（不读正文）；无删除/移动/重命名入口；默认不扫描任何目录，必须用户显式选择范围并点击开始索引；跳过 reparse/symlink、系统/AppData/依赖/私人运行器目录；拒绝 UNC、设备路径与网络映射盘；缓存/配置在 `%APPDATA%\OilFindIntranet`，缓存不得是链接。
- Windows 版首版只做本机搜索：不支持 SMB/NAS 共享盘、多人服务、正文检索。改变平台或共享索引架构先确认用户需求。
- 现有 Bundle ID、数据路径和全局快捷键仍与上游一致，安装前必须进行隔离改造；不得覆盖已有 Oil Find。

## 构建与必要验证

```sh
swift build -c release
CI=true swift test --filter '^OilFindCoreTests\.(M1Tests|M3Tests|M5Tests)/|^OilFindUITests\.IntranetUpdateTests/'
python3 Tests/Scripts/test_open_core.py
bash -n scripts/build-app.sh scripts/package.sh
```

工作区执行 Python 时使用已配置的托管 Python 绝对路径。
`CI=true` 不是安全沙箱；不直接运行全量测试。部分上游测试会读写真实 Desktop、访问本地 AI 会话缓存或挂载卷。白名单仅使用临时样例或内存数据；更大测试集先审查副作用。

```sh
scripts/build-app.sh          # builds build/Oil Find.app; does not install
scripts/build-app.sh --debug
scripts/package.sh            # archives this fork only; no upstream publication
```

构建签名可访问钥匙串，不能不经确认生成证书、安装应用或运行全盘扫描。官网不是应用构建依赖，无需安装 pnpm / Stripe 等依赖。

Windows（在 `windows/` 下；缓存必须用项目内绝对路径，避免污染用户环境）：

```sh
P=$(cd .. && pwd)
GOCACHE=$P/outputs/go-cache GOPATH=$P/outputs/go-path GOMODCACHE=$P/outputs/go-mod-cache \
  go test ./...
bash scripts/build.sh /path/to/go            # 交叉编译到 outputs/windows-amd64
python3 scripts/verify-pe.py ../outputs/windows-amd64/OilFindIntranet.exe --arch amd64
```

- Windows 上 `t.TempDir()` 默认落在 `%LOCALAPPDATA%\Temp`，会被引擎的 AppData 安全排除拒绝；构建脚本在跑测试时把 `TEMP/TMP` 指向 `outputs/test-tmp`，不要为此放宽引擎排除。
- manifest 由 `scripts/make-resource.py` 生成 `.syso` 后嵌入（`.syso` 已忽略），不要交付外置 manifest 冒充已嵌入。
- 不启动 GUI 做“验证”（开发机为 macOS 时无 Windows 运行环境）；Windows 真机 UI/DPI/权限/SmartScreen 属人工验收项，未验收不得声称可交付。
- `TestLoadRejectsStreamOverEntryLimit` 默认跳过（需 `OILFIND_HEAVY_TESTS=1`，约 950MB RSS）；它是唯一覆盖“流式解码在 200 万条处截断”的用例，改动 Load 限额逻辑时应手动跑一次。

## 复用与踩坑日志（双写）

- 项目内明细写入 `REUSE_AND_PITFALL_LOG.md`，最新记录置顶。
- 有跨项目价值的结论同步到工作区根 `../../../REUSE_AND_PITFALL_LOG.md` 并注明 P088；单独克隆时先保留项目内记录。
- 总库暂时不可写时标明待同步；不记录密钥、个人数据或无复用价值的一次性操作。

## HANDOFF 交接规则

1. `HANDOFF.md` 当前状态为项目事实源；近期轮次置顶保留历史，每轮不超过 8 行。
2. 有实质进展更新工作区根 `../../../HANDOFF.md` 中 P088 的一行动态，不覆盖其他项目。
3. 使用 `YYYY-MM-DD：完成了……；下一步……` 格式，只读任务不制造交接提交。

## Git 与备份

- 独立仓库，分支 `main`；`upstream` 保留来源，仅供拉取，推送地址已禁用。
- 本轮不创建公司远端、不推送、不发布。自有修改仅本机，尚未异机备份。
- 密钥、索引、依赖、构建产物、应用安装包、个人会话数据不提交。保留上游历史，不改写已发布提交。
