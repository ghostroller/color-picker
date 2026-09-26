//! Opt-in resource measurement of start -> first Live frame -> cancel -> Idle.
//! Run only on an unlocked, stable desktop without touching mouse/buttons/Esc.

#[cfg(not(windows))]
fn main() {
    eprintln!("resource-probe currently supports Windows only");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    if let Err(error) = probe::main() {
        eprintln!("resource-probe: {error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod probe {
    use std::{
        error::Error as StdError,
        fs::{File, OpenOptions},
        io::{self, Write},
        mem::size_of,
        os::windows::io::AsRawHandle,
        path::PathBuf,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use color_picker::{
        app::controller::PreviewController,
        core::state::AppState,
        platform::windows::{
            input::WM_INPUT_WAKE,
            instance::{InstanceStatus, SingleInstance},
        },
    };
    use serde::Serialize;
    use windows::{
        Win32::{
            Foundation::{
                CloseHandle, ERROR_NO_MORE_FILES, ERROR_SUCCESS, FILETIME, GetLastError, HANDLE,
                HWND, SetLastError, WAIT_FAILED,
            },
            Graphics::Gdi::{GetUpdateRect, UpdateWindow},
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                    Thread32Next,
                },
                ProcessStatus::{
                    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
                },
                RemoteDesktop::{
                    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
                    WTSUnRegisterSessionNotification,
                },
                Threading::{
                    GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetCurrentProcessId,
                    GetCurrentThreadId, GetGuiResources, GetProcessHandleCount, GetProcessTimes,
                },
            },
            UI::{
                HiDpi::{
                    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                    SetThreadDpiAwarenessContext,
                },
                WindowsAndMessaging::*,
            },
        },
        core::{Error, HRESULT, w},
    };

    type ProbeResult<T> = Result<T, Box<dyn StdError>>;
    const WARMUP_CYCLES: u32 = 20;
    const START_LIMIT: Duration = Duration::from_secs(10);
    const CLEANUP_LIMIT: Duration = Duration::from_secs(10);

    #[derive(Serialize)]
    struct Parameters {
        cycles: u32,
        warmup_cycles: u32,
        idle_seconds: u64,
    }

    struct Arguments {
        parameters: Parameters,
        output: Option<PathBuf>,
    }

    #[derive(Serialize)]
    struct Report {
        schema_version: u32,
        status: &'static str,
        error: Option<String>,
        cleanup_error: Option<String>,
        cleanup_completed: bool,
        package_version: &'static str,
        pinned_toolchain: &'static str,
        executable: String,
        architecture: &'static str,
        debug_assertions: bool,
        started_unix_ms: u64,
        elapsed_ms: f64,
        parameters: Parameters,
        scope: &'static str,
        not_measured: [&'static str; 5],
        warmup_completed: u32,
        cycles_completed: u32,
        cold_idle: Option<IdleMeasurement>,
        after_warmup: Option<Resources>,
        checkpoints: Vec<Checkpoint>,
        used_idle: Option<IdleMeasurement>,
        activation: Latencies,
    }

    #[derive(Serialize, Default)]
    struct Latencies {
        definition: &'static str,
        sample_count: usize,
        p50_ms: Option<f64>,
        p95_ms: Option<f64>,
        samples_ms: Vec<f64>,
    }

    impl Latencies {
        fn summarize(&mut self) {
            self.sample_count = self.samples_ms.len();
            if !self.samples_ms.is_empty() {
                let mut sorted = self.samples_ms.clone();
                sorted.sort_by(f64::total_cmp);
                let rank = |percent: usize| (sorted.len() * percent).div_ceil(100) - 1;
                self.p50_ms = Some(sorted[rank(50)]);
                self.p95_ms = Some(sorted[rank(95)]);
            }
        }
    }

    #[derive(Serialize)]
    struct Resources {
        working_set_bytes: usize,
        private_commit_bytes: usize,
        thread_count: u32,
        handle_count: u32,
        gdi_objects: u32,
        user_objects: u32,
        process_kernel_100ns: u64,
        process_user_100ns: u64,
        controller_idle: bool,
        active_timer_id: usize,
        input_worker_attached: bool,
    }

    #[derive(Serialize)]
    struct IdleMeasurement {
        before: Resources,
        after: Resources,
        wall_ms: f64,
        cpu_kernel_ms: f64,
        cpu_user_ms: f64,
        cpu_percent_of_one_core: f64,
    }

    #[derive(Serialize)]
    struct Checkpoint {
        completed_cycles: u32,
        resources: Resources,
    }

    pub fn main() -> ProbeResult<()> {
        let Some(arguments) = arguments()? else {
            return Ok(());
        };
        // Open before baselines, so this output handle is constant throughout
        // the measurements. Refuse to silently overwrite an earlier report.
        let mut output = match arguments.output.as_ref() {
            Some(path) => Some(OpenOptions::new().write(true).create_new(true).open(path)?),
            None => None,
        };
        let started = Instant::now();
        let mut report = Report {
            schema_version: 1,
            status: "NOT_COMPLETED",
            error: None,
            cleanup_error: None,
            cleanup_completed: true,
            package_version: env!("CARGO_PKG_VERSION"),
            pinned_toolchain: include_str!("../rust-toolchain.toml").trim(),
            executable: std::env::current_exe()?.display().to_string(),
            architecture: std::env::consts::ARCH,
            debug_assertions: cfg!(debug_assertions),
            started_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
            elapsed_ms: 0.0,
            parameters: arguments.parameters,
            scope: "This-process, this-machine controlled start -> one Live preview frame -> cancel -> joined Idle. Includes measurement-harness overhead; no tray or registered hotkey.",
            not_measured: [
                "physical input gesture correctness or hotkey event delivery",
                "frozen magnifier idle resources or freeze latency",
                "result/settings windows and clipboard copy flows",
                "actual on-screen presentation latency or universal performance guarantees",
                "complete 1000-use workflow or clean-machine deployment acceptance",
            ],
            warmup_completed: 0,
            cycles_completed: 0,
            cold_idle: None,
            after_warmup: None,
            checkpoints: Vec::new(),
            used_idle: None,
            activation: Latencies {
                definition: "Elapsed from PreviewController::start to its first successful Live sample and synchronous preview UpdateWindow submission; nearest-rank percentiles, excluding warmup. Not input-to-visible-screen latency.",
                ..Default::default()
            },
        };
        eprintln!(
            "Controlled desktop probe: release mouse buttons and Esc, then leave input untouched. Short sessions briefly install real hooks and display a preview."
        );
        let outcome = execute(&mut report);
        if let Err(error) = outcome {
            report.error = Some(error.to_string());
        }
        if report.error.is_none() && report.cleanup_error.is_none() && report.cleanup_completed {
            report.status = "COMPLETED";
        }
        report.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        report.activation.summarize();
        write_report(output.as_mut(), &report)?;
        if report.status != "COMPLETED" {
            return Err(failure(
                "NOT_COMPLETED; inspect error and cleanup fields in the JSON report",
            ));
        }
        Ok(())
    }

    fn execute(report: &mut Report) -> ProbeResult<()> {
        let _instance = match SingleInstance::acquire()? {
            InstanceStatus::Primary(guard) => guard,
            InstanceStatus::Existing => {
                return Err(failure(
                    "An existing color-picker instance is running. Exit it through its tray menu before this probe.",
                ));
            }
        };
        let _dpi = ScopedPmv2::enter()?;
        let host = ProbeHost::new()?;
        let mut controller = PreviewController::new(host.0);
        let outcome = (|| {
            eprintln!(
                "Measuring cold idle ({} seconds)",
                report.parameters.idle_seconds
            );
            report.cold_idle = Some(measure_idle(
                &mut controller,
                host.0,
                report.parameters.idle_seconds,
            )?);
            for _ in 0..WARMUP_CYCLES {
                cycle(&mut controller, host.0)?;
                report.warmup_completed += 1;
            }
            report.after_warmup = Some(resources(&controller)?);
            eprintln!(
                "Warmup complete; measuring {} cycles",
                report.parameters.cycles
            );
            for index in 1..=report.parameters.cycles {
                let latency = cycle(&mut controller, host.0)?;
                report.activation.samples_ms.push(latency);
                report.cycles_completed = index;
                if [100, 500, 1000].contains(&index) || index == report.parameters.cycles {
                    report.checkpoints.push(Checkpoint {
                        completed_cycles: index,
                        resources: resources(&controller)?,
                    });
                    eprintln!("Completed {index} measured cycles");
                }
            }
            eprintln!(
                "Measuring used idle ({} seconds)",
                report.parameters.idle_seconds
            );
            report.used_idle = Some(measure_idle(
                &mut controller,
                host.0,
                report.parameters.idle_seconds,
            )?);
            Ok(())
        })();
        if controller.active() {
            controller.stop("resource_probe_cleanup");
            if let Err(error) = drain_to_idle(&mut controller, host.0) {
                report.cleanup_error = Some(error.to_string());
            }
        }
        report.cleanup_completed = is_clean_idle(&controller);
        if !report.cleanup_completed && report.cleanup_error.is_none() {
            report.cleanup_error = Some(
                "Controller did not reach Idle with no timer or attached input worker".to_owned(),
            );
        }
        outcome
    }

    fn cycle(controller: &mut PreviewController, host: HWND) -> ProbeResult<f64> {
        if !is_clean_idle(controller) {
            return Err(failure("Cycle did not begin in fully joined Idle"));
        }
        let before_samples = controller.sample_attempts();
        let started = Instant::now();
        if !controller.start()? {
            return Err(failure("Controller refused a fresh activation"));
        }
        let deadline = started + START_LIMIT;
        loop {
            controller.process_input()?;
            if matches!(controller.state(), AppState::Live { .. }) {
                break;
            }
            if !matches!(controller.state(), AppState::Starting { .. }) {
                return Err(failure(
                    "Input/session interrupted activation before Live; this cycle is NOT_COMPLETED",
                ));
            }
            if Instant::now() >= deadline {
                return Err(failure(
                    "Timed out waiting for the input worker to enter Live",
                ));
            }
            pump(controller, host, true)?;
            if matches!(controller.state(), AppState::Live { .. }) {
                break;
            }
            if !matches!(controller.state(), AppState::Starting { .. }) {
                return Err(failure("Input/session interrupted activation before Live"));
            }
            if controller.active() {
                wait(controller, deadline)?;
            }
        }
        if controller.last_sample().is_none() || controller.sample_attempts() != before_samples + 1
        {
            return Err(failure(
                "A controlled cycle must contain exactly one successful initial Live sample",
            ));
        }
        submit_preview_frame()?;
        let latency = started.elapsed().as_secs_f64() * 1000.0;
        controller.stop("resource_probe_cycle");
        drain_to_idle(controller, host)?;
        Ok(latency)
    }

    fn submit_preview_frame() -> ProbeResult<()> {
        // SAFETY: These identity queries return scalar IDs for the calling
        // process/thread and do not borrow, retain, or create resources.
        let pid = unsafe { GetCurrentProcessId() };
        // SAFETY: Query the current measurement thread's scalar identifier.
        let thread_id = unsafe { GetCurrentThreadId() };
        let mut after = None;
        // SAFETY: The static class name is NUL terminated. after is the previous
        // enumeration result; the query does not take ownership of any HWND.
        while let Ok(hwnd) =
            // SAFETY: Enumerate the static preview class after the prior HWND;
            // this query only borrows identifiers and retains no Rust pointers.
            unsafe { FindWindowExW(None, after, w!("ColorPicker.Preview.v1"), None) }
        {
            after = Some(hwnd);
            let mut owner = 0;
            // SAFETY: USER32 validates the enumerated HWND and writes the local
            // PID slot; identity is checked before touching the preview.
            let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
            if owner == pid && thread == thread_id {
                // SAFETY: This is our controller-owned preview on this thread;
                // no state borrow spans paint dispatch, and no rect output is
                // requested. The owner cannot drop during these synchronous calls.
                if !unsafe { IsWindowVisible(hwnd) }.as_bool()
                    // SAFETY: The controller retains this preview on our thread;
                    // no callback-state borrow spans synchronous paint dispatch.
                    || !unsafe { UpdateWindow(hwnd) }.as_bool()
                    // SAFETY: Query the owned preview's update status without
                    // lending an output rectangle or changing its update region.
                    || unsafe { GetUpdateRect(hwnd, None, false) }.as_bool()
                {
                    return Err(failure("The first preview frame could not be submitted"));
                }
                return Ok(());
            }
        }
        Err(failure(
            "The controller entered Live without its own visible preview window",
        ))
    }

    fn drain_to_idle(controller: &mut PreviewController, host: HWND) -> ProbeResult<()> {
        let deadline = Instant::now() + CLEANUP_LIMIT;
        let mut first_error = None;
        while controller.active() {
            if let Err(error) = controller.process_input() {
                first_error.get_or_insert_with(|| error.to_string());
                controller.stop("resource_probe_drain_error");
            }
            if !controller.active() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(failure(
                    "Timed out waiting for paired input release and worker join; cleanup is NOT_COMPLETED",
                ));
            }
            if let Err(error) = pump(controller, host, false) {
                first_error.get_or_insert_with(|| error.to_string());
                controller.stop("resource_probe_message_failure");
            }
            if controller.active() {
                wait(controller, deadline)?;
            }
        }
        if !is_clean_idle(controller) {
            return Err(failure("Cycle ended outside clean Idle"));
        }
        if let Some(error) = first_error {
            return Err(failure(error));
        }
        Ok(())
    }

    fn pump(controller: &mut PreviewController, host: HWND, stop_at_live: bool) -> ProbeResult<()> {
        let mut message = MSG::default();
        for _ in 0..256 {
            if stop_at_live && matches!(controller.state(), AppState::Live { .. }) {
                break;
            }
            // SAFETY: MSG is initialized local output; no callback-state borrow
            // is held while USER32 dispatches synchronous messages internally.
            if !unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                break;
            }
            if matches!(
                message.message,
                WM_QUIT
                    | WM_DISPLAYCHANGE
                    | WM_SETTINGCHANGE
                    | WM_WTSSESSION_CHANGE
                    | WM_POWERBROADCAST
                    | WM_QUERYENDSESSION
                    | WM_ENDSESSION
            ) || (message.hwnd == host && message.message == WM_CLOSE)
            {
                return Err(failure(
                    "Desktop/display/session changed during this controlled measurement",
                ));
            }
            if message.hwnd == host && message.message == WM_INPUT_WAKE {
                controller.process_input()?;
            } else if message.hwnd == host && message.message == WM_TIMER {
                // Startup returns immediately upon Live, so the initial Ready
                // sample is the only sample. Late timers in Idle/Finishing are
                // rejected by the controller's current timer generation.
                controller.on_timer(message.wParam.0)?;
            } else {
                // SAFETY: MSG came from this measurement thread's queue and
                // remains live; dispatch occurs outside window-state borrows.
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        Ok(())
    }

    fn wait(controller: &PreviewController, deadline: Instant) -> ProbeResult<()> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        let worker = controller.input_wait_handle();
        let handles = worker.map(|handle| [HANDLE(handle.as_raw_handle())]);
        // SAFETY: The controller borrow keeps its JoinHandle alive through this
        // bounded wait; the native handles array is local and never retained.
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(
                handles.as_ref().map(|handles| handles.as_slice()),
                remaining.as_millis().min(u128::from(u32::MAX - 1)).max(1) as u32,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        if result == WAIT_FAILED {
            return Err(Error::from_thread().into());
        }
        Ok(())
    }

    fn is_clean_idle(controller: &PreviewController) -> bool {
        matches!(controller.state(), AppState::Idle)
            && controller.timer_id() == 0
            && controller.input_wait_handle().is_none()
    }

    fn measure_idle(
        controller: &mut PreviewController,
        host: HWND,
        seconds: u64,
    ) -> ProbeResult<IdleMeasurement> {
        if !is_clean_idle(controller) {
            return Err(failure(
                "Idle measurement requires no attached input worker or timer",
            ));
        }
        let before = resources(controller)?;
        let cpu_before = process_times()?;
        let started = Instant::now();
        let deadline = started + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            pump(controller, host, false)?;
            wait(controller, deadline)?;
        }
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        let cpu_after = process_times()?;
        let after = resources(controller)?;
        let cpu_kernel_ms = cpu_after.0.saturating_sub(cpu_before.0) as f64 / 10_000.0;
        let cpu_user_ms = cpu_after.1.saturating_sub(cpu_before.1) as f64 / 10_000.0;
        Ok(IdleMeasurement {
            before,
            after,
            wall_ms,
            cpu_kernel_ms,
            cpu_user_ms,
            cpu_percent_of_one_core: (cpu_kernel_ms + cpu_user_ms) / wall_ms * 100.0,
        })
    }

    fn resources(controller: &PreviewController) -> ProbeResult<Resources> {
        if !is_clean_idle(controller) {
            return Err(failure("Resource checkpoint is not clean Idle"));
        }
        // Close the temporary thread-list snapshot before counting process
        // handles, so it is not mistaken for a persistent application handle.
        let thread_count = thread_count()?;
        // SAFETY: This is a borrowed current-process pseudo handle, used only for
        // queries below and never adopted by the CloseHandle owner.
        let process = unsafe { GetCurrentProcess() };
        let mut memory = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let mut handle_count = 0;
        // SAFETY: memory is an initialized EX structure whose prefix has the
        // base layout; its exact allocation size and the local count slot are
        // supplied. The current-process handle remains valid through the calls.
        unsafe {
            GetProcessMemoryInfo(
                process,
                (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
                size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )?;
            GetProcessHandleCount(process, &mut handle_count)?;
        }
        let gui_count = |flag| -> ProbeResult<u32> {
            // SAFETY: Clear only the calling thread's error slot to distinguish
            // a legitimate zero GUI resource count from API failure.
            unsafe { SetLastError(ERROR_SUCCESS) };
            // SAFETY: The current-process handle is valid and flag is one of the
            // two resource categories passed immediately below this closure.
            let count = unsafe { GetGuiResources(process, flag) };
            // SAFETY: Read this thread's error slot immediately after the query.
            let error = unsafe { GetLastError() };
            if count == 0 && error != ERROR_SUCCESS {
                return Err(Error::from_hresult(HRESULT::from_win32(error.0)).into());
            }
            Ok(count)
        };
        let (kernel, user) = process_times()?;
        Ok(Resources {
            working_set_bytes: memory.WorkingSetSize,
            private_commit_bytes: memory.PrivateUsage,
            thread_count,
            handle_count,
            gdi_objects: gui_count(GR_GDIOBJECTS)?,
            user_objects: gui_count(GR_USEROBJECTS)?,
            process_kernel_100ns: kernel,
            process_user_100ns: user,
            controller_idle: true,
            active_timer_id: controller.timer_id(),
            input_worker_attached: controller.input_wait_handle().is_some(),
        })
    }

    fn process_times() -> ProbeResult<(u64, u64)> {
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: The current-process pseudo handle is valid and all four
        // initialized FILETIME output slots remain live through the query.
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )?;
        }
        let ticks =
            |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        Ok((ticks(kernel), ticks(user)))
    }

    fn thread_count() -> ProbeResult<u32> {
        // SAFETY: The snapshot API creates one owned kernel handle; the private
        // guard adopts it once and closes it after enumeration, including errors.
        let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)? });
        // SAFETY: This query returns the calling process's scalar identifier.
        let pid = unsafe { GetCurrentProcessId() };
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        // SAFETY: entry is initialized and advertises its actual size; the
        // snapshot owner outlives this synchronous enumeration call.
        unsafe {
            Thread32First(snapshot.0, &mut entry)?;
        }
        let mut count = 0;
        loop {
            if entry.th32OwnerProcessID == pid {
                count += 1;
            }
            entry.dwSize = size_of::<THREADENTRY32>() as u32;
            // SAFETY: The snapshot remains owned and entry's capacity/layout is
            // reset before each bounded native write into this local structure.
            match unsafe { Thread32Next(snapshot.0, &mut entry) } {
                Ok(()) => {}
                Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(count)
    }

    struct OwnedHandle(HANDLE);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: This private guard owns a successful snapshot handle; it
            // is neither copied nor exposed as a second owner and closes once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    struct ProbeHost(HWND);
    impl ProbeHost {
        fn new() -> ProbeResult<Self> {
            // SAFETY: The system STATIC class and static strings are valid for
            // synchronous creation. No Rust callback/userdata pointer is passed.
            let host = Self(unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Color Picker resource probe"),
                    WS_OVERLAPPED,
                    0,
                    0,
                    1,
                    1,
                    None,
                    None,
                    None,
                    None,
                )?
            });
            // SAFETY: The newly owned host remains live throughout registration
            // and measurement; its guard unregisters before window destruction.
            unsafe {
                WTSRegisterSessionNotification(host.0, NOTIFY_FOR_THIS_SESSION)?;
            }
            Ok(host)
        }
    }
    impl Drop for ProbeHost {
        fn drop(&mut self) {
            // SAFETY: This measurement thread owns the hidden STATIC HWND. It
            // retains no Rust callback references and outlives its controller.
            unsafe {
                let _ = WTSUnRegisterSessionNotification(self.0);
                let _ = DestroyWindow(self.0);
            }
        }
    }

    struct ScopedPmv2(DPI_AWARENESS_CONTEXT);
    impl ScopedPmv2 {
        fn enter() -> ProbeResult<Self> {
            // SAFETY: Change only this measurement thread to the documented
            // pseudo context; retain the returned valid context for restoration.
            let old =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            if old.0.is_null() {
                Err(Error::from_thread().into())
            } else {
                Ok(Self(old))
            }
        }
    }
    impl Drop for ScopedPmv2 {
        fn drop(&mut self) {
            // SAFETY: The context was returned by this thread's successful
            // SetThreadDpiAwarenessContext and is restored on that same thread.
            let _ = unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }

    fn arguments() -> ProbeResult<Option<Arguments>> {
        let mut args = std::env::args_os().skip(1);
        let (mut cycles, mut idle_seconds, mut output) = (100, 10, None);
        while let Some(argument) = args.next() {
            match argument.to_str() {
                Some("--help" | "-h") => {
                    println!(
                        "resource-probe [--cycles 1..10000] [--idle-seconds 1..3600] [--output NEW_FILE.json]\nDefaults: 100 cycles, 10 seconds per idle phase, JSON to stdout. Always performs 20 warmup cycles.\nExit color-picker first. Keep the desktop unlocked and do not touch mouse buttons or Esc.\nThis explicitly installs short-lived real input hooks; it does not synthesize input or copy text.\nCOMPLETED describes only this controlled start/first-frame/cancel measurement, not full release acceptance."
                    );
                    return Ok(None);
                }
                Some("--cycles") => {
                    cycles = args
                        .next()
                        .ok_or_else(|| failure("--cycles needs a value"))?
                        .to_string_lossy()
                        .parse::<u32>()?;
                    if !(1..=10_000).contains(&cycles) {
                        return Err(failure("--cycles must be between 1 and 10000"));
                    }
                }
                Some("--idle-seconds") => {
                    idle_seconds = args
                        .next()
                        .ok_or_else(|| failure("--idle-seconds needs a value"))?
                        .to_string_lossy()
                        .parse::<u64>()?;
                    if !(1..=3600).contains(&idle_seconds) {
                        return Err(failure("--idle-seconds must be between 1 and 3600"));
                    }
                }
                Some("--output") => {
                    output = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| failure("--output needs a file path"))?,
                    ));
                }
                _ => {
                    return Err(failure(format!(
                        "Unknown argument: {}",
                        argument.to_string_lossy()
                    )));
                }
            }
        }
        Ok(Some(Arguments {
            parameters: Parameters {
                cycles,
                warmup_cycles: WARMUP_CYCLES,
                idle_seconds,
            },
            output,
        }))
    }

    fn write_report(file: Option<&mut File>, report: &Report) -> ProbeResult<()> {
        let bytes = serde_json::to_vec_pretty(report)?;
        if let Some(file) = file {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        } else {
            let mut stdout = io::stdout().lock();
            stdout.write_all(&bytes)?;
            stdout.write_all(b"\n")?;
        }
        Ok(())
    }

    fn failure(message: impl Into<String>) -> Box<dyn StdError> {
        io::Error::other(message.into()).into()
    }
}
