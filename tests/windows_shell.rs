#![cfg(windows)]

//! Explicit desktop smoke tests. Run separately with:
//! cargo test --test windows_shell --locked -- --ignored --test-threads=1
//! These tests create tray icons/previews and reserve the default hotkey temporarily.
//! Sessions briefly install input hooks; tests never synthesize input, open menus,
//! or touch the clipboard. Keep hands off input during these explicit tests.

use std::{
    fs::{self, OpenOptions},
    io,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, ExitStatus},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use color_picker::platform::windows::{
    host::{HOST_CLASS, WM_DIAGNOSTICS, WM_STOP_PREVIEW, WM_TRAY},
    hotkey::DEFAULT_HOTKEY_ID,
    instance::{InstanceStatus, SingleInstance, instance_key},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        System::Threading::CREATE_NO_WINDOW,
        UI::{
            Input::KeyboardAndMouse::{
                MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_C,
            },
            Shell::NIN_SELECT,
            WindowsAndMessaging::{
                FindWindowExW, FindWindowW, GWL_STYLE, GetParent, GetWindowLongPtrW,
                GetWindowThreadProcessId, IsWindowVisible, PostMessageW, RegisterWindowMessageW,
                SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, SendMessageTimeoutW, WM_CLOSE,
                WM_DISPLAYCHANGE, WM_HOTKEY, WM_TIMER, WS_CHILD,
            },
        },
    },
    core::{PCWSTR, w},
};

// The two desktop tests share a process and must not race each other's hosts or hotkeys.
static DESKTOP_TEST: Mutex<()> = Mutex::new(());
const WAIT_LIMIT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const DIAG_PREVIEW_ACTIVE: usize = 5;
const DIAG_ACTIVE_TIMER: usize = 6;
const DIAG_SAMPLE_ATTEMPTS: usize = 7;
const DIAG_PREVIEW_SESSION: usize = 8;
const DIAG_PICKER_STATE: usize = 9;

