# 打包操作

本文说明从源码生成发布 EXE 的操作。每次发版及版本号由用户指定。

## 准备条件

- 使用 Windows x64 和 PowerShell，在项目根目录执行命令。
- 项目内具备 Rust 1.92.0、MSVC、Windows SDK、Python，以及 Cargo.lock 对应的依赖缓存。工具链与缓存目录见 `scripts/env.ps1`。
- `lib/Scintilla.dll` 和 `lib/Lexilla.dll` 已准备；缺失时运行 `scripts/prepare-editor.ps1`，下载与解压均保存在项目内。
- 发版前将 `Cargo.toml` 和 `Cargo.lock` 中本项目版本设为用户指定值，并维护 `release-policy.json` 与 CHANGELOG 顶部当前开发版本的更新说明。实际发布前保留“未发布”标记，发布完成后固定该版本日志；后续仅新增并维护顶部“未发布”部分，用户明确授权同版本合并替换时，仅合并指定版本内容，原始包及说明独立归档，其他已发布版本不变。不要更改无关依赖的版本。

`scripts/package.ps1` 会自动加载 `scripts/env.ps1`，将缓存、编译输出和临时目录定向到项目内，不需要手动配置系统 PATH。

release-policy.json 显式指定 minimum_version、excluded_versions 和 remove。最低版本须为已发布版本且不高于目标；首次发布可等于目标，此时无可升级来源。排除项须位于升级范围内。CHANGELOG 仅用于确认发布基线，不自动选择上一版本。scripts/test-release-policy.py 在组包前检查规则。

## 完整打包

日常 Debug/测试构建使用精简调试信息并关闭增量编译，依赖不生成调试信息，以限制磁盘占用。需要完整变量调试时可在当前命令中覆盖 Cargo profile 设置；这会增加构建文件体积，不改变 Release 配置。固定工具链、目标和工作目录可复用编译依赖；Debug 与 Release、宿主构建工具与 Windows 目标产物仍有各自目录。清理编译缓存后下次需要重编译，不应同时删除测试数据、组件包或发布归档。

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

仅修正安装器界面且软件载荷未变化时，可更新安装器外壳并逐字节保留原压缩载荷、发布清单和同版本绑定，单独记录安装器构建标识。此操作不改变已安装软件的构建身份；必须验证原包可恢复、空路径目录选择及实际部署流程，再按用户授权替换入口。

开发验证不发版时，运行自动矩阵入口：

```powershell
. .\scripts\env.ps1
& .\.tools\python\python.exe .\scripts\acceptance-unified-deployment.py
```

该入口从当前 Release 构建在 testwork 下生成独立测试包，自动验证可用的代表性旧归档直升、新安装、同版本拒绝、文件保留、运行占用、多环境隔离及中断续修；不覆盖 dist，不启动组件服务。输出记录每个来源的状态；缺失原始归档明确列为未验证。数据库使用新建模拟文件，只验证升级器不改动它们，不据此声称数据库迁移兼容。

正式统一候选包可先用 scripts/acceptance-unified-deployment.py --release <项目内候选归档目录> 自动检查代表性来源及包内绑定的同版本基线。单来源可用 scripts/acceptance-release.py --release <项目内候选归档目录> --baseline <来源版本> --baseline-release <项目内来源归档目录> 验证。该脚本兼容统一 Setup 和已发布的分离包；--repair 可重复指定基线修复包，--rejected-baseline 用于验证兼容范围外来源。GUI、跨机器和真实故障须另行验证。

## 输出与命名

统一包的输出规则如下，正式版本仍须用户明确指定：

| 文件 | 用途 |
| --- | --- |
| dist/<版本>/Flaragon-Setup-<版本>-x64.exe | 新安装与升级共用的完整包 |
| dist/<版本>/payload/ | 程序、编辑器 DLL、许可及发布清单 |
| dist/<版本>/release.json | 产品身份、兼容范围和程序文件集合 |
| dist/<版本>/build-info.json | 构建来源、部署模式及签名状态 |

升级前在目标环境“停止并退出”，运行统一包，选择目录并点击“检查目录 → 开始升级”。兼容范围内允许跨版本直升；重复安装、降级、范围外来源和未知非空目录拒绝。未完成事务使用同一包续修。

0.0.5 正式入口只提供统一 Setup，用于新安装、范围内直升和原始 0.0.5 的绑定覆盖。原始 0.0.5 双包归档保存在 dist/archives/original-0.0.5；0.0.4 及更早归档保持原包内规则。普通打包不能覆盖现有发布目录，明确授权替换时先构建独立候选，验收通过后归档原目录并切换正式入口。

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

只有用户明确指定同版本重打包时才使用 -BuildId，标识须以 Cargo.toml 中的产品版本结尾。输出到 dist/rebuilds/<构建标识>/，目录已存在则拒绝覆盖；包含统一 Setup、载荷、清单及新的 release_id。

需要接受特定同版本安装时，额外用 -SameVersionBase 指定项目内原始归档。统一包携带 update-base.json，严格绑定原始清单、版本、构建身份及程序集合；配套依赖必须一致，普通同版本更新仍拒绝。同版本覆盖可使用目标包明确指定的最低版本与排除项，专用 Repair 包仍须保持原兼容范围；不能修改原始清单绕过绑定。

验收使用 acceptance-release.py 的 --release、--baseline 和 --baseline-release 指定候选和原始归档；同时验证重复应用、错误构建拒绝及中断续修。匹配源码已完成检查和 Release 构建时可加 -SkipBuild。替换任何正式入口仍需明确授权，原始归档独立保留。

安装和升级的产品操作见[安装与升级方案](deployment.zh-CN.md)，当前开发内容与已发布版本功能概述见 [CHANGELOG.md](../CHANGELOG.md)。
