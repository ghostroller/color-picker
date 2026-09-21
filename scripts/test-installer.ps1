[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $PackageDirectory,
    [string] $CompilerPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT' -or -not [Environment]::Is64BitProcess) {
    throw 'Installer smoke requires 64-bit PowerShell 7 on Windows.'
}
if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'PowerShell 7 is required for bounded child-process cleanup.' }
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$PackageDirectory = (Resolve-Path -LiteralPath $PackageDirectory).Path
$payloadFiles = @('color-picker.exe', 'README.md', 'README.zh-CN.md', 'LICENSE', 'THIRD-PARTY-NOTICES.html')
foreach ($relative in $payloadFiles) {
    if (-not (Test-Path -LiteralPath (Join-Path $PackageDirectory $relative) -PathType Leaf)) {
        throw "A complete portable payload is required: missing $relative"
    }
}
if (@(Get-ChildItem -LiteralPath $PackageDirectory -Force).Count -ne $payloadFiles.Count) {
    throw 'The portable package must contain exactly the five runtime payload files.'
}
$CompilerPath = & (Join-Path $PSScriptRoot 'find-inno-setup.ps1') -CompilerPath $CompilerPath
$CompilerPath = (Resolve-Path -LiteralPath $CompilerPath).Path
$caseId = [Guid]::NewGuid().ToString('N')
$smokeRoot = [IO.Path]::GetFullPath((Join-Path $repositoryRoot 'target/installer-smoke'))
$caseRoot = [IO.Path]::GetFullPath((Join-Path $smokeRoot $caseId))
$installDirectory = [IO.Path]::GetFullPath((Join-Path $caseRoot 'Color Picker 测试'))
if (-not $caseRoot.StartsWith($smokeRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or
    -not $installDirectory.StartsWith($caseRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Smoke paths must remain inside the dedicated workspace test directory.'
}
$identity = "ColorPicker.Smoke.$caseId"
$displayName = "Color Picker Smoke $caseId"
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\${identity}_is1"
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$startShortcut = Join-Path ([Environment]::GetFolderPath('Programs')) "$displayName.lnk"
$desktopShortcut = Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) "$displayName.lnk"
$configurationPath = Join-Path $env:LOCALAPPDATA 'color-picker/config.json'
$installedExe = Join-Path $installDirectory 'color-picker.exe'
$uninstaller = Join-Path $installDirectory 'unins000.exe'

function Get-ConfigurationFingerprint {
    if (Test-Path -LiteralPath $configurationPath -PathType Leaf) {
        return (Get-FileHash -LiteralPath $configurationPath -Algorithm SHA256).Hash
    }
    return '<absent>'
}

function Get-SmokeStartupValue {
    if (Test-Path -LiteralPath $runKey) {
        $key = Get-Item -LiteralPath $runKey
        try { return $key.GetValue($identity, $null) }
        finally { $key.Close() }
    }
    return $null
}

function Invoke-BoundedProcess {
    param([string] $FilePath, [string[]] $Arguments, [string] $Stage, [switch] $AllowFailure)
    # Start-Process joins its argument array; preserve spaces and Win32 quoting.
    $commandLine = ($Arguments | ForEach-Object {
        $escaped = [regex]::Replace($_, '(\\*)"', '$1$1\"')
        '"' + [regex]::Replace($escaped, '(\\+)$', '$1$1') + '"'
    }) -join ' '
    $process = Start-Process -FilePath $FilePath -ArgumentList $commandLine -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $caseRoot "$Stage.stdout.log") `
        -RedirectStandardError (Join-Path $caseRoot "$Stage.stderr.log")
    try {
        # Bounded equivalent of -Wait; never wait indefinitely on a modal error.
        if (-not $process.WaitForExit(60000)) {
            $process.Kill($true) # Only the process tree launched directly above.
            $null = $process.WaitForExit(5000)
            throw "$Stage exceeded 60 seconds; its isolated process tree was stopped."
        }
        $exitCode = $process.ExitCode
        Write-Host "$Stage exit=$exitCode"
        if (-not $AllowFailure -and $exitCode -ne 0) { throw "$Stage failed with exit code $exitCode. See $caseRoot" }
        return $exitCode
    }
    finally { $process.Dispose() }
}

function Invoke-SmokeInstall {
    param([string] $Installer, [string] $Stage, [string[]] $TaskArguments = @(),
        [switch] $AllowFailure, [string] $Destination = $installDirectory)
    $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', '/LANG=english',
        '/NOCLOSEAPPLICATIONS', '/NORESTARTAPPLICATIONS', "/DIR=$Destination",
        "/LOG=$(Join-Path $caseRoot "$Stage.setup.log")") + $TaskArguments
    Invoke-BoundedProcess $Installer $arguments $Stage -AllowFailure:$AllowFailure
}

function Assert-Installed {
    param([string] $Version, [bool] $Startup, [bool] $Desktop)
    $registration = Get-ItemProperty -LiteralPath $uninstallKey
    if ($registration.DisplayVersion -cne $Version) {
        throw "Installed version differs from expected $Version."
    }
    foreach ($relative in $payloadFiles) {
        $installed = Join-Path $installDirectory $relative
        if (-not (Test-Path -LiteralPath $installed -PathType Leaf) -or
            (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -cne
            (Get-FileHash -LiteralPath (Join-Path $PackageDirectory $relative) -Algorithm SHA256).Hash) {
            throw "Installed file is missing or differs from payload: $relative"
        }
    }
    $expectedFiles = @($payloadFiles) + @('unins000.exe', 'unins000.dat')
    $actualFiles = @(Get-ChildItem -LiteralPath $installDirectory -Force)
    if ($actualFiles.Count -ne $expectedFiles.Count -or
        @($actualFiles | Where-Object { $_.PSIsContainer -or $_.Name -cnotin $expectedFiles }).Count -ne 0) {
        throw 'The installation must contain only five payload files and two uninstall files.'
    }
    $startupValue = Get-SmokeStartupValue
    $expectedStartup = '"{0}" --startup' -f $installedExe
    if (($Startup -and $startupValue -cne $expectedStartup) -or (-not $Startup -and $null -ne $startupValue)) {
        throw 'Optional startup registration does not match the selected task.'
    }
    if (-not (Test-Path -LiteralPath $startShortcut -PathType Leaf) -or
        (Test-Path -LiteralPath $desktopShortcut -PathType Leaf) -ne $Desktop) {
        throw 'Installed shortcut state does not match the selected tasks.'
    }
}

function Assert-Uninstalled {
    # Inno's temporary second stage removes the uninstaller after the first
    # stage exits. Wait for that cleanup before asserting or trying again.
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ((Test-Path -LiteralPath $uninstaller) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
    if ((Test-Path -LiteralPath $installedExe) -or (Test-Path -LiteralPath $uninstallKey) -or
        (Test-Path -LiteralPath $uninstaller) -or
        $null -ne (Get-SmokeStartupValue) -or (Test-Path -LiteralPath $startShortcut) -or
        (Test-Path -LiteralPath $desktopShortcut)) {
        throw "Isolated installation was not completely removed: $caseRoot"
    }
}

function Get-FixtureHash {
    param([string] $Contents)
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($Contents)))
}

function Write-SmokeFixture {
    param([string] $Path, [string] $Contents)
    $fullPath = [IO.Path]::GetFullPath($Path)
    if (-not $fullPath.StartsWith($caseRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Fixture files must remain inside the isolated case directory.'
    }
    $null = New-Item -ItemType Directory -Path (Split-Path -Parent $fullPath) -Force
    [IO.File]::WriteAllText($fullPath, $Contents, [Text.UTF8Encoding]::new($false))
}

function Remove-SmokeFixture {
    param([string] $Path)
    $fullPath = [IO.Path]::GetFullPath($Path)
    if (-not $fullPath.StartsWith($caseRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Fixture removal must remain inside the isolated case directory.'
    }
    # Deliberately nonrecursive, including when removing a junction itself.
    if (Test-Path -LiteralPath $fullPath) { Remove-Item -LiteralPath $fullPath -Force }
}

function Assert-FixtureContents {
    param([string] $Path, [string] $Contents)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf) -or
        [IO.File]::ReadAllText($Path) -cne $Contents) {
        throw "Cleanup unexpectedly modified or removed a protected fixture: $Path"
    }
}

function Add-LegacyFixtures {
    Write-SmokeFixture (Join-Path $installDirectory 'docs/unchanged.txt') $originalFixture
    Write-SmokeFixture (Join-Path $installDirectory 'licenses/nested/license.txt') $originalFixture
    Write-SmokeFixture (Join-Path $installDirectory 'Cargo.lock') $originalFixture
    Write-SmokeFixture (Join-Path $installDirectory 'docs/modified.txt') $modifiedFixture
    Write-SmokeFixture (Join-Path $installDirectory 'docs/unknown.txt') $originalFixture
    Write-SmokeFixture (Join-Path $caseRoot 'outside.txt') $originalFixture
    Write-SmokeFixture (Join-Path $caseRoot 'outside-junction/keep.txt') $originalFixture
    $null = New-Item -ItemType Junction -Path (Join-Path $installDirectory 'docs/junction') `
        -Target (Join-Path $caseRoot 'outside-junction')
}