#[test]
#[ignore = "requires an interactive Windows desktop, Explorer, and a free Ctrl+Alt+C hotkey"]
fn resident_shell_smoke() {
    let _serial = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let title = host_title_without_existing_instance();
    let mut primary = AppChild::spawn();
    let hwnd = primary.wait_ready(&title);

    // SAFETY: Query visibility on the fixture HWND without dereferencing or retaining any
    // native pointer.
    assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool());
    assert!(
        // SAFETY: query the identified test child HWND; the parent handle is used only
        // for this assertion.
        unsafe { GetParent(hwnd) }.is_err(),
        "host must have no parent"
    );
    assert_eq!(
        // SAFETY: Query scalar style bits of the identified child HWND; no userdata
        // pointer is decoded.
        unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32 & WS_CHILD.0,
        0
    );
    assert_eq!(
        diagnostic(hwnd, 2),
        1,
        "Ctrl+Alt+C is already in use or registration failed; this is not a passing hotkey check"
    );
    assert_eq!(diagnostic(hwnd, 3), 1, "tray icon was not registered");
    assert!(matches!(
        SingleInstance::acquire().unwrap(),
        InstanceStatus::Existing
    ));
    assert_eq!(matching_hosts(&title), vec![hwnd]);
    assert_preview_idle_and_quiet(hwnd);

    let activations = diagnostic(hwnd, 1);
    let mut second = AppChild::spawn();
    assert!(
        second.wait_exit().success(),
        "second launch did not exit successfully"
    );
    let second_log = second.log_contents();
    assert_logged(&second_log, "instance.existing");
    assert_logged(&second_log, "instance.activation_forwarded");
    wait_counter(hwnd, 1, activations + 1);
    wait_diagnostic(hwnd, DIAG_PREVIEW_ACTIVE, 1);
    wait_diagnostic(hwnd, DIAG_PICKER_STATE, 2);
    let first_session = diagnostic(hwnd, DIAG_PREVIEW_SESSION);
    let first_timer = diagnostic(hwnd, DIAG_ACTIVE_TIMER);
    assert_ne!(first_session, 0);
    assert_ne!(first_timer, 0);
    assert!(diagnostic(hwnd, DIAG_SAMPLE_ATTEMPTS) > 0);
    assert_eq!(
        matching_hosts(&title),
        vec![hwnd],
        "second launch created another host"
    );
    primary.assert_owns(hwnd);

    // This only checks WM_HOTKEY routing. It does not press a key or prove physical input delivery.
    // Wait for its logged receipt before asserting no change: sent diagnostics
    // can otherwise overtake the posted hotkey and accidentally pass too early.
    let activations = diagnostic(hwnd, 1);
    let received = event_count(&primary.log_contents(), "hotkey.received");
    let packed_hotkey = (u32::from(VK_C.0) << 16) | (MOD_CONTROL | MOD_ALT).0;
    primary.post(
        hwnd,
        WM_HOTKEY,
        WPARAM(DEFAULT_HOTKEY_ID as usize),
        LPARAM(packed_hotkey as isize),
    );
    wait_logged_count(&primary, "hotkey.received", received + 1);
    assert_eq!(
        diagnostic(hwnd, 1),
        activations,
        "repeat activation nested a preview session"
    );
    assert_eq!(diagnostic(hwnd, DIAG_PREVIEW_ACTIVE), 1);
    assert_eq!(diagnostic(hwnd, DIAG_PREVIEW_SESSION), first_session);
    assert_eq!(diagnostic(hwnd, DIAG_ACTIVE_TIMER), first_timer);

    primary.post(hwnd, WM_STOP_PREVIEW, WPARAM(0), LPARAM(0));
    wait_diagnostic(hwnd, DIAG_PREVIEW_ACTIVE, 0);
    wait_diagnostic(hwnd, DIAG_ACTIVE_TIMER, 0);
    let stopped_samples = assert_preview_idle_and_quiet(hwnd);

    primary.post(
        hwnd,
        WM_HOTKEY,
        WPARAM(DEFAULT_HOTKEY_ID as usize),
        LPARAM(packed_hotkey as isize),
    );
    wait_counter(hwnd, 1, activations + 1);
    wait_diagnostic(hwnd, DIAG_PREVIEW_ACTIVE, 1);
    wait_diagnostic(hwnd, DIAG_PICKER_STATE, 2);
    let second_session = diagnostic(hwnd, DIAG_PREVIEW_SESSION);
    let second_timer = diagnostic(hwnd, DIAG_ACTIVE_TIMER);
    assert!(
        second_session > first_session,
        "preview reused an old session ID"
    );
    assert!(second_timer > first_timer, "preview reused an old timer ID");
    assert!(diagnostic(hwnd, DIAG_SAMPLE_ATTEMPTS) > stopped_samples);

    // Target only our window. Do not restart Explorer or broadcast to other applications.
    // The posted TaskbarCreated message also provides an observable barrier:
    // reaching its restoration counter means the preceding stale timer was dequeued.
    let restorations = diagnostic(hwnd, 4);
    // SAFETY: The registered message name is a static terminated UTF-16 string; registration
    // retains no Rust allocation.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    assert_ne!(taskbar_created, 0, "could not register TaskbarCreated");
    primary.post(hwnd, WM_TIMER, WPARAM(first_timer), LPARAM(0));
    primary.post(hwnd, taskbar_created, WPARAM(0), LPARAM(0));
    wait_counter(hwnd, 4, restorations + 1);
    assert_eq!(diagnostic(hwnd, 3), 1);
    assert_eq!(diagnostic(hwnd, 1), activations + 1);
    assert_eq!(diagnostic(hwnd, DIAG_PREVIEW_ACTIVE), 1);
    assert_eq!(diagnostic(hwnd, DIAG_PREVIEW_SESSION), second_session);
    assert_eq!(diagnostic(hwnd, DIAG_ACTIVE_TIMER), second_timer);

    // Simulate only the notification to our host, without changing display settings.
    primary.post(hwnd, WM_DISPLAYCHANGE, WPARAM(32), LPARAM(0));
    wait_diagnostic(hwnd, DIAG_PREVIEW_ACTIVE, 0);
    wait_diagnostic(hwnd, DIAG_ACTIVE_TIMER, 0);
    let stopped_samples = assert_preview_idle_and_quiet(hwnd);
    for stale_timer in [first_timer, second_timer] {
        primary.post(hwnd, WM_TIMER, WPARAM(stale_timer), LPARAM(0));
    }
    primary.post(hwnd, taskbar_created, WPARAM(0), LPARAM(0));
    wait_counter(hwnd, 4, restorations + 2);
    assert_eq!(diagnostic(hwnd, 3), 1);
    assert_eq!(diagnostic(hwnd, 1), activations + 1);
    assert_eq!(
        assert_preview_idle_and_quiet(hwnd),
        stopped_samples,
        "stale timers sampled after the display change stopped the session"
    );

    primary.close();
    let primary_log = primary.log_contents();
    for event in [
        "hotkey.registered",
        "hotkey.received",
        "activation.handled",
        "activation.ignored",
        "preview.started",
        "preview.stopped",
        "environment.changed",
        "host.stopped",
    ] {
        assert_logged(&primary_log, event);
    }
    assert_eq!(event_count(&primary_log, "preview.started"), 2);
    assert_eq!(event_count(&primary_log, "preview.stopped"), 2);
    assert!(
        matching_hosts(&title).is_empty(),
        "host survived normal shutdown"
    );
    let mut restarted = AppChild::spawn();
    let restarted_hwnd = restarted.wait_ready(&title);
    assert_eq!(
        diagnostic(restarted_hwnd, 2),
        1,
        "hotkey was not released on exit"
    );
    assert_eq!(diagnostic(restarted_hwnd, 3), 1);
    assert_eq!(diagnostic(restarted_hwnd, 1), 0);
    assert_preview_idle_and_quiet(restarted_hwnd);
    restarted.close();
    assert!(matching_hosts(&title).is_empty());
}

