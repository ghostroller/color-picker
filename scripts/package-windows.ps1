[CmdletBinding()]
param(
    # Use only after verify-windows.ps1 passed for these same sources/toolchain.
    [switch] $SkipChecks
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Packaging requires Windows x64 and the MSVC toolchain.' }
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
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
    $packageName = 'color-picker-{0}-windows-x64-preview-{1}-{2}' -f $project.version,
        (Get-Date -Format 'yyyyMMdd-HHmmss'), $commit.Substring(0, 7)
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
    foreach ($name in @('validation.md', 'known-limitations.md', 'resource-probe.md', 'performance-running-app.md', 'ui-preview.md')) {
        Copy-Item -LiteralPath (Join-Path $repositoryRoot "docs/$name") -Destination $docs
    }
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'docs/measurements') -Destination $docs -Recurse
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'docs/images') -Destination $docs -Recurse
    $readme = @'
# color-picker 0.1.0 本地预览版（Windows x64）

解压后双击 color-picker.exe；默认 Ctrl + Alt + C 或托盘开始取色。
左键确认，右键 / Esc 取消，滚轮向上冻结放大、向下缩小；缩出 4× 返回实时。
冻结取色时左键点击窗外也会取消；操作说明可在设置页查看。
结果窗口提供 HEX / RGB / CSS RGB / HSL 与复制；托盘“设置”可更改快捷键、
默认复制格式、自动复制，点击“应用”后保存，重启保留。设置窗口打开时暂停取色入口。
主键点击后按 A–Z / 0–9 / F1–F11 录入；Esc 或离开该控件取消监听。
退出请使用托盘菜单。应用无需管理员权限，不包含开机启动或后台网络功能。

日志：先退出旧实例，在 PowerShell 中运行：
`.\color-picker.exe --log-file .\color-picker.log`
默认不写日志；单个日志最多 1 MiB，不记录像素或剪贴板内容。

配置：Windows LocalAppData 下 color-picker/config.json；
损坏或未知版本不会覆盖，修复/移走后重启。快捷键冲突或保存失败维持原设置。

此包尚未通过全部发布实机矩阵。详见 docs/known-limitations.md、docs/validation.md。
build-info.json 记录源码、工具链和 EXE 校验值，包外 .sha256 校验 ZIP。
licenses/ 与 THIRD-PARTY-NOTICES.md 提供依赖许可；项目许可状态见 LICENSE-STATUS.md。
'@
    Set-Content -LiteralPath (Join-Path $package 'README.md') -Value $readme -Encoding UTF8
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
        channel = 'local-preview'
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
}
finally { Pop-Location }
