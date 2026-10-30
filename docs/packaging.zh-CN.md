# 打包操作

本文说明从源码生成发布 EXE 的操作。每次发版及版本号由用户指定。

## 准备条件

- 使用 Windows x64 和 PowerShell，在项目根目录执行命令。
- 项目内具备 Rust 1.92.0、MSVC、Windows SDK、Python，以及 Cargo.lock 对应的依赖缓存。工具链与缓存目录见 `scripts/env.ps1`。
- `lib/Scintilla.dll` 和 `lib/Lexilla.dll` 已准备；缺失时运行 `scripts/prepare-editor.ps1`，下载与解压均保存在项目内。
- 发版前将 `Cargo.toml` 和 `Cargo.lock` 中本项目版本设为用户指定值，并维护 `release-policy.json` 与 CHANGELOG 顶部当前开发版本的更新说明。实际发布前保留“未发布”标记，发布完成后固定该版本日志；后续仅新增并维护顶部“未发布”部分，不修改任何已发布版本日志。不要更改无关依赖的版本。

`scripts/package.ps1` 会自动加载 `scripts/env.ps1`，将缓存、编译输出和临时目录定向到项目内，不需要手动配置系统 PATH。

从 0.0.4 起，发布脚本从 CHANGELOG 的正式版本标题（`# x.y.z`，不含“未发布”标记）选择小于目标版本的最大版本，写入清单 `minimum_version`；运行时将其作为唯一允许的跨版本来源。缺少上一正式版本时拒绝组包，不用版本号减一推算，也不依赖本机是否保留对应 `dist` 目录。`release-policy.json` 的 `minimum_version` 仅用于 0.0.3 及更早版本，`excluded_versions` 和 `remove` 继续生效。规则检查由 `scripts/test-release-policy.py` 执行，打包入口会先运行该检查。

## 完整打包

在项目根目录运行：

```powershell
.\scripts\package.ps1
```

脚本依次执行格式检查、Clippy、测试、Release 构建、第三方许可收集、程序自检及 EXE 组包。检查失败时停止打包；终端显示生成的 EXE 完整路径后，再检查输出文件。

只有当前源码、版本及依赖已完成全部检查与 Release 构建，且之后没有变化时，才可跳过重复构建：

```powershell
.\scripts\package.ps1 -SkipBuild
```

`-SkipBuild` 仍执行许可收集、自检和组包，不能用来跳过失败的检查或发布旧构建。

生成后，执行真实 EXE 安装与升级验收。0.0.4 使用以下命令：

```powershell
. .\scripts\env.ps1
& .\.tools\python\python.exe .\scripts\acceptance-release.py --baseline 0.0.3 --rejected-baseline 0.0.2 --rejected-baseline 0.0.1
```

该脚本仅在项目的 `testwork` 中新建独立安装目录，检查安装载荷、版本自检、原始及已修复安装升级、用户文件与修改时间保留、运行占用拒绝、多环境隔离及中断续修，不启动组件服务。结果按目标归档、基线归档及最后应用的补丁标识分别保存在 `testwork/evidence/release-*.json`，不替代 GUI、真实组件或跨机器验证。`--baseline` 指定已归档的低版本；`--repair` 可重复指定，按顺序应用对应基线修复包；`--baseline-release` 指定独立重打包归档。

从 0.0.4 起，跨版本验收的 `--baseline` 必须指定上一正式版本；同版本验收须另外通过 `--baseline-release` 明确指定原始归档，使用 `--rejected-baseline` 指定应拒绝的更旧版本；该参数可重复，验证检查和执行阶段均拒绝跳版且不替换文件。

## 输出与命名

输出目录为项目内的 `dist/<版本>/`。0.0.4 的产物如下：

| 文件 | 用途 |
| --- | --- |
| `dist/0.0.4/Flaragon-Setup-0.0.4-x64.exe` | 新环境安装包 |
| `dist/0.0.4/Flaragon-Update-0.0.4-x64.exe` | 0.0.3 升级及指定原始 0.0.4 构建覆盖升级 |
| `dist/0.0.4/payload/` | 组包载荷，含 Flaragon.exe、编辑器 DLL、许可和发布清单 |
| `dist/0.0.4/release.json` | 发布版本、架构、兼容范围及文件清单 |
| `dist/0.0.4/build-info.json` | 构建来源、包内容和签名状态说明 |

