# Flaragon

面向个人 PHP 开发的 Windows x64 环境管理器，管理 Apache、PHP、MySQL 和 Redis，支持多环境、项目配置、服务启停、日志和托盘操作。

使用 Rust + Slint 原生桌面界面，配置编辑器采用 Scintilla + Lexilla。

## 基本使用

1. 打开环境目录中的 `Flaragon.exe`，保留配套的 `lib` 和 `licenses` 目录。
2. 将组件解压到 `bin/apache/<包目录>`、`bin/php/<包目录>`、`bin/mysql/<包目录>` 或 `bin/redis/<包目录>`。
3. 在“环境设置”配置组件、端口和使用方式，点击“应用设置”。手动添加组件后点击右上角“刷新组件”，无需重开软件，未保存的输入会保留。MySQL 服务需自行添加。
4. 在“项目管理”添加项目目录、网站入口和所用 PHP，也可在 PHP 设置中开启独立目录入口。
5. 在“服务管理”启停服务、编辑配置或查看日志。关闭主窗口会隐藏到托盘；完整退出请使用“停止并退出”。

配置入口编辑实例实际加载的文件，文件中的 Flaragon 管理段由界面维护。已有 MySQL 数据通过“检查目录 → 接入数据”确认接入，移除服务不会删除数据。环境目录改名或移动前，先完整停止服务并退出软件。

## 目录

| 目录 | 用途 |
| --- | --- |
| `bin/` | 组件程序 |
| `config/` | 软件设置与实例配置 |
| `data/` | MySQL、Redis 数据 |
| `www/` | 默认项目目录，也可在界面选择其他本机目录 |
| `runtime/` | 运行状态、候选配置与临时文件 |
| `logs/` | 服务日志 |

## 源码构建

准备项目内工具链及编辑器依赖后，在仓库根目录运行：

```powershell
.\scripts\build.ps1
```

开发程序生成于 `target/x86_64-pc-windows-msvc/release/Flaragon.exe`。从该位置启动时显式指定环境目录：

```powershell
.\target\x86_64-pc-windows-msvc\release\Flaragon.exe (Get-Location).Path
```

准备条件、打包命令和产物命名见[打包操作](docs/packaging.zh-CN.md)。组件、数据、配置、缓存和构建产物不纳入 Git。

## 文档

- [安装与升级](docs/deployment.zh-CN.md)
- [局域网访问](docs/lan-access.zh-CN.md)
- [设计与功能说明](docs/design.zh-CN.md)
- [开发约定](AGENTS.md) · [待办与限制](PLAN.md)
- [更新说明](CHANGELOG.md) · [第三方来源与许可](docs/third-party.zh-CN.md)
