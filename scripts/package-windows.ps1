[CmdletBinding()]
param(
    # Use only after verify-windows.ps1 passed for these same sources/toolchain.
    [switch] $SkipChecks,
    # Stable names and metadata for an explicitly requested public release.
    [switch] $Release,
    # Optional machine-readable output for the installer and CI; never scrape console text.
    [string] $OutputManifestPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Packaging requires Windows x64 and the MSVC toolchain.' }
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ($OutputManifestPath) { $OutputManifestPath = [IO.Path]::GetFullPath($OutputManifestPath) }
Push-Location -LiteralPath $repositoryRoot
try {
    if (-not $SkipChecks) { & (Join-Path $PSScriptRoot 'verify-windows.ps1') }
    # Even with checks skipped, build the exact current sources; never copy an old EXE.
    & cargo build --release --locked --target x86_64-pc-windows-msvc | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    $metadataJson = & cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Cargo license inventory failed.' }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $project = $metadata.packages | Where-Object { $_.name -eq 'color-picker' }
    $commit = & git -c "safe.directory=$repositoryRoot" rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Could not identify source commit.' }
    $changes = @(& git -c "safe.directory=$repositoryRoot" status --porcelain)
    if ($LASTEXITCODE -ne 0) { throw 'Could not read source state.' }
    if ($Release -and $changes.Count -ne 0) { throw 'Release packaging requires a clean working tree.' }
    $packageName = if ($Release) {
        'color-picker-{0}-windows-x64' -f $project.version
    } else {
        'color-picker-{0}-windows-x64-preview-{1}-{2}' -f $project.version,
            (Get-Date -Format 'yyyyMMdd-HHmmss'), $commit.Substring(0, 7)
    }
    $dist = Join-Path $repositoryRoot 'dist'
    $package = Join-Path $dist $packageName
    # New unique output only. No recursive deletion and no overwriting older packages.
    New-Item -ItemType Directory -Path $package -ErrorAction Stop | Out-Null
    $exe = Join-Path $metadata.target_directory 'x86_64-pc-windows-msvc/release/color-picker.exe'
    Copy-Item -LiteralPath $exe -Destination (Join-Path $package 'color-picker.exe')
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination $package
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'LICENSE-STATUS.md') -Destination $package
    $docs = Join-Path $package 'docs'
    New-Item -ItemType Directory -Path $docs | Out-Null
    foreach ($name in @('validation.md', 'known-limitations.md', 'troubleshooting.md', 'resource-probe.md', 'performance-running-app.md', 'ui-preview.md', 'windows-installer.md', 'ci-packaging.md')) {
        Copy-Item -LiteralPath (Join-Path $repositoryRoot "docs/$name") -Destination $docs
    }
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'docs/measurements') -Destination $docs -Recurse
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'docs/images') -Destination $docs -Recurse
    $readme = @'
# Color Picker __VERSION____CHANNEL__ for Windows x64

[简体中文](README.zh-CN.md) · [Full user and developer guide](https://github.com/ghostroller/color-picker/blob/main/README.md)

A small Windows system tray app for picking screen colors. The app interface is currently in Simplified Chinese.

1. Extract the ZIP and run `color-picker.exe`. Your first manual launch shows a short guide, then the app stays in the system tray.
2. Press **Ctrl + Alt + C**, click its tray icon, or choose **开始取色** (Start picking) from the tray menu.
3. Left-click to pick a color. Copy **HEX**, **RGB**, **CSS RGB**, or **HSL** from the result window.

Right-click or press **Esc** to cancel. Scroll up to freeze and magnify the screen; scroll down to zoom out, returning to live picking below 4×. Hovering over frozen pixels updates their HEX and original screen X/Y coordinates alongside the zoom level. In frozen mode, clicking outside the picker also cancels. The tray menu shows the current shortcut and its registration status.

Choose **设置** (Settings) from the tray menu to change the shortcut, default copy format, automatic copying, Quick pick, border width (0–6 DIP), or background transparency (0–80%). Defaults are 2 DIP and 35%; 0 hides the border or makes the background opaque, respectively. Settings fits the screen's available height, with scrollable content and **应用** (Apply) fixed at the bottom. Its synthetic-color preview updates as you drag the sliders; click Apply to save. Close Settings to resume picking; trying to pick while it is open restores it with an explanation and preserves your edits. Saved preferences survive restarts. For the shortcut's main key, click the key button and press A–Z, 0–9, or F1–F11; Esc or leaving the control cancels recording.

Automatic copying and **快速取色** (Quick pick) are off by default. Quick pick always copies the default format; on success it leaves your current app focused without a result popup, while failure opens the result for retry. Your ordinary automatic-copy preference is retained while Quick pick is enabled. In a normal result window, a successful copy briefly shows **已复制** (Copied) on its button without expanding the layout; only errors need extra space.

Use **退出** (Exit) in the tray menu to quit. Closing a result window leaves the app running. Administrator access is not required, and the app has no background network features.

## Installation and help

The portable ZIP does not register a startup entry. The installer offers optional startup at Windows sign-in, which stays quiet without showing the welcome guide; see the [installation guide](docs/windows-installer.md).

Preferences are stored in `%LOCALAPPDATA%\color-picker\config.json`. A damaged file or unknown configuration version is preserved; exit the app, repair or move the file, and restart to restore saving. Shortcut conflicts and failed saves preserve the previous settings. See [troubleshooting](docs/troubleshooting.md) for help.

The welcome guide is remembered separately in `%LOCALAPPDATA%\color-picker\welcome-v1.seen`. Automation can use `--no-onboarding` to suppress the guide without marking it as seen.

For diagnostic logging, first exit the running instance, then run this command in PowerShell:

```powershell
.\color-picker.exe --log-file .\color-picker.log
```

Logging is off by default, capped at 1 MiB per file, and excludes pixel and clipboard contents.

This build has not passed the full release hardware and environment matrix. See [known limitations](docs/known-limitations.md) and [validation records](docs/validation.md). `build-info.json` records the source, toolchain, and executable checksum; the adjacent `.sha256` file verifies the ZIP.

Dependency notices are in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) and `licenses/`. See [LICENSE-STATUS.md](LICENSE-STATUS.md) for the project's license status.
'@
    $readme = $readme.Replace('__VERSION__', $project.version)
    $readme = $readme.Replace('__CHANNEL__', $(if ($Release) { '' } else { ' preview' }))
    Set-Content -LiteralPath (Join-Path $package 'README.md') -Value $readme -Encoding UTF8
    $readmeChinese = @'
