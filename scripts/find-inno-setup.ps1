[CmdletBinding()]
param([string] $CompilerPath)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Inno Setup requires Windows.' }
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$pin = Get-Content -LiteralPath (Join-Path $repositoryRoot 'installer/toolchain.json') -Raw | ConvertFrom-Json
$candidates = @()
if ($CompilerPath) {
    $candidates += $CompilerPath
} else {
    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($command) { $candidates += $command.Source }
    foreach ($root in @(${env:ProgramFiles(x86)}, $env:ProgramFiles, (Join-Path $env:LOCALAPPDATA 'Programs'))) {
        if ($root) { $candidates += Join-Path $root 'Inno Setup 6/ISCC.exe' }
    }
}
foreach ($candidate in ($candidates | Select-Object -Unique)) {
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { continue }
    $file = Get-Item -LiteralPath $candidate
    # Official ISCC binaries may report 0.0.0.0 in PE metadata. Ask the actual
    # preprocessor instead; /O- makes this a compile check with no generated EXE.
    $output = & $file.FullName '/Q' '/O-' "/DExpectedInnoVersion=$($pin.version)" `
        (Join-Path $repositoryRoot 'installer/check-toolchain.iss') 2>&1
    if ($LASTEXITCODE -eq 0) { return $file.FullName }
    Write-Host ($output -join "`n")
    Write-Host "Ignoring incompatible compiler at $candidate; required: $($pin.version)."
}
throw "Install Inno Setup $($pin.version) first (see docs/windows-installer.md), or pass -CompilerPath <path to ISCC.exe>. Packaging never downloads or installs tools."