#[test]
#[ignore = "temporarily reserves Ctrl+Alt+C; requires an interactive Windows desktop and Explorer"]
fn hotkey_conflict_keeps_tray_activation_available() {
    let _serial = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let title = host_title_without_existing_instance();
    let reservation = HotkeyReservation::new();
    // Declared after the reservation so unwinding closes the child before releasing the hotkey.
    let mut child = AppChild::spawn();
    let hwnd = child.wait_ready(&title);
    assert_eq!(
        diagnostic(hwnd, 2),
        0,
        "application unexpectedly registered the reserved hotkey"
    );
    assert_eq!(
        diagnostic(hwnd, 3),
        1,
        "a hotkey conflict must not remove the tray entry"
    );
    assert_preview_idle_and_quiet(hwnd);

    let activations = diagnostic(hwnd, 1);
    // Version-4 tray callback: icon ID in the high word, NIN_SELECT in the low word.
    child.post(
        hwnd,
        WM_TRAY,
        WPARAM(0),
        LPARAM(((1_u32 << 16) | NIN_SELECT) as isize),
    );
    wait_counter(hwnd, 1, activations + 1);
    wait_diagnostic(hwnd, DIAG_PREVIEW_ACTIVE, 1);
    wait_diagnostic(hwnd, DIAG_PICKER_STATE, 2);
    assert_ne!(diagnostic(hwnd, DIAG_ACTIVE_TIMER), 0);
    assert_ne!(diagnostic(hwnd, DIAG_PREVIEW_SESSION), 0);
    assert!(diagnostic(hwnd, DIAG_SAMPLE_ATTEMPTS) > 0);
    assert_eq!(diagnostic(hwnd, 2), 0);
    child.post(hwnd, WM_STOP_PREVIEW, WPARAM(0), LPARAM(0));
    wait_diagnostic(hwnd, DIAG_PREVIEW_ACTIVE, 0);
    wait_diagnostic(hwnd, DIAG_ACTIVE_TIMER, 0);
    assert_preview_idle_and_quiet(hwnd);
    child.close();
    let conflict_log = child.log_contents();
    assert_logged(&conflict_log, "hotkey.registration_failed");
    assert_logged(&conflict_log, "tray.activation_received");
    assert_logged(&conflict_log, "preview.started");
    assert_logged(&conflict_log, "preview.stopped");
    assert!(
        !contains_event(&conflict_log, "hotkey.registered"),
        "conflicting hotkey was incorrectly logged as registered:\n{conflict_log}"
    );
    assert!(matching_hosts(&title).is_empty());
    drop(reservation);
}