# Color Picker __VERSION____CHANNEL__（Windows x64）

[English](README.md) · [完整用户与开发者指南](https://github.com/ghostroller/color-picker/blob/main/README.zh-CN.md)

一款驻留 Windows 系统托盘的轻量屏幕取色工具。当前应用界面为简体中文。

1. 解压 ZIP，运行 `color-picker.exe`；首次手动启动会显示简短指引，之后应用驻留系统托盘。
2. 按 **Ctrl + Alt + C**、点击托盘图标，或在托盘菜单选择**开始取色**。
3. 左键确认颜色，然后在结果窗口复制 **HEX**、**RGB**、**CSS RGB** 或 **HSL**。

右键或 **Esc** 取消。滚轮向上冻结并放大画面，向下缩小；缩出 4× 后返回实时取色。在冻结像素格内移动鼠标，信息栏会更新 HEX、原始屏幕 X/Y 坐标和倍率。冻结模式下，左键点击取色窗口外也会取消。托盘菜单显示当前快捷键及注册状态。

托盘菜单中的**设置**可更改快捷键、默认复制格式、自动复制、快速取色、边框粗细（0–6 DIP）和背景透明度（0–80%）。默认边框 2 DIP、透明度 35%；边框设为 0 时隐藏，透明度设为 0 时背景不透明。设置按屏幕可用高度调整，内容可滚动，底部**应用**按钮固定可见。拖动滑块会立即更新合成颜色预览，点击应用才保存。关闭设置后继续取色；设置打开时触发取色，会恢复设置窗口、说明原因并保留编辑。重启后设置仍保留。录入快捷键主键时，点击主键按钮后按 A–Z、0–9 或 F1–F11；Esc 或离开控件取消监听。

自动复制和**快速取色**默认均关闭。快速取色始终复制默认格式，成功时不弹出结果、不改变当前应用焦点，失败才显示结果供重试；普通自动复制偏好会保留。普通结果窗口复制成功后，对应按钮短暂显示**已复制**，正常布局不变；只有错误信息才会展开额外空间。

退出请使用托盘菜单中的**退出**；关闭结果窗口后应用仍会驻留。应用无需管理员权限，没有后台网络功能。

## 安装与帮助

便携 ZIP 不注册启动项。安装版可选择登录 Windows 时自动启动，此时保持安静、不弹出首次指引，详见[安装指南](docs/windows-installer.md)。

配置保存在 `%LOCALAPPDATA%\color-picker\config.json`。损坏或未知版本的文件会保留；退出应用后修复或移走该文件，重新启动即可恢复保存设置。快捷键冲突或保存失败时保留原设置。遇到问题可查看[故障排查](docs/troubleshooting.md)。

首次指引使用独立的 `%LOCALAPPDATA%\color-picker\welcome-v1.seen` 文件记录。自动化启动可使用 `--no-onboarding` 跳过指引，不会将其标记为已显示。

需要诊断日志时，先退出旧实例，再在 PowerShell 中运行：

```powershell
.\color-picker.exe --log-file .\color-picker.log
```

默认不写日志；单个日志最多 1 MiB，不记录像素或剪贴板内容。

此包尚未通过全部发布实机与环境矩阵，详见[已知限制](docs/known-limitations.md)和[验证记录](docs/validation.md)。`build-info.json` 记录源码、工具链和 EXE 校验值；包外的 `.sha256` 文件用于校验 ZIP。

依赖许可见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) 和 `licenses/`。项目许可状态见 [LICENSE-STATUS.md](LICENSE-STATUS.md)。
'@
    $readmeChinese = $readmeChinese.Replace('__VERSION__', $project.version)
    $readmeChinese = $readmeChinese.Replace('__CHANNEL__', $(if ($Release) { '' } else { ' 预览版' }))
    Set-Content -LiteralPath (Join-Path $package 'README.zh-CN.md') -Value $readmeChinese -Encoding UTF8
    $licenses = Join-Path $package 'licenses'
    New-Item -ItemType Directory -Path $licenses | Out-Null
    $inventory = [Collections.Generic.List[string]]::new()
    $inventory.Add('# Third-party dependency notices')
    $inventory.Add('')
    $inventory.Add('Includes locked runtime and build dependencies; some are not linked into the executable.')
    $inventory.Add('Original license and notice texts are retained under licenses/.')
    $inventory.Add('')
    $inventory.Add('| Package | Version | Declared license |')
    $inventory.Add('|---|---|---|')
    foreach ($dependency in ($metadata.packages | Where-Object { $_.name -ne 'color-picker' } | Sort-Object name,version)) {
        $inventory.Add("| $($dependency.name) | $($dependency.version) | $($dependency.license) |")
        $source = Split-Path -Parent $dependency.manifest_path
        $licenseFiles = @(Get-ChildItem -LiteralPath $source -File | Where-Object { $_.Name -match '^(LICENSE|COPYING|NOTICE)' })
        if ($dependency.license_file) {
            $licenseFiles += Get-Item -LiteralPath (Join-Path $source $dependency.license_file)
        }
        $licenseFiles = @($licenseFiles | Sort-Object FullName -Unique)
        if ($licenseFiles.Count -eq 0) { throw "Missing license text for $($dependency.name)." }
        $destination = Join-Path $licenses "$($dependency.name)-$($dependency.version)"
        New-Item -ItemType Directory -Path $destination | Out-Null
        foreach ($file in $licenseFiles) { Copy-Item -LiteralPath $file.FullName -Destination $destination }
    }
    $sysroot = & rustc --print sysroot
    if ($LASTEXITCODE -ne 0) { throw 'Could not locate Rust library notices.' }
    $rustDocs = Join-Path $sysroot 'share/doc/rust'
    $rustNotices = Join-Path $licenses 'rust-library'
    New-Item -ItemType Directory -Path $rustNotices | Out-Null
    Copy-Item -LiteralPath (Join-Path $rustDocs 'COPYRIGHT-library.html') -Destination $rustNotices
    Copy-Item -LiteralPath (Join-Path $rustDocs 'licenses') -Destination $rustNotices -Recurse
    Set-Content -LiteralPath (Join-Path $package 'THIRD-PARTY-NOTICES.md') -Value $inventory -Encoding UTF8
    $rustVersion = (& rustc -Vv) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Could not identify compiler.' }
    $info = [ordered]@{
        version = $project.version
        channel = $(if ($Release) { 'release' } else { 'local-preview' })
        source_commit = $commit
        source_dirty = ($changes.Count -ne 0)
        built_at_utc = [DateTime]::UtcNow.ToString('o')
        target = 'x86_64-pc-windows-msvc'
        rustc = $rustVersion
        os = [Environment]::OSVersion.VersionString
        crt = 'static'
        checks_run_by_packager = (-not $SkipChecks)
        exe_sha256 = (Get-FileHash -LiteralPath (Join-Path $package 'color-picker.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
        exe_bytes = (Get-Item -LiteralPath (Join-Path $package 'color-picker.exe')).Length
    }
    $info | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $package 'build-info.json') -Encoding UTF8
    $zip = Join-Path $dist "$packageName.zip"
    Compress-Archive -LiteralPath $package -DestinationPath $zip -CompressionLevel Optimal
    $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$zip.sha256" -Value "$hash  $packageName.zip" -Encoding ASCII
    Write-Host "Package: $zip"
    Write-Host "SHA256:  $hash"
    if ($OutputManifestPath) {
        [ordered]@{
            version = $project.version
            source_commit = $commit
            source_dirty = ($changes.Count -ne 0)
            package_directory = $package
            archive_path = $zip
            archive_sha256_path = "$zip.sha256"
        } | ConvertTo-Json | Set-Content -LiteralPath $OutputManifestPath -Encoding UTF8
    }
}
finally { Pop-Location }
