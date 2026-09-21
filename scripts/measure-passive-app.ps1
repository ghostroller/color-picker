<#
.SYNOPSIS
Passively measures an explicitly selected, already running diagnostic color-picker.exe.
.DESCRIPTION
Reads process counters and the application's existing read-only diagnostic queries.
It never starts or stops an application/session, changes settings, sends input,
captures the screen, or accesses the clipboard. Open the desired application state
yourself before running, or use WaitForStateSeconds to wait for it. Diagnostics must
already be enabled (--diagnostics or --log-file).

ExpectedState: 0 Idle, 1 Starting, 2 Live, 3 Frozen, 4 Finishing, 5 Result,
6 Settings, or -1 to permit mixed states. A mismatch during the measured interval
preserves the samples as NOT_COMPLETED. State checks occur at sample boundaries;
transitions between samples cannot be ruled out. COMPLETED describes collection,
not compliance with performance thresholds. The output file must not exist.
.EXAMPLE
.\scripts\measure-passive-app.ps1 -ProcessId 1234 -Label frozen-still -ExpectedState 3 -WaitForStateSeconds 60 -OutputPath .\logs\frozen.json
.EXAMPLE
.\scripts\measure-passive-app.ps1 -ProcessId 1234 -Label settings-still -ExpectedState 6 -OutputPath .\logs\settings.json
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidateRange(1, [int]::MaxValue)] [int] $ProcessId,
    [ValidateRange(1, 3600)] [int] $Seconds = 30,
    [Parameter(Mandatory)] [ValidateNotNullOrEmpty()] [string] $Label,
    [Parameter(Mandatory)] [ValidateRange(-1, 6)] [int] $ExpectedState,
    [ValidateRange(0, 3600)] [int] $WaitForStateSeconds = 0,
    [Parameter(Mandatory)] [string] $OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($env:OS -ne 'Windows_NT') { throw 'This measurement requires Windows.' }