fn host_title_without_existing_instance() -> Vec<u16> {
    let title: Vec<u16> = instance_key()
        .unwrap()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    assert!(
        matching_hosts(&title).is_empty(),
        "An existing color-picker host is running. Close it yourself before this desktop test; the test will not close your instance."
    );
    title
}

fn matching_hosts(title: &[u16]) -> Vec<HWND> {
    let mut result = Vec::new();
    let mut previous = None;
    while let Ok(hwnd) =
        // SAFETY: The class and title are live terminated strings; enumeration borrows
        // HWND values without accessing callback pointers.
        unsafe { FindWindowExW(None, previous, HOST_CLASS, PCWSTR(title.as_ptr())) }
    {
        result.push(hwnd);
        assert!(result.len() < 16, "unexpectedly many matching host windows");
        previous = Some(hwnd);
    }
    result
}

fn window_process(hwnd: HWND) -> u32 {
    let mut process = 0;
    // SAFETY: The HWND is only queried and the process-ID output points to a writable local u32.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process)) };
    process
}

fn try_diagnostic(hwnd: HWND, field: usize) -> Option<usize> {
    let mut result = 0;
    // SAFETY: The identified test child receives scalar diagnostics; the writable result
    // slot stays alive through this bounded synchronous call.
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_DIAGNOSTICS,
            WPARAM(field),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
            300,
            Some(&mut result),
        )
    };
    (sent.0 != 0).then_some(result)
}

fn diagnostic(hwnd: HWND, field: usize) -> usize {
    try_diagnostic(hwnd, field).expect("host diagnostics timed out or the window exited")
}

