[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($env:OS -ne 'Windows_NT') {
    throw 'This launcher requires Windows and the MSVC toolchain.'
}

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$targetDirectory = Join-Path $repositoryRoot 'target\diagnostic'
$logDirectory = Join-Path $repositoryRoot 'logs'
$timestamp = [DateTime]::Now.ToString('yyyyMMdd-HHmmss')
$uniqueId = [guid]::NewGuid().ToString('N')
$logPath = Join-Path $logDirectory "color-picker-$timestamp-$uniqueId.log"

Write-Host '先从托盘退出旧实例；否则本次只激活旧进程并退出。'
Write-Host '正在构建独立的诊断版本，输出目录：' $targetDirectory

Push-Location -LiteralPath $repositoryRoot
try {
    & cargo build --release --locked --target x86_64-pc-windows-msvc --target-dir $targetDirectory | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE."
    }
}
finally {
    Pop-Location
}

$executable = Join-Path $targetDirectory 'x86_64-pc-windows-msvc\release\color-picker.exe'
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "The diagnostic executable is missing: $executable"
}
New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null

# Start-Process joins ArgumentList entries into a Windows command line. Quote the
# absolute log path explicitly so repository paths containing spaces stay intact.
$launchArguments = '--log-file "' + $logPath + '"'
$process = Start-Process -FilePath $executable -ArgumentList $launchArguments `
    -WorkingDirectory $repositoryRoot -WindowStyle Hidden -PassThru
try {
    Write-Host '日志路径：' $logPath
    if ($process.WaitForExit(2000)) {
        $process.Refresh()
        $exitCode = $process.ExitCode
        Write-Host "本次启动的进程已经退出，退出码：$exitCode"
        if (Test-Path -LiteralPath $logPath -PathType Leaf) {
            Get-Content -LiteralPath $logPath -Encoding UTF8 -Tail 80 | Out-Host
        }
        else {
            Write-Host '尚未生成日志文件，请检查上面的构建或启动错误。'
        }
        Write-Host '若日志提示已有实例，请先从托盘退出旧实例，再重新运行本脚本。'
        if ($exitCode -ne 0) {
            throw "color-picker exited with code $exitCode. See the startup log above."
        }
    }
    else {
        Write-Host "诊断进程已启动（PID $($process.Id)）。请尝试快捷键，再查看日志。"
        Write-Host '在另一个 PowerShell 窗口运行以下命令实时查看日志：'
        $quotedLogPath = $logPath.Replace("'", "''")
        Write-Host "Get-Content -LiteralPath '$quotedLogPath' -Encoding UTF8 -Wait"
    }
}
finally {
    # Disposing the local process handle does not stop the application.
    $process.Dispose()
}
