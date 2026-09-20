# Windows 安装与维护

## 方案选择

选择 **Inno Setup EXE，按当前用户安装**；保留 ZIP 便携包。

| 方案 | 启动项、升级方式 | 对本项目的取舍 |
|---|---|---|
| Inno Setup | 当前用户启动项；固定 AppId 覆盖安装；独立卸载程序 | 适合现有单 EXE / Win32 应用，无需引入运行时或管理员安装，采用此方案 |
| WiX / MSI | Windows Installer 的组件、产品与 MajorUpgrade 机制 | 更适合需要企业集中部署/修复的场景；当前维护组件身份与升级规则的成本偏高 |
| MSIX | 包身份、签名，配合 App Installer / Store 提供更新 | 后续商店分发可考虑；当前需额外解决可信签名、包身份与分发渠道 |

以上取舍基于本项目规模。参考官方文档：
[Inno 用户安装](https://jrsoftware.org/ishelp/topic_setup_privilegesrequired.htm)、
[稳定 AppId](https://jrsoftware.org/ishelp/topic_setup_appid.htm)、
[WiX MajorUpgrade](https://docs.firegiant.com/wix/schema/wxs/majorupgrade/)、
[MSIX 签名](https://learn.microsoft.com/en-us/windows/msix/package/sign-msix-package-guide)、
[App Installer 更新](https://learn.microsoft.com/en-us/windows/msix/app-installer/auto-update-and-repair--overview)。

## 使用安装包

从 [GitHub Releases](https://github.com/ghostroller/color-picker/releases) 下载 `*-setup.exe`，支持简体中文和英文。默认目录是
`%LOCALAPPDATA%\Programs\Color Picker`，无需管理员权限。
目标平台是 Windows 11 x64，安装器最低允许 Windows 10 22H2 x64；
最低版本门槛不等于已完成该平台的全部兼容性验收，见 [已知限制](known-limitations.md)。

- 安装时可选“登录 Windows 时启动 Color Picker”，首次默认关闭。
- 启动项为当前用户 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 的
  `Color Picker` 值，命令为带引号的安装路径加 `--startup`。
  Windows 可延迟启动，属于用户登录启动，并非开机前的系统服务。
- `--startup` 首次启动仅驻留托盘，已有实例时安静退出。
  普通开始菜单/桌面快捷方式保持再次启动可唤起取色的行为。
- 可以在 Windows“启动应用”/任务管理器中禁用；安装器不修改 Windows 的
  `StartupApproved` 禁用状态。重跑安装器取消启动选项可删除注册项。
- 配置继续保存于 `%LOCALAPPDATA%\color-picker\config.json`，与程序目录分离。
  覆盖升级、卸载均保留配置；要彻底清空偏好，退出程序后手动移走此文件。
- 卸载入口为 Windows“已安装的应用”中的 Color Picker。卸载删除程序文件、
  安装器创建的快捷方式和启动项，不扫描或删除用户额外放入目录的文件。

启动项依据 [Microsoft Run 键文档](https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys)。
安装任务记忆使用 [UsePreviousTasks](https://jrsoftware.org/ishelp/topic_setup_useprevioustasks.htm)。

## 升级和退出

下载较新版本安装包，直接运行即可。固定 AppId、安装范围和目录，安装器沿用原先的
启动/桌面快捷方式选择；同版本允许重装修复，低版本覆盖高版本会被拒绝，静默安装同样有效。
如需明确回退，先卸载再安装旧版本；保留的配置若 schema 较新，旧应用会保护原文件并禁用保存。

安装/卸载先调用对应安装目录中的 `color-picker.exe --quit`。
此参数只关闭可执行文件路径匹配的实例，最多等待 10 秒正常退出；找不到实例则直接结束。
不同目录的便携实例不会被此命令关闭。退出失败会中止并提示从托盘退出后重试。
升级还启用 [Restart Manager 检查](https://jrsoftware.org/ishelp/topic_setup_closeapplications.htm)，
不强行终止进程。静默安装不启动应用，交互安装完成页可选择重新启动。

当前提供手动下载和覆盖升级，无后台更新检查/下载服务；应用的后台资源占用不因此增加。

## 本地构建

按 README 安装 Rust 与 Windows SDK 后，使用系统包管理器单独安装编译器：

```powershell
winget install --id JRSoftware.InnoSetup -e --version 6.7.3 --source winget --scope user
.\scripts\package-installer.ps1
```

`installer/toolchain.json` 固定 Inno Setup 6.7.3 及官方安装文件 SHA256，来源是
[官方发布记录](https://github.com/jrsoftware/issrc/releases/tag/is-6_7_3)。
项目脚本只发现和调用已有的编译器，**不会下载或安装编译器**，不需要杀毒白名单。
非标准目录可传 `-CompilerPath 'D:\Tools\Inno Setup 6\ISCC.exe'`。
编译器版本不匹配时明确报错，不悄悄使用其他版本。

打包默认先运行格式、Clippy、单元/非交互测试、Release 和清单/DPI 检查。
同一源码刚验证通过可用 `-SkipChecks`，仍会确认 Release 构建和 EXE 版本。
输出保存在 `dist/`，默认预览包每次使用独立名称，包含版本、时间及短提交号：

- `*-setup.exe` 与 `.exe.sha256`：安装包和校验。
- `*.zip` 与 `.zip.sha256`：保留的便携包和校验。
- `*-setup.build.json`：源码、构建器和产物路径；程序包内部 `build-info.json`
  还包含是否有未提交改动、工具链、EXE 校验值和检查状态。

显式使用 `./scripts/package-installer.ps1 -Release` 可生成发布名称；
`package-windows.ps1` 同样支持 `-Release`。以 `0.1.0` 为例，安装包为
`color-picker-0.1.0-windows-x64-setup.exe`，便携包为 `color-picker-0.1.0-windows-x64.zip`，
各自附带 `.sha256`。发布包不含时间或短提交号；已有输出不会被覆盖。
`-Release` 只控制打包模式，不创建 Git 标签或发布 GitHub Release。

EXE、安装器显示版本和降级比较版本都来自 `Cargo.toml`。
安装包接受 `major.minor.patch`，每段 0–65535，Windows 资源版本为 `major.minor.patch.0`。
暂不接受 SemVer 预发布后缀，避免把 `rc` 和正式版本错误地视为相同版本。

## 后续维护约定

1. 每次面向用户发布升级，递增 `Cargo.toml` 版本并更新 `Cargo.lock`，提交并推送代码后，
   创建和推送 `v<版本>` 标签，再手动运行发布工作流。
   不把不同源码的同版本预览包当作有序升级渠道。
2. 不改 `installer/color-picker.iss` 的生产 AppId、用户范围、启动值名或配置路径。
   文件移除/重命名若需升级清理，加入明确的旧文件路径清单，不递归清空程序或配置目录。
3. `--startup` / `--quit` 是安装器兼容接口，未来版本应保留；不能移除旧版正常退出协议。
4. 修改配置 schema 时实现迁移并保留损坏/较新版本保护；安装器不替换配置。
5. 升级编译器时同步 `toolchain.json` 的版本/官方 URL/SHA256，核对语言包和安装行为，
   再运行安装生命周期 smoke。GitHub Actions 固定完整 commit，更新时一起核对官方版本。
6. 产物当前未签名。接入可信代码签名时，先签应用，再组装 ZIP 和安装器；
   Inno 的签名配置应覆盖卸载器与安装器，最终签名后重新计算校验值。
   不把签名证书或私钥提交到仓库。

CI 仅手动触发，默认 Actions 产物保留 30 天。维护者可在对应版本标签上启用
`publish_release`，通过构建和安装生命周期检查后，将本次 EXE、ZIP、两个校验文件发布到 GitHub Release，
作为长期下载来源。CI 不自动创建标签或推送版本；同标签已有 Release（包括草稿）时拒绝发布。
失败留下的草稿需要人工排查，不会在重跑时覆盖。完整操作见 [CI 打包说明](ci-packaging.md)。

许可证状态见 [LICENSE-STATUS.md](../LICENSE-STATUS.md)。
Inno Setup 自身使用条件见 [官方许可](https://jrsoftware.org/files/is/license.txt)，
后续商业使用时需按其许可安排构建工具授权。
