# 组件来源与第三方许可

Flaragon 的管理逻辑、原生界面与图标为本项目独立实现，没有复制 Laragon 或 FlyEnv 的应用源码。两者用于操作方式和功能边界参考：[Laragon](https://github.com/leokhoa/laragon)、[FlyEnv](https://github.com/xpf0000/FlyEnv)。

## 独立运行组件

| 组件 | 当前验证版本与来源 | 许可材料 |
| --- | --- | --- |
| Apache | 2.4.69 VS18 x64，[Apache Lounge](https://www.apachelounge.com/download/) | 发行包 `Apache24/LICENSE.txt`、`NOTICE.txt`，包含 Apache 及随包依赖说明 |
| PHP | 7.4.33 / 8.3.35 / 8.5.11 NTS x64，[PHP Windows 下载](https://windows.php.net/download/) | 各包 `license.txt`，PHP License 3.01；附带组件按各自条款 |
| MySQL | Community 5.7.44 / 8.0.44 x64，[MySQL Archives](https://downloads.mysql.com/archives/community/) | 各包 `LICENSE` 中的 Oracle / MySQL 与第三方许可说明 |
| Redis | 7.4.3 MSYS2 x64，[redis-windows 发布](https://github.com/redis-windows/redis-windows/releases/tag/7.4.3) | 社区 Windows 移植构建；Redis 7.4.3 的许可为 RSALv2 / SSPLv1 二选一，详见[对应源码许可](https://github.com/redis/redis/blob/7.4.3/LICENSE.txt)；MSYS2、OpenSSL 等随包 DLL 有各自许可 |

redis-windows 仓库标注的 Apache-2.0 是其仓库代码许可，不能据此将 Redis 7.4.3 引擎或整个发行包标记为 Apache-2.0。Flaragon 直接启动 `redis-server.exe`，不安装该包的系统服务包装器。该构建不是 Redis 官方 Windows 二进制发行包。

`bin` 内保存原始发行包文件；组件与业务数据分离，不把这些二进制合并进管理器。下载脚本固定已验证版本，下载成功不等于其他版本或其他机器已经通过兼容性测试。

实例配置的模板在运行时读取用户放入的组件包，来源包括 PHP 的 php.ini-development、Apache Lounge 的 conf/httpd.conf 和 redis-windows 的 redis.conf，适用各发行包许可。正式配置保留模板注释并注明包内来源；管理器不内嵌第三方模板文本。

## Rust 依赖与工具链

直接依赖包括 `slint` / `slint-build` 1.18.1、`windows-sys` 0.59、`raw-window-handle`、`serde`、`serde_json`、`anyhow`，以及部署归档使用的 `flate2` 1.1.10 和 `tar` 0.4.46；具体版本及传递依赖由 `Cargo.lock` 固定。完整依赖许可元数据可以在已准备工具链的本项目内运行以下命令查看：

```powershell
. .\scripts\env.ps1
cargo metadata --locked --offline --format-version 1
```

依赖缓存中保留各 crate 自带的许可文件。MSVC、Windows SDK、Rust、Python 和用于准备工具链的 portablemsvc 是开发工具，位于 `.tools` / `.cache`，不作为管理器可执行文件的运行组件分发。没有为验证结果主动计算额外校验和；Cargo 锁文件的校验字段由 Cargo 管理。

## 框架测试依赖

`testwork/cake-framework` 中的 Composer 2.10.3 来自 [Composer 官方下载](https://getcomposer.org/download/)，CakePHP 4.6.5 和 5.4.3 通过 Composer 安装自 [CakePHP 官方仓库](https://github.com/cakephp/cakephp)，采用 MIT 许可。各测试目录的 `composer.lock` 保存依赖版本与来源，`vendor` 保留框架及依赖自身的许可文件。这些包仅用于真实组件集成测试，不编入或随管理器分发。

## 主界面与编辑器

- [Slint 1.18.1](https://github.com/slint-ui/slint/tree/v1.18.1)：采用 Slint Royalty-free Desktop, Mobile, and Web Applications License 2.0，许可全文在 `licenses/Slint-Royalty-free-2.0.md`。主程序“帮助 → 关于 Flaragon”和安装升级程序“关于”中使用官方 `AboutSlint` 控件。
- [Scintilla 5.6.7 / Lexilla 5.5.4](https://www.scintilla.org/SciTEDownload.html)：使用官方 `wscite567.zip` 的 Windows x64 DLL，由官方 [SourceForge 发布目录](https://sourceforge.net/projects/scintilla/files/SciTE/5.6.7/) 下载。源码 API 消息编号来源于上游公开头文件；许可全文在 `licenses/Scintilla.txt`。运行时从环境根目录的 `lib` 加载 DLL，不通过 PATH 搜索。
- Rust 依赖的许可与版权文件汇集于 `licenses/rust`，清单由 `scripts/collect-licenses.py` 按 Cargo.lock 和项目内依赖缓存生成；工具链、测试组件包和完整缓存不属于管理器运行时发布文件。

发布目录须同时包含 `Flaragon.exe`、`lib/Scintilla.dll`、`lib/Lexilla.dll` 和 `licenses`。不得移除 AboutSlint 署名或依赖版权声明。
