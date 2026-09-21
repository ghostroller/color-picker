# 手动构建与发布 Windows 安装包

仓库的 `Verify` 和 `Package Windows` 工作流都只允许手动触发；推送代码、PR 和标签不会启动它们。

在 GitHub 仓库的 **Actions → Package Windows → Run workflow** 中选择要构建的分支并运行。
工作流文件需要先存在于默认分支，GitHub 才会提供手动运行入口。
参见 [GitHub 手动运行工作流说明](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)。

`publish_release` 是布尔输入，默认为 `false`，只生成 Actions 产物。
启用后必须选用与 `Cargo.toml` 版本完全一致的 `v<版本>` 标签；不能从分支直接发版。
工作流不创建标签、不修改版本，也不推送代码。

## 发布到 GitHub Release

先提交版本、功能和文档改动并推送到默认分支，然后由维护者创建带说明的标签。
首次 `0.1.0` 发布示例：

```powershell
git push origin main
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
gh workflow run package-windows.yml --ref v0.1.0 -f publish_release=true
```

后续发布先递增 `Cargo.toml` 版本并更新 `Cargo.lock`，示例中的标签随之更新。
标签推送本身不会运行工作流。已发布标签不能通过移动标签来替换源码；修改后发布新版本。

打包 job 首先校验标签名、Cargo 版本与所选提交；构建后再次检查 manifest 中的版本、源码提交和干净状态。
通过常规验证及安装生命周期检查后，发布 job 使用 `gh run download` 下载**本次运行**的四个产物：

- `color-picker-0.1.0-windows-x64-setup.exe`
- `color-picker-0.1.0-windows-x64-setup.exe.sha256`
- `color-picker-0.1.0-windows-x64.zip`
- `color-picker-0.1.0-windows-x64.zip.sha256`

发布 job 使用 `gh release create --verify-tag --draft` 上传这些文件，确认四个资产完整后再公开 Release。
同标签只允许创建一次 Release：已存在的公开 Release 或草稿都会使运行失败，不覆盖、不使用 `--clobber`。
若创建或上传中断留下草稿，维护者需先核对运行日志与草稿资产，修复或删除该草稿后再决定重跑；
不能删除已经公开的 Release 来替换同版本的内容。

公开文件可从 [GitHub Releases](https://github.com/ghostroller/color-picker/releases) 长期下载。
工作流通过不等于完整实机矩阵验收完成，发布内容仍保留 [已知限制](known-limitations.md) 与验证记录。

## 构建与验证

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

普通构建保持预览包名称及构建信息；启用 `publish_release` 时，工作流给脚本传入 `-Release`，
生成上面不含时间戳的发布名称。`package-installer.ps1` 将该开关传给 `package-windows.ps1`，
使安装包、ZIP 和构建信息使用同一发布模式。构建信息位于 `dist/` 下 ZIP 旁的
`.build.json`，不再安装到用户目录。脚本不覆盖已有输出。

上传前还会运行 `scripts/test-installer.ps1 -PackageDirectory <本次便携包目录>`：
以独立 AppId、安装目录、启动项和快捷方式验证默认安装、任务启用与取消、升级保留、拒绝降级/改址、卸载清理。
同时检查精简后的文件清单，以及旧文件清理时保留修改文件、未知文件和目录联接指向的数据。
该检查使用真实 payload，`0.1.0` / `0.1.1` 仅为合成安装版本；不启动驻留程序，不修改用户配置。
每个测试子进程限时 60 秒，测试结束后卸载自己的隔离实例，日志保留在 `target/installer-smoke/`。
本地运行前需先安装固定版本的 Inno Setup，或通过 `-CompilerPath` 指向已安装的对应编译器。

成功后，运行页面的 **Artifacts** 提供一个保留 30 天的下载项，包含安装程序 EXE、便携 ZIP 及各自的 `.sha256`。
运行摘要同时列出版本、完整源码提交和两个包的 SHA256。下载并解压 Actions 产物后，可以用
`Get-FileHash -Algorithm SHA256 -LiteralPath <文件路径>` 与对应 `.sha256` 内容核对。
未启用 `publish_release` 时流程到此结束；启用后发布 job 继续将同一批文件放入对应 Release。

`Verify` 工作流及打包 job 仅请求 `contents: read`，不保存 checkout 凭据。
仅发布 job 请求 `contents: write` 来创建和发布 Release，以及 `actions: read` 来下载本次运行产物。
发布令牌只提供给需要 GitHub API 的发布步骤。
打包工作流使用完整提交固定官方 [checkout v7.0.1](https://github.com/actions/checkout/releases/tag/v7.0.1)
和 [upload-artifact v7.0.1](https://github.com/actions/upload-artifact/releases/tag/v7.0.1)。
升级时核对官方 release 对应提交再更新完整 SHA；工作流检查通过不等于已完成全部发布实机矩阵。
