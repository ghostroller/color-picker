# 手动构建 Windows 安装包

仓库的 `Verify` 和 `Package Windows` 工作流都只允许手动触发；推送代码、PR 和标签不会启动它们。

在 GitHub 仓库的 **Actions → Package Windows → Run workflow** 中选择要构建的分支并运行。
工作流文件需要先存在于默认分支，GitHub 才会提供手动运行入口。
参见 [GitHub 手动运行工作流说明](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)。

构建运行于 `windows-2022`，使用 PowerShell 7，按 `rust-toolchain.toml` 安装固定 Rust 版本（当前为 `1.95.0`）、
rustfmt、Clippy 和 `x86_64-pc-windows-msvc` 目标。
[该 runner 当前预装](https://github.com/actions/runner-images/blob/main/images/windows/Windows2022-Readme.md)
Inno Setup 6.7.1，未提供 WinGet，因此不依赖预装编译器或 Chocolatey 的可用版本。
仅在临时 GitHub runner 中，三个独立步骤使用 curl 下载 `installer/toolchain.json` 固定的
官方 Inno Setup 6.7.3 安装文件、严格核对 SHA256、以官方便携模式解包到 runner 临时目录，然后调用：

```powershell
./scripts/package-installer.ps1 -OutputManifestPath "$env:RUNNER_TEMP/color-picker-package.json" -CompilerPath "$env:RUNNER_TEMP/Inno Setup 6/ISCC.exe"
```

该调用保留脚本的全部默认验证，包括格式、Clippy、默认测试、Release 构建、内嵌 manifest 和进程 DPI 检查。
打包脚本仅使用已安装且版本匹配的编译器，不负责下载和执行安装工具。
需要交互桌面的 ignored 测试仍由本机验证流程承担。

上传前还会运行 `scripts/test-installer.ps1 -PackageDirectory <本次便携包目录>`：
以独立 AppId、安装目录、启动项和快捷方式验证默认安装、任务启用与取消、升级保留、拒绝降级/改址、卸载清理。
该检查使用真实 payload，`0.1.0` / `0.1.1` 仅为合成安装版本；不启动驻留程序，不修改用户配置。
每个测试子进程限时 60 秒，测试结束后卸载自己的隔离实例，日志保留在 `target/installer-smoke/`。
本地运行前需先安装固定版本的 Inno Setup，或通过 `-CompilerPath` 指向已安装的对应编译器。

成功后，运行页面的 **Artifacts** 提供一个保留 30 天的下载项，包含安装程序 EXE、便携 ZIP 及各自的 `.sha256`。
运行摘要同时列出版本、完整源码提交和两个包的 SHA256。下载并解压 Actions 产物后，可以用
`Get-FileHash -Algorithm SHA256 -LiteralPath <文件路径>` 与对应 `.sha256` 内容核对。
工作流仅上传构建产物；GitHub Release 由维护者另外管理。

两个工作流仅请求 `contents: read`，不保存 checkout 凭据。
打包工作流使用完整提交固定官方 [checkout v7.0.1](https://github.com/actions/checkout/releases/tag/v7.0.1)
和 [upload-artifact v7.0.1](https://github.com/actions/upload-artifact/releases/tag/v7.0.1)。
升级时核对官方 release 对应提交再更新完整 SHA；工作流检查通过不等于已完成全部发布实机矩阵。