通用命名为 `Flaragon-Setup-<版本>-x64.exe` 和 `Flaragon-Update-<版本>-x64.exe`。对外提供安装 EXE 即可，不需要附带源码、Rust 或打包脚本。

0.0.1 归档仅包含 Setup，不提供该版本的 Update；从 0.0.1 升级时使用目标版本的 Update。

新环境使用 Setup 包；0.0.4 Update 接受 0.0.3 正式安装及明确绑定的原始 0.0.4 构建，0.0.1 和 0.0.2 安装须先使用归档的 0.0.3 Update 升级至 0.0.3。升级前在目标环境“停止并退出”，再运行 Update，选择安装目录，点击“检查目录 → 开始升级”。跳版、重复或不匹配的同版本构建、降级和无安装清单目录拒绝；实际验收范围以对应发布验证证据为准。

## 产物范围与检查

- 包仅包含管理器、Scintilla/Lexilla、第三方许可和发布清单，不包含 Apache、PHP、MySQL、Redis、本地配置、数据库或网站。
- `Cargo.toml`、EXE 产品版本、安装界面、包名和 `release.json` 必须一致。不能只重命名 EXE 来修改版本。
- 每份发布归档的载荷固定，内容变化时脚本拒绝覆盖；通常由用户指定新版本。用户明确要求同版本重打包时，使用独立构建标识另存，不能删除既有发布目录绕过检查。
- 构建文件位于 `target/x86_64-pc-windows-msvc/release/`。打包不覆盖项目根目录正在使用的 `Flaragon.exe`。
- `dist`、`target`、DLL、组件和缓存均由 `.gitignore` 排除，不加入 Git。
- 当前发布包未签名；Windows 10、跨电脑及不同 DPI 的待验证范围见 [PLAN](../PLAN.md)。

## 不发版的专用修复包

仅在用户明确要求后生成同版本修复包。脚本从 `Cargo.toml` 读取产品版本，默认绑定 `dist/<版本>/release.json` 中的原始安装清单与构建身份。已应用归档修复包的安装，通过 `-BaseRepairId` 指定该修复包标识，绑定其修复结果的清单与构建身份。修复标识必须以产品版本结尾，用于独立归档；重复修复、其他构建和未知目录均拒绝。

```powershell
.\scripts\package-repair.ps1 -RepairId redis-refresh-0.0.2 -Description 'Redis authentication and component settings refresh'
```

匹配源码已完成全部检查和 Release 构建时，可加 `-SkipBuild`。输出为 `dist/repairs/<修复标识>/update.exe`，同目录保存载荷、构建清单与来源。脚本使用所选基线归档的编辑器 DLL 和许可，只替换载荷主程序；已有修复包和正式发布归档不覆盖。

| 修复包 | 适用安装 | 内容 |
| --- | --- | --- |
| `dist/repairs/latest-dev-from-redis-refresh-0.0.2/update.exe` | 已应用 `redis-refresh-0.0.2` 的 0.0.2 安装 | PHP 8.4 以上 NTS 识别修正及对应归档构建的功能 |
| `dist/repairs/latest-dev-0.0.2/update.exe` | 原始 0.0.2 正式安装 | PHP 8.4 以上 NTS 识别修正；保留 Redis 认证与组件刷新功能 |
| `dist/repairs/redis-refresh-0.0.2/update.exe` | 原始 0.0.2 正式安装 | Redis 密码配置与认证启停、环境设置组件刷新 |
| `dist/repairs/php-version-0.0.1/update.exe` | 原始 0.0.1 正式安装 | PHP 完整版本识别，不包含 MySQL 数据接入 |

当前源码不能重新生成旧版本修复包；重新生成必须使用与其产品版本和载荷一致的源码及构建。

使用时先在目标软件中“停止并退出”，运行与当前构建匹配的 `update.exe`，选择安装目录，点击“检查目录”和“开始修复”。目标目录内的组件、配置、项目、数据库和会话文件保留。修复中断后重新运行同一文件完成续修。