function Assert-LegacyCleanup {
    foreach ($relative in @('docs/unchanged.txt', 'licenses', 'Cargo.lock')) {
        if (Test-Path -LiteralPath (Join-Path $installDirectory $relative)) {
            throw "An unchanged legacy file or empty parent directory was not removed: $relative"
        }
    }
    Assert-FixtureContents (Join-Path $installDirectory 'docs/modified.txt') $modifiedFixture
    Assert-FixtureContents (Join-Path $installDirectory 'docs/unknown.txt') $originalFixture
    Assert-FixtureContents (Join-Path $caseRoot 'outside.txt') $originalFixture
    Assert-FixtureContents (Join-Path $caseRoot 'outside-junction/keep.txt') $originalFixture
    if (((Get-Item -LiteralPath (Join-Path $installDirectory 'docs/junction')).Attributes -band
        [IO.FileAttributes]::ReparsePoint) -eq 0) { throw 'The protected junction was removed or replaced.' }
    foreach ($relative in @('docs/junction', 'docs/modified.txt', 'docs/unknown.txt', 'docs')) {
        Remove-SmokeFixture (Join-Path $installDirectory $relative)
    }
}

# A GUID identity must be new. Do not reuse or mutate any previous installation.
if ((Test-Path -LiteralPath $caseRoot) -or (Test-Path -LiteralPath $uninstallKey) -or
    $null -ne (Get-SmokeStartupValue) -or (Test-Path -LiteralPath $startShortcut) -or
    (Test-Path -LiteralPath $desktopShortcut)) { throw 'Smoke identity unexpectedly exists; refusing to reuse it.' }
