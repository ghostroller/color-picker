<#
.SYNOPSIS
Measures an explicitly selected, already running diagnostic color-picker.exe.
.DESCRIPTION
Keep the desktop unlocked and do not press mouse buttons, wheel, Esc or the picker
hotkey while this runs. The live phases use the application's real input hooks.
No process is launched or terminated, no input is synthesized, and no image or
clipboard data is read or written. The output file must not already exist.

Sequence: Idle -> activation/cancel cycles -> sustained Live -> Idle. Each stable
phase lasts Seconds; resource samples are taken approximately once per second.
COMPLETED means the measurement completed, not that performance thresholds passed.
.EXAMPLE
.\scripts\measure-running-app.ps1 -ProcessId 1234 -OutputPath .\logs\running-app.json
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidateRange(1, [int]::MaxValue)] [int] $ProcessId,
    [ValidateRange(1, 3600)] [int] $Seconds = 30,
    [ValidateRange(1, 1000)] [int] $ActivationCycles = 20,
    [Parameter(Mandatory)] [string] $OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($env:OS -ne 'Windows_NT') { throw 'This measurement requires Windows.' }
$absoluteOutput = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
# Reserve evidence before touching the application; never truncate an old report.
$outputStream = [IO.File]::Open($absoluteOutput, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
$report = [ordered]@{
    schema_version = 1
    status = 'NOT_COMPLETED'
    started_utc = [DateTime]::UtcNow.ToString('o')
    finished_utc = $null
    requested = @{ process_id = $ProcessId; seconds_per_stable_phase = $Seconds; activation_cycles = $ActivationCycles }
    scope = @(
        'Existing diagnostic application; no process launch/exit, synthetic input, screenshots or clipboard access.',
        'Actual session input hooks are active during Live; avoid input while measuring.',
        'Latency is PostMessage to queried Live + sample counter growth + visible preview with empty update region.',
        'Latency includes message delivery, polling and queries; it is not real-hotkey or display presentation latency.',
        'CPU and memory belong to the target process; diagnostic messages add a small amount of target work.',
        'State/session/timer invariants are observed at sample boundaries; cancellation rechecks the owned session.',
        'This does not test complete pick/zoom/copy workflows or establish performance threshold compliance.'
    )
    metadata = [ordered]@{ process_id = $ProcessId }
    phases = [Collections.Generic.List[object]]::new()
    latency = [ordered]@{ method = 'nearest-rank'; count = 0; p50_ms = $null; p95_ms = $null; raw = [Collections.Generic.List[object]]::new() }
    cleanup = [ordered]@{ completed = $false; action = 'not_needed'; owned_session = 0; error = $null }
    error = $null
}
$script:targetProcess = $null
$script:queryHandle = [IntPtr]::Zero
$script:hostWindow = [IntPtr]::Zero
$script:ownedSession = [long]0
$script:activationPending = $false
$script:processStartTicks = [long]0
$script:logicalProcessors = 1
$script:measurementClock = [Diagnostics.Stopwatch]::StartNew()
$script:cyclePhase = $null
$script:cycleSampleClock = [Diagnostics.Stopwatch]::StartNew()
$script:lastCycleSample = $null
$script:nativeReady = $false

$nativeSource = @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

namespace ColorPicker.RunningMeasurementV1 {
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
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool PostMessageW(IntPtr hwnd, uint message, UIntPtr wparam, IntPtr lparam);
        [DllImport("user32.dll")]
        private static extern bool IsWindowVisible(IntPtr hwnd);
        [DllImport("user32.dll")]
        private static extern bool GetUpdateRect(IntPtr hwnd, IntPtr rect, bool erase);
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
        [StructLayout(LayoutKind.Sequential)]
        public struct Point { public int X; public int Y; }
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool GetCursorPos(out Point point);

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
        public static long Query(IntPtr hwnd, uint selector) {
            UIntPtr result;
            SetLastError(0);
            // BLOCK | ABORTIFHUNG | ERRORONEXIT; no unlimited synchronous sends.
            if (SendMessageTimeoutW(hwnd, 0x8003, new UIntPtr(selector), IntPtr.Zero,
                    0x23, 250, out result) == IntPtr.Zero) {
                int error = Marshal.GetLastWin32Error();
                throw new Win32Exception(error == 0 ? 1460 : error, "Diagnostic query failed or timed out.");
            }
            return unchecked((long)result.ToUInt64());
        }
        public static void Post(IntPtr hwnd, uint message, ulong session) {
            if (!PostMessageW(hwnd, message, new UIntPtr(session), IntPtr.Zero))
                throw new Win32Exception();
        }
        public static IntPtr ReadyPreview(uint processId) {
            foreach (IntPtr hwnd in FindWindows(processId, "ColorPicker.Preview.v1"))
                if (IsWindowVisible(hwnd) && !GetUpdateRect(hwnd, IntPtr.Zero, false)) return hwnd;
            return IntPtr.Zero;
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
        public static Point Cursor() {
            Point point;
            if (!GetCursorPos(out point)) throw new Win32Exception();
            return point;
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
    # A transition can occur between individual diagnostic messages. Retry a torn
    # state/session read instead of interpreting it as a stable application state.
    for ($attempt = 0; $attempt -lt 4; $attempt++) {
        $state = [ColorPicker.RunningMeasurementV1.Native]::Query($script:hostWindow, 9)
        $session = [ColorPicker.RunningMeasurementV1.Native]::Query($script:hostWindow, 8)
        $value = [pscustomobject]@{
            ready = [ColorPicker.RunningMeasurementV1.Native]::Query($script:hostWindow, 0)
            active = [ColorPicker.RunningMeasurementV1.Native]::Query($script:hostWindow, 5)
            timer = [ColorPicker.RunningMeasurementV1.Native]::Query($script:hostWindow, 6)
            samples = [ColorPicker.RunningMeasurementV1.Native]::Query($script:hostWindow, 7)
            session = $session
            state = $state
        }
        $endSession = [ColorPicker.RunningMeasurementV1.Native]::Query($script:hostWindow, 8)
        $endState = [ColorPicker.RunningMeasurementV1.Native]::Query($script:hostWindow, 9)
        if ($state -eq $endState -and $session -eq $endSession) {
            if ($value.ready -ne 1) { throw 'Diagnostics are disabled or the application is not ready.' }
            return $value
        }
    }
    throw 'Application state changed repeatedly during diagnostic queries.'
}

function Assert-Idle($Diagnostics) {
    if ($Diagnostics.state -ne 0 -or $Diagnostics.active -ne 0 -or $Diagnostics.session -ne 0 -or $Diagnostics.timer -ne 0) {
        throw "Expected Idle; found state=$($Diagnostics.state), session=$($Diagnostics.session), active=$($Diagnostics.active), timer=$($Diagnostics.timer). No user Result/Settings window will be closed."
    }
}

function Get-ResourceSample($Previous) {
    Assert-ProcessIdentity
    $cpu = $script:targetProcess.TotalProcessorTime.TotalMilliseconds
    $sampleTime = $script:measurementClock.Elapsed.TotalSeconds
    $cursor = [ColorPicker.RunningMeasurementV1.Native]::Cursor()
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
        gdi_objects = [ColorPicker.RunningMeasurementV1.Native]::GuiCount($script:queryHandle, 0)
        user_objects = [ColorPicker.RunningMeasurementV1.Native]::GuiCount($script:queryHandle, 1)
        cursor_x = $cursor.X
        cursor_y = $cursor.Y
        diagnostics = Get-PickerDiagnostics
        sample_attempts_delta = $null
        sample_attempts_per_second = $null
    }
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

function New-Phase([string] $Name) {
    $phase = [ordered]@{
        name = $Name; status = 'NOT_COMPLETED'; started_utc = [DateTime]::UtcNow.ToString('o')
        samples = [Collections.Generic.List[object]]::new(); summary = $null
    }
    $report.phases.Add($phase)
    return $phase
}

function Complete-Phase($Phase) {
    $first = $Phase.samples[0]
    $last = $Phase.samples[$Phase.samples.Count - 1]
    $duration = $last.elapsed_seconds - $first.elapsed_seconds
    $cpuDelta = $last.cpu_total_ms - $first.cpu_total_ms
    $attempts = $last.diagnostics.samples - $first.diagnostics.samples
    $singleCore = if ($duration -gt 0) { $cpuDelta / ($duration * 10.0) } else { $null }
    $Phase.summary = [ordered]@{
        duration_seconds = $duration; cpu_delta_ms = $cpuDelta
        cpu_user_delta_ms = $last.cpu_user_ms - $first.cpu_user_ms
        cpu_kernel_delta_ms = $last.cpu_kernel_ms - $first.cpu_kernel_ms
        cpu_single_core_percent = $singleCore
        cpu_machine_percent = if ($null -ne $singleCore) { $singleCore / $script:logicalProcessors } else { $null }
        sample_attempts_delta = $attempts
        sample_attempts_per_second = if ($duration -gt 0) { $attempts / $duration } else { $null }
        working_set_delta_bytes = $last.working_set_bytes - $first.working_set_bytes
        private_delta_bytes = $last.private_bytes - $first.private_bytes
        handles_delta = $last.handles - $first.handles
        threads_delta = $last.threads - $first.threads
        gdi_objects_delta = [long]$last.gdi_objects - [long]$first.gdi_objects
        user_objects_delta = [long]$last.user_objects - [long]$first.user_objects
    }
    $Phase.status = 'COMPLETED'
}

function Measure-StablePhase([string] $Name, [long] $ExpectedState, [long] $ExpectedSession, [long] $ExpectedTimer) {
    Write-Host "$Name : $Seconds seconds"
    $phase = New-Phase $Name
    $phase.expected = @{ state = $ExpectedState; session = $ExpectedSession; timer = $ExpectedTimer }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $previous = $null
    for ($index = 0; $index -le $Seconds; $index++) {
        $remaining = $index - $clock.Elapsed.TotalSeconds
        if ($remaining -gt 0) { [Threading.Thread]::Sleep([int][Math]::Ceiling($remaining * 1000)) }
        $sample = Get-ResourceSample $previous
        $phase.samples.Add($sample)
        $diag = $sample.diagnostics
        if ($diag.state -ne $ExpectedState -or $diag.session -ne $ExpectedSession -or $diag.timer -ne $ExpectedTimer -or
            $diag.active -ne [int]($ExpectedState -eq 2)) {
            throw "Interrupted $Name : state/session/timer changed (state=$($diag.state), session=$($diag.session), timer=$($diag.timer))."
        }
        if ($null -ne $previous -and $ExpectedState -eq 0 -and $sample.sample_attempts_delta -ne 0) {
            throw "Interrupted $Name : sampling occurred during the Idle interval."
        }
        $previous = $sample
    }
    Complete-Phase $phase
}

function Add-CycleSample([switch] $Force) {
    if ($null -ne $script:cyclePhase -and ($Force -or $script:cycleSampleClock.ElapsedMilliseconds -ge 1000)) {
        $sample = Get-ResourceSample $script:lastCycleSample
        $script:cyclePhase.samples.Add($sample)
        $script:lastCycleSample = $sample
        $script:cycleSampleClock.Restart()
    }
}

function Start-OwnedSession {
    Assert-ProcessIdentity
    $baseline = Get-PickerDiagnostics
    Assert-Idle $baseline
    $script:ownedSession = 0
    $clock = [Diagnostics.Stopwatch]::StartNew()
    [ColorPicker.RunningMeasurementV1.Native]::Post($script:hostWindow, 0x8001, 0)
    $script:activationPending = $true
    while ($clock.Elapsed.TotalSeconds -lt 10) {
        $diag = Get-PickerDiagnostics
        if ($diag.session -ne 0) {
            if ($script:ownedSession -eq 0) {
                $script:ownedSession = $diag.session
                $script:activationPending = $false
            }
            if ($diag.session -ne $script:ownedSession) { throw 'Interrupted activation: the session changed.' }
        }
        if ($diag.state -notin @(0, 1, 2) -or ($script:ownedSession -ne 0 -and $diag.state -eq 0)) {
            throw "Interrupted activation: state=$($diag.state), session=$($diag.session)."
        }
        if ($diag.state -eq 2 -and $diag.active -eq 1 -and $diag.timer -ne 0 -and $diag.samples -gt $baseline.samples) {
            $preview = [ColorPicker.RunningMeasurementV1.Native]::ReadyPreview([uint32]$ProcessId)
            if ($preview -ne [IntPtr]::Zero) {
                $confirmed = Get-PickerDiagnostics
                if ($confirmed.state -eq 2 -and $confirmed.session -eq $script:ownedSession -and $confirmed.timer -eq $diag.timer) {
                    return [pscustomobject]@{
                        elapsed_ms = $clock.Elapsed.TotalMilliseconds
                        session = $script:ownedSession; timer = $diag.timer
                        sample_attempts_before = $baseline.samples; sample_attempts_ready = $confirmed.samples
                        preview_hwnd = $preview.ToInt64()
                    }
                }
            }
        }
        Add-CycleSample
        [Threading.Thread]::Sleep(1)
    }
    throw 'Activation did not reach queryable Live/preview readiness within 10 seconds.'
}

function Stop-OwnedSession {
    if ($script:ownedSession -eq 0) { return }
    Assert-ProcessIdentity
    $diag = Get-PickerDiagnostics
    if ($diag.state -eq 0 -and $diag.session -eq 0) {
        $script:ownedSession = 0
        return
    }
    if ($diag.session -ne $script:ownedSession -or $diag.active -ne 1) {
        throw "Owned session is no longer active; preserving current state=$($diag.state), session=$($diag.session)."
    }
    # Current host versions do not enforce this wParam. Recheck immediately before
    # posting; the report does not claim atomic protection against concurrent input.
    $confirmed = Get-PickerDiagnostics
    if ($confirmed.session -ne $script:ownedSession -or $confirmed.active -ne 1) {
        throw 'Session changed before cancellation; no cancel message was sent.'
    }
    [ColorPicker.RunningMeasurementV1.Native]::Post($script:hostWindow, 0x8004, [uint64]$script:ownedSession)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.Elapsed.TotalSeconds -lt 10) {
        $diag = Get-PickerDiagnostics
        if ($diag.state -eq 0) {
            Assert-Idle $diag
            $script:ownedSession = 0
            return
        }
        if ($diag.session -ne $script:ownedSession -or $diag.active -ne 1) {
            throw "Interrupted cancellation: preserving state=$($diag.state), session=$($diag.session)."
        }
        Add-CycleSample
        [Threading.Thread]::Sleep(1)
    }
    throw 'Owned session did not return to Idle within 10 seconds.'
}

try {
    if (-not ('ColorPicker.RunningMeasurementV1.Native' -as [type])) { Add-Type -TypeDefinition $nativeSource }
    $script:nativeReady = $true
    $script:targetProcess = [Diagnostics.Process]::GetProcessById($ProcessId)
    $script:processStartTicks = $script:targetProcess.StartTime.ToUniversalTime().Ticks
    $script:queryHandle = [ColorPicker.RunningMeasurementV1.Native]::OpenQueryHandle([uint32]$ProcessId)
    $exe = [ColorPicker.RunningMeasurementV1.Native]::ImagePath($script:queryHandle)
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
    $report.metadata.source_note = 'Repository HEAD is the source checkout at measurement time, not proof of the executable build revision.'
    $windows = @([ColorPicker.RunningMeasurementV1.Native]::FindWindows([uint32]$ProcessId, 'ColorPicker.Host.v1'))
    if ($windows.Count -ne 1) { throw "Expected exactly one ColorPicker.Host.v1 window for PID $ProcessId; found $($windows.Count)." }
    $script:hostWindow = $windows[0]
    $report.metadata.host_hwnd = $script:hostWindow.ToInt64()
    $initial = Get-PickerDiagnostics
    $report.metadata.initial_diagnostics = $initial
    Assert-Idle $initial
    Write-Host "Measuring existing PID $ProcessId. Avoid mouse buttons/wheel, Esc and picker hotkeys until finished."

    Measure-StablePhase 'idle_before' 0 0 0
    $script:cyclePhase = New-Phase 'activation_cycles'
    Add-CycleSample -Force
    Write-Host "activation_cycles : $ActivationCycles cycles"
    for ($cycle = 1; $cycle -le $ActivationCycles; $cycle++) {
        $latency = Start-OwnedSession
        $report.latency.raw.Add([pscustomobject]@{ cycle = $cycle; readiness = $latency; cancelled = $false })
        Stop-OwnedSession
        $report.latency.raw[$report.latency.raw.Count - 1].cancelled = $true
        Add-CycleSample
    }
    Add-CycleSample -Force
    Complete-Phase $script:cyclePhase
    $script:cyclePhase = $null
    $live = Start-OwnedSession
    Measure-StablePhase 'live' 2 $live.session $live.timer
    Stop-OwnedSession
    Measure-StablePhase 'idle_after' 0 0 0
    $report.status = 'COMPLETED'
}
catch {
    $report.error = $_.Exception.Message
    Write-Warning "NOT_COMPLETED: $($report.error)"
}
finally {
    # Disable phase sampling during cleanup: partial evidence remains untouched.
    $script:cyclePhase = $null
    $report.cleanup.owned_session = $script:ownedSession
    try {
        if ($script:nativeReady -and $script:ownedSession -ne 0) {
            $report.cleanup.action = 'cancel_owned_session_if_still_active'
            Stop-OwnedSession
        }
        if ($script:activationPending) {
            # No session was ever observed. Do not guess ownership or send a broad
            # cancel: an unresponsive host may still have the activation queued.
            throw 'Activation was posted but no owned session was observed; pending delivery cannot be ruled out. No unowned session was cancelled.'
        }
        $report.cleanup.completed = $true
    }
    catch {
        $report.cleanup.error = $_.Exception.Message
        $report.status = 'NOT_COMPLETED'
        Write-Warning "Cleanup: $($report.cleanup.error)"
    }
    if ($script:queryHandle -ne [IntPtr]::Zero) {
        if (-not [ColorPicker.RunningMeasurementV1.Native]::CloseHandle($script:queryHandle)) {
            $report.cleanup.error = 'CloseHandle failed for the read-only query handle.'
            $report.cleanup.completed = $false
            $report.status = 'NOT_COMPLETED'
        }
    }
    if ($null -ne $script:targetProcess) { $script:targetProcess.Dispose() }
    $successful = @($report.latency.raw | Where-Object { $_.cancelled } | ForEach-Object { $_.readiness.elapsed_ms } | Sort-Object)
    $report.latency.count = $successful.Count
    if ($successful.Count -gt 0) {
        $report.latency.p50_ms = $successful[[Math]::Ceiling($successful.Count * 0.50) - 1]
        $report.latency.p95_ms = $successful[[Math]::Ceiling($successful.Count * 0.95) - 1]
    }
    $report.finished_utc = [DateTime]::UtcNow.ToString('o')
    try {
        $json = $report | ConvertTo-Json -Depth 16
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes($json + [Environment]::NewLine)
        $outputStream.Write($bytes, 0, $bytes.Length)
        $outputStream.Flush()
    }
    finally { $outputStream.Dispose() }
    Write-Host "$($report.status): $absoluteOutput"
}

if ($report.status -ne 'COMPLETED') { throw "Measurement NOT_COMPLETED. See $absoluteOutput" }