针对已应用 `redis-refresh-0.0.2` 的安装生成及验证后续补丁：

```powershell
.\scripts\package-repair.ps1 -RepairId latest-dev-from-redis-refresh-0.0.2 -BaseRepairId redis-refresh-0.0.2 -Description 'Update the redis-refresh repaired build to the latest development program' -SkipBuild
. .\scripts\env.ps1
& .\.tools\python\python.exe .\scripts\acceptance-repair.py --repair .\dist\repairs\latest-dev-from-redis-refresh-0.0.2 --baseline-repair .\dist\repairs\redis-refresh-0.0.2
```

验收在项目内新安装原始版本、应用指定的前置补丁，再验证后续修复及中断续修。后续补丁仅接受该前置构建；不要通过删除或改写安装清单绕过身份检查，也不需要重新安装环境。

真实 EXE 验收命令：加载 `scripts/env.ps1` 后，运行 `.\.tools\python\python.exe .\scripts\acceptance-repair.py --repair .\dist\repairs\redis-refresh-0.0.2`；只操作项目内的新建安装夹具。其他修复包通过 `--repair` 指定对应目录。

## 用户指定的同版本重打包

用户授权替换 0.0.4 正式入口时，先保留原始归档 `dist/archives/original-0.0.4`，再生成独立候选包。以下命令适用于尚未存在的候选目录：

```powershell
.\scripts\package.ps1 -BuildId missing-components-0.0.4 -SameVersionBase .\dist\archives\original-0.0.4
. .\scripts\env.ps1
& .\.tools\python\python.exe .\scripts\acceptance-release.py --baseline 0.0.4 --baseline-release .\dist\archives\original-0.0.4 --release .\dist\rebuilds\missing-components-0.0.4
& .\.tools\python\python.exe .\scripts\acceptance-release.py --baseline 0.0.3 --release .\dist\rebuilds\missing-components-0.0.4 --rejected-baseline 0.0.2 --rejected-baseline 0.0.1
```

`-SameVersionBase` 仅配合独立构建标识使用；新 Update 携带原始完整清单作为 update-base.json，Setup 不携带此绑定。保持程序文件集合和配套依赖，使用新的 release_id。候选通过安装、两种升级来源、拒绝场景及中断续修验证后，才可按用户授权将候选内容复制到 `dist/0.0.4` 正式入口。原始归档保留，不删除归档以绕过内容保护；普通打包命令不会覆盖不同内容的既有归档。

未要求替换正式入口、未指定同版本绑定时，按以下独立重打包规则处理。

只有用户明确指定同版本重打包时才使用 `-BuildId`，标识须以产品版本结尾。以下命令示例对应独立归档；该目录已存在时不能重复生成或覆盖，应使用新的唯一构建标识。

```powershell
.\scripts\package.ps1 -BuildId latest-dev-0.0.2
```

输出目录为 `dist/rebuilds/<构建标识>/`，包含同版本 Setup、Update、载荷与清单，使用新的 `release_id`。原始 `dist/<版本>/` 及已有修复包保持不变。新安装使用此目录中的 Setup；已有较低版本使用 Update；已有相同版本仍须使用与当前构建匹配的专属补丁，普通 Update 不开放同版本更新。`latest-dev-0.0.2` 补丁针对原始 0.0.2；已应用 `redis-refresh-0.0.2` 的安装使用 `latest-dev-from-redis-refresh-0.0.2`。

匹配源码已完成全部检查与 Release 构建时可以加 `-SkipBuild`。验收通过 `--release` 指定独立归档，证据按构建标识分别保存：

```powershell
. .\scripts\env.ps1
& .\.tools\python\python.exe .\scripts\acceptance-release.py --baseline 0.0.1 --repair .\dist\repairs\php-version-0.0.1\update.exe --release .\dist\rebuilds\latest-dev-0.0.2
```

安装和升级的产品操作见[安装与升级方案](deployment.zh-CN.md)，当前开发内容与已发布版本功能概述见 [CHANGELOG.md](../CHANGELOG.md)。
