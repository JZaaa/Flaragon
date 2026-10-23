# 打包操作

本文说明从源码生成发布 EXE 的操作。每次发版及版本号由用户指定，当前发布版本为 **0.0.1**。

## 准备条件

- 使用 Windows x64 和 PowerShell，在项目根目录执行命令。
- 项目内具备 Rust 1.92.0、MSVC、Windows SDK、Python，以及 Cargo.lock 对应的依赖缓存。工具链与缓存目录见 `scripts/env.ps1`。
- `lib/Scintilla.dll` 和 `lib/Lexilla.dll` 已准备；缺失时运行 `scripts/prepare-editor.ps1`，下载与解压均保存在项目内。
- 发版前将 `Cargo.toml` 和 `Cargo.lock` 中本项目版本设为用户指定值，并维护 `release-policy.json` 与本次更新说明。不要更改无关依赖的版本。

`scripts/package.ps1` 会自动加载 `scripts/env.ps1`，将缓存、编译输出和临时目录定向到项目内，不需要手动配置系统 PATH。

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

初始版本生成后，可执行真实 EXE 安装验收：

```powershell
. .\scripts\env.ps1
& .\.tools\python\python.exe .\scripts\acceptance-initial-release.py
```

该脚本仅在项目的 `testwork` 中新建独立安装目录，检查载荷、版本自检、多环境隔离及错误目标拒绝，不启动组件服务。结果保存在 `testwork/evidence/initial-release-<版本>.json`，不替代 GUI、真实组件或跨机器验证。

## 输出与命名

输出目录为项目内的 `dist/<版本>/`。0.0.1 的文件如下：

| 文件 | 用途 |
| --- | --- |
| `dist/0.0.1/Flaragon-Setup-0.0.1-x64.exe` | 初始版本安装包 |
| `dist/0.0.1/Flaragon-Update-0.0.1-x64.exe` | 同版本软件载荷的升级入口 |
| `dist/0.0.1/payload/` | 组包载荷，含 Flaragon.exe、编辑器 DLL、许可和发布清单 |
| `dist/0.0.1/release.json` | 发布版本、架构、兼容范围及文件清单 |
| `dist/0.0.1/build-info.json` | 构建来源、包内容和签名状态说明 |

通用命名为 `Flaragon-Setup-<版本>-x64.exe` 和 `Flaragon-Update-<版本>-x64.exe`。对外提供安装 EXE 即可，不需要附带源码、Rust 或打包脚本。

0.0.1 为初始版本，使用 Setup 包；当前没有可升级至 0.0.1 的更早正式版本。Update 包不用于首次安装，也不能将更高版本降级到 0.0.1。最低兼容版本设为 0.0.1，不据此宣称已验证跨版本升级。

## 产物范围与检查

- 包仅包含管理器、Scintilla/Lexilla、第三方许可和发布清单，不包含 Apache、PHP、MySQL、Redis、本地配置、数据库或网站。
- `Cargo.toml`、EXE 产品版本、安装界面、包名和 `release.json` 必须一致。不能只重命名 EXE 来修改版本。
- 同一发布版本的载荷固定，内容变化时脚本拒绝覆盖；需要由用户指定新版本，不能删除既有发布目录绕过检查。
- 构建文件位于 `target/x86_64-pc-windows-msvc/release/`。打包不覆盖项目根目录正在使用的 `Flaragon.exe`。
- `dist`、`target`、DLL、组件和缓存均由 `.gitignore` 排除，不加入 Git。
- 当前发布包未签名；Windows 10、跨电脑及不同 DPI 的完整验证范围见 [README](../README.md)。

安装和升级的产品操作见[安装与升级方案](deployment.zh-CN.md)，本次功能概述见 [CHANGELOG.md](../CHANGELOG.md)。
