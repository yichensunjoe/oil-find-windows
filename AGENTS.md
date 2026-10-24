# AGENTS.md — Oil Find for Windows 项目级规则

本文件是本仓库的项目级规则，独立生效。共享安全、授权与现场保护底线适用于所有改动。

## 会话开始

1. 接力或实质修改读取 `HANDOFF.md` 当前状态及相关实现；简单问答不全读历史。
2. 经验按 `REUSE_AND_PITFALL_LOG.md` 的索引/主题读取，已读未变内容不重复加载。
3. 用户本轮要求建立开源改造底座，不代表已授权部署、推送、安装或扫描真实用户文件。

## 项目约束

- 来源：`https://github.com/oil-oil/oil-find`，基线 `e63c7150ccbd401a1375c08215d1386b56dd9471`（1.7.1）。保留完整历史、MIT 许可证和原作者声明。
- 两个平台入口并存，共享同一公开功能边界：
  - macOS：`Sources/` + `Package.swift`，Swift 5 语言模式 / SwiftPM 5.10+、AppKit、SwiftUI、C；Apple 芯片，macOS 14+；无第三方 Swift 包。
  - Windows：`rust/`，Rust 1.92（`rust/rust-toolchain.toml` 固定）+ `windows-sys` 原生 Win32；第三方 crate 见 `rust/Cargo.toml`（pinyin/serde/serde_json/regex/walkdir/rayon，均 MIT/Apache）。仅 Windows 10/11 x64。`main` 分支的 Go 实现打标签 `v0.1-go` 保留，仅作对照与回退，不再演进。
- 永久只构建公开模块；不要恢复私有 target 自动检测，不导入 Pro，也不下载或分发上游含 Pro 的安装包。
- `OILFIND_OFFLINE_BUILD` 固定用于 macOS 应用库；在线更新检查、下载安装、更新官网跳转均禁用，由用户手动获取新版本。Windows 版不实现任何在线更新/遥测。
- 保留中性 `ApplicationExtension`，不要把它误当 Pro 实现。上游官网 `site/` 仅作来源参考，不安装/部署其支付、授权或分析服务。
- macOS 热路径保持 Unsafe 指针 / SoA 设计；代码注释用英文；界面文案维护 `L10n.swift` 的中英文对应项。
- Windows 版硬边界：只读元数据（不读正文）；无删除/移动/重命名入口；默认不扫描任何目录，必须用户显式选择范围并点击开始索引；跳过 reparse/symlink、系统/AppData/依赖/私人运行器目录；拒绝 UNC、设备路径与网络映射盘；缓存/配置在 `%LOCALAPPDATA%\OilFind`，缓存不得是链接。
- Windows 版首版只做本机搜索：不支持 SMB/NAS 共享盘、多人服务、正文检索。改变平台或共享索引架构先确认用户需求。
- 现有 Bundle ID、数据路径和全局快捷键仍与上游一致，安装前必须进行隔离改造；不得覆盖已有 Oil Find。

## 构建与必要验证

```sh
swift build -c release
CI=true swift test --filter '^OilFindCoreTests\.(M1Tests|M3Tests|M5Tests)/|^OilFindUITests\.OfflineUpdateTests/'
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

Windows（在 `rust/` 下）：

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p oilfind-win --target x86_64-pc-windows-msvc   # 只做类型检查，不链接
```

```powershell
powershell -ExecutionPolicy Bypass -File rust\scripts\build.ps1   # Windows 上：测试→打包→PE 校验
```

- 开发机是 macOS 时无法链接 Windows 产物；用 `--target x86_64-pc-windows-msvc` 做类型检查，真正的构建与 PE 校验交给 CI 或 Windows 机器。
- manifest 必须真的嵌入 exe（`rust/scripts/verify-pe.py` 会检查 GUI 子系统、`asInvoker`、Common-Controls v6、PerMonitorV2）；不要交付外置 manifest 冒充已嵌入。
- 不启动 GUI 做“验证”；Windows 真机 UI/DPI/权限/SmartScreen 属人工验收项，未验收不得声称可交付。
- 测试只允许在临时目录建样例，绝不访问真实用户目录；Windows 上临时目录若落在 AppData，引擎会按安全规则拒绝，构建脚本需把 TEMP/TMP 指到项目内目录，不要为此放宽排除规则。

## 复用与踩坑日志（双写）

- 项目内明细写入 `REUSE_AND_PITFALL_LOG.md`，最新记录置顶。
- 只在本仓库内记录；如项目后来并入其他工作区，再按那边的规则同步。
- 总库暂时不可写时标明待同步；不记录密钥、个人数据或无复用价值的一次性操作。

## HANDOFF 交接规则

1. `HANDOFF.md` 当前状态为项目事实源；近期轮次置顶保留历史，每轮不超过 8 行。
2. 只更新本仓库 `HANDOFF.md`，不涉及外部工作区。
3. 使用 `YYYY-MM-DD：完成了……；下一步……` 格式，只读任务不制造交接提交。

## Git 与备份

- 独立仓库，分支 `main`；`origin` 为私有仓库 `yichensunjoe/oil-find-windows`，`upstream` 仅追踪来源、推送地址已禁用。
- 推送前确认不含密钥、构建产物、个人路径或内部主机信息；仓库虽为私有，仍按“内容会被他人看到”的标准检查，改回公开需明确授权。
- 密钥、索引、依赖、构建产物、应用安装包、个人会话数据不提交。保留上游历史，不改写已发布提交。