fn wait_counter(hwnd: HWND, field: usize, expected: usize) {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let value = diagnostic(hwnd, field);
        if value >= expected {
            assert_eq!(
                value, expected,
                "host processed an unexpected extra activation/event"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "host diagnostic {field} never reached {expected}"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_diagnostic(hwnd: HWND, field: usize, expected: usize) {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        if diagnostic(hwnd, field) == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "host diagnostic {field} did not become {expected}"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn assert_preview_idle_and_quiet(hwnd: HWND) -> usize {
    assert_eq!(diagnostic(hwnd, DIAG_PREVIEW_ACTIVE), 0);
    assert_eq!(diagnostic(hwnd, DIAG_ACTIVE_TIMER), 0);
    assert_eq!(diagnostic(hwnd, DIAG_PREVIEW_SESSION), 0);
    let samples = diagnostic(hwnd, DIAG_SAMPLE_ATTEMPTS);
    // Multiple requested 17ms ticks must pass without any new sampling at idle.
    let deadline = Instant::now() + Duration::from_millis(125);
    while Instant::now() < deadline {
        thread::sleep(POLL_INTERVAL);
        assert_eq!(
            diagnostic(hwnd, DIAG_PREVIEW_ACTIVE),
            0,
            "preview resumed without activation"
        );
        assert_eq!(
            diagnostic(hwnd, DIAG_ACTIVE_TIMER),
            0,
            "a timer survived session cleanup"
        );
        assert_eq!(diagnostic(hwnd, DIAG_PREVIEW_SESSION), 0);
        assert_eq!(
            diagnostic(hwnd, DIAG_SAMPLE_ATTEMPTS),
            samples,
            "sampling continued while idle"
        );
    }
    samples
}

fn wait_logged_count(child: &AppChild, event: &str, expected: usize) {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let count = event_count(&child.log_contents(), event);
        if count >= expected {
            assert_eq!(count, expected, "unexpected extra {event} log event");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "host did not log {event} {expected} times"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn contains_event(log: &str, event: &str) -> bool {
    event_count(log, event) != 0
}

fn event_count(log: &str, event: &str) -> usize {
    // Each record starts with unix_ms, pid and elapsed_ms, followed by its event.
    // Match the event token rather than text that might occur in a message field.
    log.lines()
        .filter(|line| line.split_whitespace().nth(3) == Some(event))
        .count()
}

fn assert_logged(log: &str, event: &str) {
    assert!(
        contains_event(log, event),
        "missing log event {event}:\n{log}"
    );
}

struct AppChild {
    child: Child,
    hwnd: Option<HWND>,
    log: TempLog,
}

impl AppChild {
    fn spawn() -> Self {
        let log = TempLog::new();
        let child = Command::new(env!("CARGO_BIN_EXE_color-picker"))
            .arg("--diagnostics")
            .arg("--no-onboarding")
            .arg("--log-file")
            .arg(&log.path)
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .expect("could not start the color-picker test child");
        Self {
            child,
            hwnd: None,
            log,
        }
    }

    fn log_contents(&self) -> String {
        fs::read_to_string(&self.log.path).expect("could not read the test child's diagnostic log")
    }

    fn wait_ready(&mut self, title: &[u16]) -> HWND {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "test child exited before becoming ready"
            );
            // SAFETY: The class and title are live terminated strings; ownership is
            // checked before sending any message to the returned HWND.
            if let Ok(hwnd) = unsafe { FindWindowW(HOST_CLASS, PCWSTR(title.as_ptr())) } {
                self.assert_owns(hwnd);
                self.hwnd = Some(hwnd);
                if try_diagnostic(hwnd, 0) == Some(1) {
                    return hwnd;
                }
            }
            assert!(
                Instant::now() < deadline,
                "test child did not become ready within five seconds"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn assert_owns(&self, hwnd: HWND) {
        assert_eq!(
            window_process(hwnd),
            self.child.id(),
            "host belongs to another process; refusing to send messages or close it"
        );
    }

    fn post(&self, hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) {
        self.assert_owns(hwnd);
        // SAFETY: The HWND was checked against this test child process; the posted
        // message contains only scalar values and no borrowed pointers.
        unsafe { PostMessageW(Some(hwnd), message, wparam, lparam) }.unwrap();
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "test child did not exit within five seconds"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn close(&mut self) {
        let hwnd = self.hwnd.expect("test child has no ready host");
        self.post(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        assert!(
            self.wait_exit().success(),
            "test child failed during normal shutdown"
        );
        self.hwnd = None;
    }
}

impl Drop for AppChild {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Some(hwnd) = self.hwnd
            && window_process(hwnd) == self.child.id()
        {
            // SAFETY: The HWND was checked against this test child process; the
            // posted message contains only scalar values and no borrowed pointers.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                thread::sleep(POLL_INTERVAL);
            }
        }
        // This Child handle was created by this test. Never enumerate or kill user processes.
        self.log.remove_on_drop = if self.child.kill().is_ok() {
            self.child.wait().is_ok()
        } else {
            matches!(self.child.try_wait(), Ok(Some(_)))
        };
        // If cleanup could not confirm exit, leave this child's log intact.
        // Otherwise its TempLog field is dropped only after the child stopped.
    }
}

struct TempLog {
    path: PathBuf,
    remove_on_drop: bool,
}

impl TempLog {
    fn new() -> Self {
        static NEXT_LOG: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let serial = NEXT_LOG.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "color-picker-shell-test-{}-{timestamp}-{serial}.log",
                std::process::id(),
            ));
            // Atomically reserve a new file. Never adopt or truncate an existing log.
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    drop(file);
                    return Self {
                        path,
                        remove_on_drop: true,
                    };
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("could not allocate a temporary diagnostic log: {error}"),
            }
        }
        panic!("could not allocate a unique temporary diagnostic log");
    }
}

impl Drop for TempLog {
    fn drop(&mut self) {
        if self.remove_on_drop {
            // Only this exact file, reserved with create_new, belongs to this guard.
            let _ = fs::remove_file(&self.path);
        }
    }
}

const CONFLICT_HOTKEY_ID: i32 = 0x43a1;

struct HotkeyReservation;

impl HotkeyReservation {
    fn new() -> Self {
        // SAFETY: Register only this test thread hotkey ID; its reservation guard
        // unregisters the same ID on this thread.
        unsafe {
            RegisterHotKey(None, CONFLICT_HOTKEY_ID, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, u32::from(VK_C.0))
        }
        .expect("Ctrl+Alt+C is already in use on this desktop; cannot run a controlled conflict test or claim it passed");
        Self
    }
}

impl Drop for HotkeyReservation {
    fn drop(&mut self) {
        // SAFETY: Release the hotkey ID successfully registered by this guard on the
        // same test thread.
        let _ = unsafe { UnregisterHotKey(None, CONFLICT_HOTKEY_ID) };
    }
}