$absoluteOutput = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
# Reserve a new evidence file before querying the application; never overwrite one.
$outputStream = [IO.File]::Open($absoluteOutput, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
$report = [ordered]@{
    schema_version = 1
    status = 'NOT_COMPLETED'
    label = $Label
    started_utc = [DateTime]::UtcNow.ToString('o')
    measurement_started_utc = $null
    finished_utc = $null
    requested = @{ process_id = $ProcessId; seconds = $Seconds; expected_state = $ExpectedState; wait_for_state_seconds = $WaitForStateSeconds }
    scope = @(
        'Passive observation of one existing diagnostic process; no application/session activation, cancellation or termination.',
        'No synthetic input, screen capture, cursor reads, clipboard access or configuration changes.',
        'CPU is process CPU time / elapsed wall time; machine CPU additionally divides by logical processor count.',
        'Approximately one sample per second. Read-only diagnostic messages add a small amount of target work.',
        'State/session reads are checked for transitions; state invariants are checked only at sample boundaries.',
        'Working set and private committed bytes are distinct counters and must not be added together.',
        'No GPU measurement. No process restart, working-set trim or system timer-resolution change.',
        'COMPLETED means collection completed, not that performance thresholds passed.'
    )
    metadata = [ordered]@{ process_id = $ProcessId }
    wait_observations = [Collections.Generic.List[object]]::new()
    samples = [Collections.Generic.List[object]]::new()
    summary = $null
    cleanup = [ordered]@{ completed = $false; action = 'release_read_only_query_handle'; error = $null }
    error = $null
}
$script:targetProcess = $null
$script:queryHandle = [IntPtr]::Zero
$script:hostWindow = [IntPtr]::Zero
$script:processStartTicks = [long]0
$script:logicalProcessors = 1
$script:measurementClock = [Diagnostics.Stopwatch]::new()

$nativeSource = @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

namespace ColorPicker.PassiveMeasurementV1 {
    public static class Native {
        private delegate bool EnumProc(IntPtr hwnd, IntPtr parameter);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool EnumWindows(EnumProc callback, IntPtr parameter);
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern int GetClassNameW(IntPtr hwnd, StringBuilder name, int capacity);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern IntPtr SendMessageTimeoutW(IntPtr hwnd, uint message, UIntPtr wparam,
            IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr OpenProcess(uint access, bool inherit, uint processId);
        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool QueryFullProcessImageNameW(IntPtr process, uint flags,
            StringBuilder path, ref uint size);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern uint GetGuiResources(IntPtr process, uint flags);
        [DllImport("kernel32.dll")]
        private static extern void SetLastError(uint error);

        public static IntPtr[] FindWindows(uint processId, string className) {
            var matches = new List<IntPtr>();
            EnumProc callback = delegate(IntPtr hwnd, IntPtr ignored) {
                uint owner;
                GetWindowThreadProcessId(hwnd, out owner);
                if (owner == processId) {
                    var name = new StringBuilder(256);
                    if (GetClassNameW(hwnd, name, name.Capacity) > 0 && name.ToString() == className)
                        matches.Add(hwnd);
                }
                return true;
            };
            if (!EnumWindows(callback, IntPtr.Zero)) throw new Win32Exception();
            return matches.ToArray();
        }
        public static long Query(IntPtr hwnd, uint processId, uint selector) {
            uint owner;
            if (GetWindowThreadProcessId(hwnd, out owner) == 0 || owner != processId)
                throw new InvalidOperationException("Diagnostic window no longer belongs to the target process.");
            UIntPtr result;
            SetLastError(0);
            // WM_APP + 3 is the existing read-only diagnostic query.
            // BLOCK | ABORTIFHUNG | ERRORONEXIT; never an unlimited synchronous send.
            if (SendMessageTimeoutW(hwnd, 0x8003, new UIntPtr(selector), IntPtr.Zero,
                    0x23, 250, out result) == IntPtr.Zero) {
                int error = Marshal.GetLastWin32Error();
                throw new Win32Exception(error == 0 ? 1460 : error, "Diagnostic query failed or timed out.");
            }
            return unchecked((long)result.ToUInt64());
        }
        public static IntPtr OpenQueryHandle(uint processId) {
            // PROCESS_QUERY_INFORMATION is read-only and required by GetGuiResources.
            IntPtr handle = OpenProcess(0x0400, false, processId);
            if (handle == IntPtr.Zero) throw new Win32Exception();
            return handle;
        }
        public static string ImagePath(IntPtr handle) {
            uint capacity = 32768;
            var path = new StringBuilder((int)capacity);
            if (!QueryFullProcessImageNameW(handle, 0, path, ref capacity)) throw new Win32Exception();
            return path.ToString();
        }
        public static uint GuiCount(IntPtr handle, uint kind) {
            SetLastError(0);
            uint count = GetGuiResources(handle, kind);
            int error = Marshal.GetLastWin32Error();
            if (count == 0 && error != 0) throw new Win32Exception(error);
            return count;
        }
    }
}
'@

function Assert-ProcessIdentity {
    $script:targetProcess.Refresh()
    if ($script:targetProcess.HasExited -or $script:targetProcess.StartTime.ToUniversalTime().Ticks -ne $script:processStartTicks) {
        throw 'The target process exited or its identity changed.'
    }
}

function Get-PickerDiagnostics {
    # Retry state/session reads that straddle a transition; individual messages
    # do not constitute an atomic snapshot of all diagnostic counters.
    for ($attempt = 0; $attempt -lt 4; $attempt++) {
        $state = [ColorPicker.PassiveMeasurementV1.Native]::Query($script:hostWindow, [uint32]$ProcessId, 9)
        $session = [ColorPicker.PassiveMeasurementV1.Native]::Query($script:hostWindow, [uint32]$ProcessId, 8)
        $value = [pscustomobject][ordered]@{
            ready = [ColorPicker.PassiveMeasurementV1.Native]::Query($script:hostWindow, [uint32]$ProcessId, 0)
            active = [ColorPicker.PassiveMeasurementV1.Native]::Query($script:hostWindow, [uint32]$ProcessId, 5)
            timer = [ColorPicker.PassiveMeasurementV1.Native]::Query($script:hostWindow, [uint32]$ProcessId, 6)
            samples = [ColorPicker.PassiveMeasurementV1.Native]::Query($script:hostWindow, [uint32]$ProcessId, 7)
            session = $session
            state = $state
        }
        $endSession = [ColorPicker.PassiveMeasurementV1.Native]::Query($script:hostWindow, [uint32]$ProcessId, 8)
        $endState = [ColorPicker.PassiveMeasurementV1.Native]::Query($script:hostWindow, [uint32]$ProcessId, 9)
        if ($state -eq $endState -and $session -eq $endSession) {
            if ($value.ready -ne 1) { throw 'Diagnostics are disabled or the application is not ready.' }
            return $value
        }
    }
    throw 'Application state changed repeatedly during diagnostic queries.'
}

function Get-ResourceSample($Previous) {
    Assert-ProcessIdentity
    $cpu = $script:targetProcess.TotalProcessorTime.TotalMilliseconds
    $sampleTime = $script:measurementClock.Elapsed.TotalSeconds
    $sample = [pscustomobject][ordered]@{
        utc = [DateTime]::UtcNow.ToString('o')
        elapsed_seconds = $sampleTime
        cpu_total_ms = $cpu
        cpu_user_ms = $script:targetProcess.UserProcessorTime.TotalMilliseconds
        cpu_kernel_ms = $script:targetProcess.PrivilegedProcessorTime.TotalMilliseconds
        cpu_delta_ms = $null
        interval_seconds = $null
        cpu_single_core_percent = $null
        cpu_machine_percent = $null
        working_set_bytes = $script:targetProcess.WorkingSet64
        private_bytes = $script:targetProcess.PrivateMemorySize64
        handles = $script:targetProcess.HandleCount
        threads = $script:targetProcess.Threads.Count
        gdi_objects = [ColorPicker.PassiveMeasurementV1.Native]::GuiCount($script:queryHandle, 0)
        user_objects = [ColorPicker.PassiveMeasurementV1.Native]::GuiCount($script:queryHandle, 1)
        diagnostics = $null
        sample_attempts_delta = $null
        sample_attempts_per_second = $null
    }
    # Retain the resource snapshot even if the following diagnostic read fails.
    $report.samples.Add($sample)
    $sample.diagnostics = Get-PickerDiagnostics
    if ($null -ne $Previous) {
        $elapsed = $sampleTime - $Previous.elapsed_seconds
        $cpuDelta = $cpu - $Previous.cpu_total_ms
        if ($elapsed -le 0 -or $cpuDelta -lt 0) { throw 'Invalid process CPU time delta.' }
        $sample.interval_seconds = $elapsed
        $sample.cpu_delta_ms = $cpuDelta
        $sample.cpu_single_core_percent = $cpuDelta / ($elapsed * 10.0)
        $sample.cpu_machine_percent = $sample.cpu_single_core_percent / $script:logicalProcessors
        $sample.sample_attempts_delta = $sample.diagnostics.samples - $Previous.diagnostics.samples
        $sample.sample_attempts_per_second = $sample.sample_attempts_delta / $elapsed
    }
    return $sample
}

function Set-MeasurementSummary {
    if ($report.samples.Count -lt 2) { return }
    $first = $report.samples[0]
    $last = $report.samples[$report.samples.Count - 1]
    $duration = $last.elapsed_seconds - $first.elapsed_seconds
    if ($duration -le 0) { return }
    $cpuDelta = $last.cpu_total_ms - $first.cpu_total_ms
    $attempts = if ($null -ne $first.diagnostics -and $null -ne $last.diagnostics) { $last.diagnostics.samples - $first.diagnostics.samples } else { $null }
    $report.summary = [ordered]@{
        duration_seconds = $duration
        sample_count = $report.samples.Count
        cpu_delta_ms = $cpuDelta
        cpu_user_delta_ms = $last.cpu_user_ms - $first.cpu_user_ms
        cpu_kernel_delta_ms = $last.cpu_kernel_ms - $first.cpu_kernel_ms
        cpu_single_core_percent = $cpuDelta / ($duration * 10.0)
        cpu_machine_percent = $cpuDelta / ($duration * 10.0 * $script:logicalProcessors)
        sample_attempts_delta = $attempts
        sample_attempts_per_second = if ($null -ne $attempts) { $attempts / $duration } else { $null }
        working_set_delta_bytes = $last.working_set_bytes - $first.working_set_bytes
        private_delta_bytes = $last.private_bytes - $first.private_bytes
        handles_delta = $last.handles - $first.handles
        threads_delta = $last.threads - $first.threads
        gdi_objects_delta = [long]$last.gdi_objects - [long]$first.gdi_objects
        user_objects_delta = [long]$last.user_objects - [long]$first.user_objects
    }
}

try {
    if (-not ('ColorPicker.PassiveMeasurementV1.Native' -as [type])) { Add-Type -TypeDefinition $nativeSource }
    $script:targetProcess = [Diagnostics.Process]::GetProcessById($ProcessId)
    $script:processStartTicks = $script:targetProcess.StartTime.ToUniversalTime().Ticks
    $script:queryHandle = [ColorPicker.PassiveMeasurementV1.Native]::OpenQueryHandle([uint32]$ProcessId)
    $exe = [ColorPicker.PassiveMeasurementV1.Native]::ImagePath($script:queryHandle)
    if ([IO.Path]::GetFileName($exe) -ine 'color-picker.exe') { throw "Expected color-picker.exe; target is $exe" }
    $report.metadata.process_start_utc = $script:targetProcess.StartTime.ToUniversalTime().ToString('o')
    $report.metadata.executable = $exe
    $report.metadata.executable_sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
    $report.metadata.executable_file_version = [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).FileVersion
    $report.metadata.machine = [Environment]::MachineName
    $report.metadata.os_version = [Environment]::OSVersion.VersionString
    $report.metadata.powershell_version = $PSVersionTable.PSVersion.ToString()
    $script:logicalProcessors = [Environment]::ProcessorCount
    $report.metadata.logical_processor_source = 'Environment.ProcessorCount'
    try {
        $machine = Get-CimInstance -ClassName Win32_ComputerSystem
        $script:logicalProcessors = [int]$machine.NumberOfLogicalProcessors
        $report.metadata.logical_processor_source = 'Win32_ComputerSystem.NumberOfLogicalProcessors'
        $report.metadata.cpu_model = @((Get-CimInstance -ClassName Win32_Processor).Name)
    }
    catch { $report.metadata.cpu_metadata_warning = $_.Exception.Message }
    if ($script:logicalProcessors -lt 1) { throw 'Logical processor count is unavailable.' }
    $report.metadata.logical_processors = $script:logicalProcessors
    $report.metadata.executable_note = 'Path/hash identify the running binary; file version and repository HEAD do not independently prove its build profile or source revision.'
    $repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
    $report.metadata.source_repository = $repository
    $report.metadata.source_git_head = $null
    $report.metadata.source_worktree_dirty = $null
    try {
        $revision = & git -C $repository rev-parse HEAD 2>$null
        if ($LASTEXITCODE -ne 0) { throw 'git rev-parse failed.' }
        $report.metadata.source_git_head = [string]$revision
        $changes = @(& git -C $repository status --porcelain 2>$null)
        if ($LASTEXITCODE -ne 0) { throw 'git status failed.' }
        $report.metadata.source_worktree_dirty = $changes.Count -gt 0
    }
    catch { $report.metadata.git_metadata_warning = $_.Exception.Message }
    $windows = @([ColorPicker.PassiveMeasurementV1.Native]::FindWindows([uint32]$ProcessId, 'ColorPicker.Host.v1'))
    if ($windows.Count -ne 1) { throw "Expected exactly one ColorPicker.Host.v1 window for PID $ProcessId; found $($windows.Count)." }
    $script:hostWindow = $windows[0]
    $report.metadata.host_hwnd = $script:hostWindow.ToInt64()
    $waitClock = [Diagnostics.Stopwatch]::StartNew()
    do {
        Assert-ProcessIdentity
        $diag = Get-PickerDiagnostics
        $report.wait_observations.Add([pscustomobject]@{ utc = [DateTime]::UtcNow.ToString('o'); elapsed_seconds = $waitClock.Elapsed.TotalSeconds; diagnostics = $diag })
        if ($ExpectedState -eq -1 -or $diag.state -eq $ExpectedState) { break }
        if ($waitClock.Elapsed.TotalSeconds -ge $WaitForStateSeconds) {
            throw "Expected state=$ExpectedState; found state=$($diag.state), session=$($diag.session), timer=$($diag.timer). The application was left unchanged."
        }
        [Threading.Thread]::Sleep([int][Math]::Min(1000, [Math]::Max(1, ($WaitForStateSeconds - $waitClock.Elapsed.TotalSeconds) * 1000)))
    } while ($true)

    Write-Host "Passively measuring PID $ProcessId, label=$Label, expected state=$ExpectedState, for $Seconds seconds."
    $report.measurement_started_utc = [DateTime]::UtcNow.ToString('o')
    $script:measurementClock.Restart()
    $previous = $null
    for ($index = 0; $index -le $Seconds; $index++) {
        $remaining = $index - $script:measurementClock.Elapsed.TotalSeconds
        if ($remaining -gt 0) { [Threading.Thread]::Sleep([int][Math]::Ceiling($remaining * 1000)) }
        $sample = Get-ResourceSample $previous
        if ($ExpectedState -ne -1 -and $sample.diagnostics.state -ne $ExpectedState) {
            throw "Interrupted $Label : expected state=$ExpectedState, found state=$($sample.diagnostics.state), session=$($sample.diagnostics.session), timer=$($sample.diagnostics.timer)."
        }
        $previous = $sample
    }
    $report.status = 'COMPLETED'
}
catch {
    $report.error = $_.Exception.Message
    Write-Warning "NOT_COMPLETED: $($report.error)"
}
finally {
    try {
        if ($script:queryHandle -ne [IntPtr]::Zero -and -not [ColorPicker.PassiveMeasurementV1.Native]::CloseHandle($script:queryHandle)) {
            throw 'CloseHandle failed for the read-only query handle.'
        }
        if ($null -ne $script:targetProcess) { $script:targetProcess.Dispose() }
        $report.cleanup.completed = $true
    }
    catch {
        $report.cleanup.error = $_.Exception.Message
        $report.status = 'NOT_COMPLETED'
    }
    $report.finished_utc = [DateTime]::UtcNow.ToString('o')
    try {
        Set-MeasurementSummary
        $json = $report | ConvertTo-Json -Depth 16
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes($json + [Environment]::NewLine)
        $outputStream.Write($bytes, 0, $bytes.Length)
        $outputStream.Flush()
    }
    finally { $outputStream.Dispose() }
    Write-Host "$($report.status): $absoluteOutput"
}

if ($report.status -ne 'COMPLETED') { throw "Measurement NOT_COMPLETED. See $absoluteOutput" }