New-Item -ItemType Directory -Path $caseRoot | Out-Null
$configurationBefore = Get-ConfigurationFingerprint
$originalFixture = "Original legacy release fixture`n"
$modifiedFixture = "Locally modified release fixture`n"
$originalHash = Get-FixtureHash $originalFixture
$fixtureManifest = Join-Path $caseRoot 'legacy-fixture.sha256'
$manifestLines = @(Get-Content -LiteralPath (Join-Path $repositoryRoot 'installer/legacy-files.sha256'))
foreach ($relative in @('docs/unchanged.txt', 'licenses/nested/license.txt', 'Cargo.lock', 'docs/modified.txt',
    'docs/junction/keep.txt', 'docs/../../outside.txt', '../outside.txt', 'C:/outside.txt')) {
    $manifestLines += "$originalHash  $relative"
}
# Even a manifest entry must not authorize deletion of a current runtime file.
$manifestLines += "$(Get-FileHash -LiteralPath (Join-Path $PackageDirectory 'README.md') -Algorithm SHA256 | Select-Object -ExpandProperty Hash)  README.md"
[IO.File]::WriteAllLines($fixtureManifest, $manifestLines, [Text.UTF8Encoding]::new($false))
Write-Host "Isolated installer smoke: $caseRoot"
Write-Host 'Installer versions 0.1.0 and 0.1.1 below are synthetic upgrade fixtures, not release versions.'
try {
    foreach ($version in @('0.1.0', '0.1.1')) {
        $arguments = @('/Qp', "/DAppVersion=$version", "/DVersionInfoVersion=$version.0",
            "/DPayloadDir=$PackageDirectory", "/DOutputDir=$caseRoot", "/DOutputBaseName=setup-$version",
            "/DLegacyManifest=$fixtureManifest",
            "/DSmokeTestId=$caseId", "/DDefaultInstallDir=$installDirectory",
            (Join-Path $repositoryRoot 'installer/color-picker.iss'))
        $null = Invoke-BoundedProcess $CompilerPath $arguments "compile-$version"
    }
    $older = Join-Path $caseRoot 'setup-0.1.0.exe'
    $newer = Join-Path $caseRoot 'setup-0.1.1.exe'
    $null = Invoke-SmokeInstall $older 'install-default'
    Assert-Installed '0.1.0' $false $false
    $null = Invoke-BoundedProcess $installedExe @('--check-environment') 'check-environment'
    $null = Invoke-SmokeInstall $older 'enable-tasks' @('/TASKS=startup,desktopicon')
    Assert-Installed '0.1.0' $true $true
    Add-LegacyFixtures
    $null = Invoke-SmokeInstall $newer 'upgrade-preserve-tasks'
    Assert-LegacyCleanup
    Assert-Installed '0.1.1' $true $true
    $moveCode = Invoke-SmokeInstall $newer 'reject-directory-move' -Destination (Join-Path $caseRoot 'moved') -AllowFailure
    if ($moveCode -eq 0 -or (Test-Path -LiteralPath (Join-Path $caseRoot 'moved/color-picker.exe'))) {
        throw 'Upgrade unexpectedly allowed moving the installation directory.'
    }
    Assert-Installed '0.1.1' $true $true
    $downgradeCode = Invoke-SmokeInstall $older 'reject-downgrade' -AllowFailure
    if ($downgradeCode -eq 0) { throw 'Downgrade unexpectedly succeeded.' }
    Assert-Installed '0.1.1' $true $true
    $null = Invoke-SmokeInstall $newer 'disable-tasks' @('/TASKS=!startup,!desktopicon')
    Assert-Installed '0.1.1' $false $false
    $null = Invoke-SmokeInstall $newer 'reenable-before-uninstall' @('/TASKS=startup,desktopicon')
    Assert-Installed '0.1.1' $true $true
    $null = Invoke-BoundedProcess $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART',
        "/LOG=$(Join-Path $caseRoot 'uninstall.setup.log')") 'uninstall'
    Assert-Uninstalled
    # The cleanup guard must also reject a junction at the installation root or
    # above it, even when the legacy file itself is an ordinary unchanged file.
    foreach ($reparseCase in @('root', 'ancestor')) {
        $junction = Join-Path $caseRoot "reparse-$reparseCase"
        $junctionTarget = Join-Path $caseRoot "reparse-$reparseCase-target"
        $null = New-Item -ItemType Directory -Path $junctionTarget
        $null = New-Item -ItemType Junction -Path $junction -Target $junctionTarget
        $installDirectory = if ($reparseCase -eq 'root') { $junction } else { Join-Path $junction 'child' }
        $installedExe = Join-Path $installDirectory 'color-picker.exe'
        $uninstaller = Join-Path $installDirectory 'unins000.exe'
        Write-SmokeFixture (Join-Path $installDirectory 'docs/unchanged.txt') $originalFixture
        $null = Invoke-SmokeInstall $newer "install-reparse-$reparseCase"
        Assert-FixtureContents (Join-Path $installDirectory 'docs/unchanged.txt') $originalFixture
        Remove-SmokeFixture (Join-Path $installDirectory 'docs/unchanged.txt')
        Remove-SmokeFixture (Join-Path $installDirectory 'docs')
        Assert-Installed '0.1.1' $false $false
        $null = Invoke-BoundedProcess $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART',
            "/LOG=$(Join-Path $caseRoot "uninstall-reparse-$reparseCase.setup.log")") "uninstall-reparse-$reparseCase"
        Assert-Uninstalled
        Remove-SmokeFixture $junction
    }
}
finally {
    $cleanupError = $null
    try {
        if ((Test-Path -LiteralPath $uninstallKey) -or (Test-Path -LiteralPath $installedExe)) {
            $null = Invoke-BoundedProcess $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART',
                "/LOG=$(Join-Path $caseRoot 'cleanup.setup.log')") 'cleanup'
        }
        Assert-Uninstalled
    }
    catch { $cleanupError = $_ }
    if ((Get-ConfigurationFingerprint) -cne $configurationBefore) {
        throw 'User configuration changed during installer smoke; the test never writes or restores this file.'
    }
    if ($cleanupError) { throw $cleanupError }
}
Write-Host "Installer lifecycle smoke passed; retained compiler/install logs in $caseRoot"
