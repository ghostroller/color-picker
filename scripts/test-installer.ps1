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
foreach ($relative in @('color-picker.exe', 'LICENSE-STATUS.md', 'THIRD-PARTY-NOTICES.md', 'licenses/rust-library/COPYRIGHT-library.html')) {
    if (-not (Test-Path -LiteralPath (Join-Path $PackageDirectory $relative) -PathType Leaf)) {
        throw "A complete portable payload is required: missing $relative"
    }
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
    foreach ($relative in @('color-picker.exe', 'LICENSE-STATUS.md', 'THIRD-PARTY-NOTICES.md', 'licenses/rust-library/COPYRIGHT-library.html')) {
        $installed = Join-Path $installDirectory $relative
        if (-not (Test-Path -LiteralPath $installed -PathType Leaf) -or
            (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -cne
            (Get-FileHash -LiteralPath (Join-Path $PackageDirectory $relative) -Algorithm SHA256).Hash) {
            throw "Installed file is missing or differs from payload: $relative"
        }
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

# A GUID identity must be new. Do not reuse or mutate any previous installation.
if ((Test-Path -LiteralPath $caseRoot) -or (Test-Path -LiteralPath $uninstallKey) -or
    $null -ne (Get-SmokeStartupValue) -or (Test-Path -LiteralPath $startShortcut) -or
    (Test-Path -LiteralPath $desktopShortcut)) { throw 'Smoke identity unexpectedly exists; refusing to reuse it.' }
New-Item -ItemType Directory -Path $caseRoot | Out-Null
$configurationBefore = Get-ConfigurationFingerprint
Write-Host "Isolated installer smoke: $caseRoot"
Write-Host 'Installer versions 0.1.0 and 0.1.1 below are synthetic upgrade fixtures, not release versions.'
try {
    foreach ($version in @('0.1.0', '0.1.1')) {
        $arguments = @('/Qp', "/DAppVersion=$version", "/DVersionInfoVersion=$version.0",
            "/DPayloadDir=$PackageDirectory", "/DOutputDir=$caseRoot", "/DOutputBaseName=setup-$version",
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
    $null = Invoke-SmokeInstall $newer 'upgrade-preserve-tasks'
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
