//! Hidden top-level host. Callbacks enqueue small, coalesced intentions only;
//! application work runs after DispatchMessage returns, with no borrowed window
//! data surviving a reentrant Win32 call. No HWND userdata allocation is needed.

use std::{cell::Cell, time::Duration};

use windows::{
    Win32::{
        Foundation::{E_FAIL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
                WTSUnRegisterSessionNotification,
            },
        },
        UI::{
            Shell::{NIN_BALLOONHIDE, NIN_BALLOONSHOW, NIN_BALLOONTIMEOUT, NIN_SELECT, NINF_KEY},
            WindowsAndMessaging::*,
        },
    },
    core::{Error, PCWSTR, Result, w},
};

use crate::{
    app::{controller::PreviewController, diagnostics},
    core::format::{ColorFormat, format_color},
};

use super::{
    hotkey,
    input::WM_INPUT_WAKE,
    instance::{InstanceStatus, SingleInstance, instance_key},
    tray::{TrayCommand, TrayIcon},
};

pub const HOST_CLASS: PCWSTR = w!("ColorPicker.Host.v1");
pub const WM_ACTIVATE_PICKER: u32 = WM_APP + 1;
pub const WM_TRAY: u32 = WM_APP + 2;
/// Read-only diagnostics, enabled explicitly with --diagnostics. wParam selects
/// ready / activation count / hotkey registered / tray added / tray restorations.
pub const WM_DIAGNOSTICS: u32 = WM_APP + 3;
/// Diagnostic harness only: normal users end M2 preview using the tray menu.
pub const WM_STOP_PREVIEW: u32 = WM_APP + 4;

// Activation source bits preserve logging even when the request must be ignored.
const ACTIVATE: u16 = 1;
const MENU: u16 = 2;
const EXIT: u16 = 4;
const RESTORE_TRAY: u16 = 8;
const ENVIRONMENT_CHANGED: u16 = 16;
const HOTKEY_ACTIVATE: u16 = 32;
const TRAY_ACTIVATE: u16 = 64;
const BALLOON_SHOW: u16 = 128;
const BALLOON_HIDE: u16 = 256;
const BALLOON_TIMEOUT: u16 = 512;
const SAMPLE_TICK: u16 = 1024;
const STOP_PREVIEW: u16 = 2048;
const START_REQUEST: u16 = 4096;
const ACTIVATION_IGNORED: u16 = 8192;
const INPUT_WAKE: u16 = 16384;
// shellapi.h defines this expression; windows 0.62.2 does not emit that macro.
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

thread_local! {
    // Cells have no dynamic borrows, callbacks cannot alias the main-thread app.
    static PENDING: Cell<u16> = const { Cell::new(0) };
    static TASKBAR_MESSAGE: Cell<u32> = const { Cell::new(0) };
    static CALLBACK_FAILED: Cell<bool> = const { Cell::new(false) };
    static DIAGNOSTICS: Cell<bool> = const { Cell::new(false) };
    static READY: Cell<bool> = const { Cell::new(false) };
    static ACTIVATIONS: Cell<u32> = const { Cell::new(0) };
    static HOTKEY_REGISTERED: Cell<bool> = const { Cell::new(false) };
    static TRAY_ADDED: Cell<bool> = const { Cell::new(false) };
    static TRAY_RESTORATIONS: Cell<u32> = const { Cell::new(0) };
    static PREVIEW_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ACTIVE_TIMER: Cell<usize> = const { Cell::new(0) };
    static PENDING_TIMER: Cell<usize> = const { Cell::new(0) };
    static SAMPLE_ATTEMPTS: Cell<u64> = const { Cell::new(0) };
    static PREVIEW_SESSION: Cell<u64> = const { Cell::new(0) };
    static PICKER_STATE: Cell<isize> = const { Cell::new(0) };
}

pub fn run(diagnostics: bool) -> Result<()> {
    let _instance = match SingleInstance::acquire()? {
        InstanceStatus::Primary(instance) => {
            diagnostics::event(format_args!("instance.primary"));
            instance
        }
        InstanceStatus::Existing => {
            diagnostics::event(format_args!(
                "instance.existing logging_applies_to_this_process_only; exit the old instance from its tray and restart with --log-file to trace hotkeys"
            ));
            return activate_existing();
        }
    };
    DIAGNOSTICS.set(diagnostics);
    let taskbar_message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    if taskbar_message == 0 {
        return Err(Error::from_thread());
    }
    TASKBAR_MESSAGE.set(taskbar_message);

    let class = RegisteredClass::new()?;
    let title = wide(&instance_key()?);
    let window = OwnedWindow(unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            HOST_CLASS,
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(class.instance),
            None,
        )?
    });
    // Deliberately never ShowWindow; parent=None makes this a broadcast-capable
    // top-level window, not HWND_MESSAGE. Guards below drop before the window.
    let mut tray = TrayIcon::new(window.0, WM_TRAY)?;
    TRAY_ADDED.set(true);
    diagnostics::event(format_args!("tray.registered version=4"));
    let hotkey = match hotkey::register_default(window.0) {
        Ok(guard) => {
            diagnostics::event(format_args!(
                "hotkey.registered chord=Ctrl+Alt+C id={} norepeat=true",
                guard.id()
            ));
            Some(guard)
        }
        Err(error) => {
            diagnostics::event(format_args!(
                "hotkey.registration_failed chord=Ctrl+Alt+C error={error}"
            ));
            notify(
                &tray,
                "快捷键注册失败",
                &format!("Ctrl + Alt + C 不可用：{error}。仍可通过托盘开始取色。"),
            );
            None
        }
    };
    HOTKEY_REGISTERED.set(hotkey.is_some());
    let session_notifications = match SessionNotifications::new(window.0) {
        Ok(guard) => {
            diagnostics::event(format_args!("session_notifications.registered"));
            Some(guard)
        }
        Err(error) => {
            diagnostics::event(format_args!(
                "session_notifications.registration_failed error={error}"
            ));
            notify(
                &tray,
                "会话通知不可用",
                &format!("无法订阅锁屏和会话通知：{error}。当前仍可使用托盘。"),
            );
            None
        }
    };
    READY.set(true);
    diagnostics::event(format_args!(
        "host.ready hotkey_registered={} stage=M3; left click picks, right click or Esc cancels",
        hotkey.is_some()
    ));

    let result = message_loop(window.0, &mut tray);
    READY.set(false);
    HOTKEY_REGISTERED.set(false);
    TRAY_ADDED.set(false);
    drop(session_notifications);
    drop(hotkey);
    drop(tray);
    drop(window);
    drop(class);
    diagnostics::event(format_args!("host.stopped resources_released=true"));
    result
}

fn message_loop(hwnd: HWND, tray: &mut TrayIcon) -> Result<()> {
    let mut controller = PreviewController::new(hwnd);
    let mut exiting = false;
    let mut deferred_menu = false;
    loop {
        if CALLBACK_FAILED.get() {
            return Err(Error::new(E_FAIL, "Failed to wake the host message loop"));
        }
        if let Err(error) = controller.process_input() {
            diagnostics::event(format_args!("input.failed error={error}"));
            notify(tray, "取色已停止", &error.to_string());
        }
        publish_preview_status(&controller);
        if let Some(picked) = controller.take_result()
            && !exiting
        {
            // M5 replaces this temporary shell result with the native result window.
            notify(
                tray,
                "已取色",
                &format!(
                    "{}  X: {}  Y: {}",
                    format_color(picked.rgb, ColorFormat::Hex),
                    picked.source.x,
                    picked.source.y
                ),
            );
        }
        if exiting && !controller.active() {
            return Ok(());
        }
        if deferred_menu && !controller.active() {
            deferred_menu = false;
            PENDING.set(PENDING.get() | MENU);
        }
        // Drain before blocking too: a startup error dialog can consume WM_NULL
        // in its nested loop while leaving an activation/exit intention pending.
        // Menu APIs can pump messages; newly queued intentions stay in PENDING
        // and are consumed after the menu closes instead of disappearing.
        loop {
            let pending = PENDING.replace(0);
            if pending == 0 {
                break;
            }
            if pending & EXIT != 0 {
                diagnostics::event(format_args!("host.exit_requested"));
                exiting = true;
                controller.stop("host_exit");
                publish_preview_status(&controller);
            }
            if exiting {
                continue;
            }
            // A topology, desktop/session or power transition invalidates the
            // current DC and coordinates. Never automatically resume capture.
            if pending & ENVIRONMENT_CHANGED != 0 {
                controller.stop("environment_changed");
                publish_preview_status(&controller);
                diagnostics::event(format_args!("environment.changed"));
            }
            if pending & STOP_PREVIEW != 0 {
                controller.stop("diagnostic_request");
                publish_preview_status(&controller);
            }
            if pending & BALLOON_SHOW != 0 {
                diagnostics::event(format_args!("tray.balloon_show received_from_shell=true"));
            }
            if pending & BALLOON_HIDE != 0 {
                diagnostics::event(format_args!("tray.balloon_hide"));
            }
            if pending & BALLOON_TIMEOUT != 0 {
                diagnostics::event(format_args!("tray.balloon_timeout"));
            }
            if pending & RESTORE_TRAY != 0 {
                TRAY_ADDED.set(false);
                match tray.recreate() {
                    Ok(()) => {
                        TRAY_ADDED.set(true);
                        TRAY_RESTORATIONS.set(TRAY_RESTORATIONS.get().saturating_add(1));
                        diagnostics::event(format_args!("tray.restored"));
                    }
                    // A hidden process without a tray may have no usable exit
                    // path (especially if its hotkey also conflicted).
                    Err(error) => {
                        return Err(Error::new(
                            error.code(),
                            format!("恢复托盘图标失败：{error}"),
                        ));
                    }
                }
            }
            if pending & HOTKEY_ACTIVATE != 0 {
                diagnostics::event(format_args!("hotkey.received chord=Ctrl+Alt+C"));
            }
            if pending & TRAY_ACTIVATE != 0 {
                diagnostics::event(format_args!("tray.activation_received"));
            }
            if pending & ACTIVATE != 0 {
                diagnostics::event(format_args!("instance.activation_received"));
            }
            dispatch_activation_intent(
                pending,
                || activate(tray, &mut controller),
                || {
                    diagnostics::event(format_args!(
                        "activation.ignored reason=preview_already_active"
                    ))
                },
            );
            if pending & MENU != 0 {
                diagnostics::event(format_args!("tray.menu_requested"));
                if controller.active() {
                    // Native menus must not run while the input owner consumes
                    // their clicks. Drain hooks asynchronously before opening.
                    controller.stop("tray_menu");
                    deferred_menu = true;
                    publish_preview_status(&controller);
                    continue;
                }
                match tray.show_menu(false)? {
                    Some(TrayCommand::Start) => {
                        diagnostics::event(format_args!("tray.menu_selected command=start"));
                        activate(tray, &mut controller);
                    }
                    Some(TrayCommand::Stop) => {
                        controller.stop("tray_menu");
                        diagnostics::event(format_args!("tray.menu_selected command=stop"));
                    }
                    Some(TrayCommand::Settings) => {
                        controller.stop("settings");
                        controller.close_result()?;
                        notify(tray, "设置", "快捷键和复制设置将在后续版本接入。");
                    }
                    Some(TrayCommand::Exit) => {
                        diagnostics::event(format_args!("tray.menu_selected command=exit"));
                        exiting = true;
                        controller.stop("host_exit");
                    }
                    None => {}
                }
                publish_preview_status(&controller);
            }
            if pending & SAMPLE_TICK != 0 {
                let timer = PENDING_TIMER.replace(0);
                if let Err(error) = controller.on_timer(timer) {
                    diagnostics::event(format_args!("preview.failed error={error}"));
                    notify(tray, "预览已停止", &error.to_string());
                }
                publish_preview_status(&controller);
            }
        }
        if exiting && !controller.active() {
            return Ok(());
        }
        let mut message = MSG::default();
        let status = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if status == -1 {
            return Err(Error::from_thread());
        }
        if status == 0 {
            exiting = true;
            controller.stop("quit_message");
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn activate(tray: &TrayIcon, controller: &mut PreviewController) {
    match controller.start() {
        Ok(true) => {
            ACTIVATIONS.set(ACTIVATIONS.get().saturating_add(1));
            diagnostics::event(format_args!(
                "activation.handled count={} stage=M3",
                ACTIVATIONS.get()
            ));
        }
        Ok(false) => diagnostics::event(format_args!(
            "activation.ignored reason=preview_already_active"
        )),
        Err(error) => {
            diagnostics::event(format_args!("preview.start_failed error={error}"));
            notify(tray, "无法开始预览", &error.to_string());
        }
    }
    publish_preview_status(controller);
}

fn publish_preview_status(controller: &PreviewController) {
    PREVIEW_ACTIVE.set(controller.active());
    ACTIVE_TIMER.set(controller.timer_id());
    PREVIEW_SESSION.set(controller.session_id());
    SAMPLE_ATTEMPTS.set(controller.sample_attempts());
    PICKER_STATE.set(controller.state_code());
}

fn notify(tray: &TrayIcon, title: &str, message: &str) {
    match tray.notify(title, message) {
        Ok(()) => diagnostics::event(format_args!(
            "tray.notification_accepted title={title}; visibility depends on Windows notification settings"
        )),
        Err(error) => {
            diagnostics::event(format_args!(
                "tray.notification_failed title={title} error={error}"
            ));
            show_error(&format!("{title}\n{message}\n\n托盘通知失败：{error}"));
        }
    }
}

fn activate_existing() -> Result<()> {
    let title = wide(&instance_key()?);
    // A second launch can race the first process's window creation. This is a
    // bounded startup retry, never a resident timer or a background worker.
    for _ in 0..20 {
        if let Ok(hwnd) = unsafe { FindWindowW(HOST_CLASS, PCWSTR(title.as_ptr())) }
            && unsafe { PostMessageW(Some(hwnd), WM_ACTIVATE_PICKER, WPARAM(0), LPARAM(0)) }.is_ok()
        {
            diagnostics::event(format_args!("instance.activation_forwarded"));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(Error::new(
        E_FAIL,
        "color-picker 已在运行，但现有实例尚未就绪；请稍后重试。",
    ))
}

fn enqueue(hwnd: HWND, action: u16) {
    let previous = PENDING.replace(PENDING.get() | action);
    if previous != 0 {
        return;
    }
    // GetMessage dispatches sent broadcasts internally; wake it so it can
    // return to the outer loop even if no other posted messages are waiting.
    if unsafe { PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)) }.is_err() {
        CALLBACK_FAILED.set(true);
        unsafe { PostQuitMessage(1) };
    }
}

/// Capture eligibility at receipt, before a nested menu loop can stop the
/// preview. Source bits alone must never turn into a later start request.
fn activation_intent(source: u16, preview_active: bool) -> u16 {
    source
        | if preview_active {
            ACTIVATION_IGNORED
        } else {
            START_REQUEST
        }
}

fn dispatch_activation_intent(pending: u16, start: impl FnOnce(), ignored: impl FnOnce()) {
    if pending & ACTIVATION_IGNORED != 0 {
        ignored();
    }
    // An explicit stop or invalidated desktop wins over coalesced starts.
    if pending & START_REQUEST != 0 && pending & (ENVIRONMENT_CHANGED | STOP_PREVIEW) == 0 {
        start();
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // A Rust panic must never unwind through Win32. Normal API errors use Result.
    std::panic::catch_unwind(|| unsafe { window_proc_inner(hwnd, message, wparam, lparam) })
        .unwrap_or_else(|_| std::process::abort())
}

unsafe fn window_proc_inner(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == TASKBAR_MESSAGE.get() && message != 0 {
        enqueue(hwnd, RESTORE_TRAY);
        return LRESULT(0);
    }
    match message {
        WM_INPUT_WAKE => enqueue(hwnd, INPUT_WAKE),
        WM_TIMER if wparam.0 != 0 && wparam.0 == ACTIVE_TIMER.get() => {
            PENDING_TIMER.set(wparam.0);
            enqueue(hwnd, SAMPLE_TICK);
        }
        WM_STOP_PREVIEW if DIAGNOSTICS.get() => enqueue(hwnd, STOP_PREVIEW),
        WM_ACTIVATE_PICKER => enqueue(hwnd, activation_intent(ACTIVATE, PREVIEW_ACTIVE.get())),
        WM_HOTKEY if wparam.0 == hotkey::DEFAULT_HOTKEY_ID as usize => enqueue(
            hwnd,
            activation_intent(HOTKEY_ACTIVATE, PREVIEW_ACTIVE.get()),
        ),
        WM_TRAY => match (lparam.0 as u32) & 0xffff {
            NIN_SELECT | NIN_KEYSELECT => {
                enqueue(hwnd, activation_intent(TRAY_ACTIVATE, PREVIEW_ACTIVE.get()))
            }
            WM_CONTEXTMENU => enqueue(hwnd, MENU),
            NIN_BALLOONSHOW => enqueue(hwnd, BALLOON_SHOW),
            NIN_BALLOONHIDE => enqueue(hwnd, BALLOON_HIDE),
            NIN_BALLOONTIMEOUT => enqueue(hwnd, BALLOON_TIMEOUT),
            _ => {}
        },
        WM_DIAGNOSTICS if DIAGNOSTICS.get() => {
            return LRESULT(match wparam.0 {
                0 => READY.get() as isize,
                1 => ACTIVATIONS.get() as isize,
                2 => HOTKEY_REGISTERED.get() as isize,
                3 => TRAY_ADDED.get() as isize,
                4 => TRAY_RESTORATIONS.get() as isize,
                5 => PREVIEW_ACTIVE.get() as isize,
                6 => ACTIVE_TIMER.get() as isize,
                7 => SAMPLE_ATTEMPTS.get() as isize,
                8 => PREVIEW_SESSION.get() as isize,
                9 => PICKER_STATE.get(),
                _ => -1,
            });
        }
        WM_CLOSE => enqueue(hwnd, EXIT),
        WM_QUERYENDSESSION => return LRESULT(1),
        WM_ENDSESSION if wparam.0 != 0 => enqueue(hwnd, EXIT),
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_WTSSESSION_CHANGE => {
            enqueue(hwnd, ENVIRONMENT_CHANGED)
        }
        WM_POWERBROADCAST => {
            enqueue(hwnd, ENVIRONMENT_CHANGED);
            return LRESULT(1);
        }
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}

pub fn show_error(message: &str) {
    let text = wide(message);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("color-picker"),
            MB_OK | MB_ICONERROR,
        )
    };
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct RegisteredClass {
    instance: HINSTANCE,
}

impl RegisteredClass {
    fn new() -> Result<Self> {
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None)? }.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: HOST_CLASS,
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(Error::from_thread());
        }
        Ok(Self { instance })
    }
}

impl Drop for RegisteredClass {
    fn drop(&mut self) {
        let _ = unsafe { UnregisterClassW(HOST_CLASS, Some(self.instance)) };
    }
}

struct OwnedWindow(HWND);

impl Drop for OwnedWindow {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

struct SessionNotifications(HWND);

impl SessionNotifications {
    fn new(hwnd: HWND) -> Result<Self> {
        unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION)? };
        Ok(Self(hwnd))
    }
}

impl Drop for SessionNotifications {
    fn drop(&mut self) {
        let _ = unsafe { WTSUnRegisterSessionNotification(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real receipt/dispatch helpers run against a controlled preview owner.
    /// No window, input, timer, capture operation, or nested native menu is created.
    struct DeferredPreview {
        active: Cell<bool>,
        sessions_started: Cell<u32>,
        ignored: Cell<u32>,
    }

    impl DeferredPreview {
        fn new(active: bool) -> Self {
            Self {
                active: Cell::new(active),
                sessions_started: Cell::new(u32::from(active)),
                ignored: Cell::new(0),
            }
        }

        fn receive(&self, pending: &mut u16, source: u16) {
            *pending |= activation_intent(source, self.active.get());
        }

        fn stop(&self) {
            self.active.set(false);
        }

        fn drain(&self, pending: u16) {
            dispatch_activation_intent(
                pending,
                || {
                    if !self.active.replace(true) {
                        self.sessions_started.set(self.sessions_started.get() + 1);
                    }
                },
                || self.ignored.set(self.ignored.get() + 1),
            );
        }
    }

    #[test]
    fn activation_received_in_a_paused_menu_does_not_restart_after_stop() {
        for source in [ACTIVATE, HOTKEY_ACTIVATE, TRAY_ACTIVATE] {
            let preview = DeferredPreview::new(true);
            let mut pending = 0;
            // Pausing a menu removes the timer but leaves the preview active.
            // Simulate nested dispatch, then selecting Stop before the outer loop resumes.
            preview.receive(&mut pending, source);
            preview.stop();
            preview.drain(pending);
            assert!(
                !preview.active.get(),
                "a queued repeat reopened the preview"
            );
            assert_eq!(preview.sessions_started.get(), 1);
            assert_eq!(preview.ignored.get(), 1);
        }
    }

    #[test]
    fn a_fresh_activation_after_menu_stop_can_start_the_next_session() {
        let preview = DeferredPreview::new(true);
        let mut old_request = 0;
        preview.receive(&mut old_request, HOTKEY_ACTIVATE);
        preview.stop();
        preview.drain(old_request);
        assert!(!preview.active.get());

        let mut fresh_request = 0;
        preview.receive(&mut fresh_request, HOTKEY_ACTIVATE);
        preview.drain(fresh_request);
        assert!(preview.active.get());
        assert_eq!(preview.sessions_started.get(), 2);
        assert_eq!(preview.ignored.get(), 1);
    }

    #[test]
    fn coalesced_activation_sources_start_only_one_session() {
        let preview = DeferredPreview::new(false);
        let mut pending = 0;
        for source in [ACTIVATE, HOTKEY_ACTIVATE, TRAY_ACTIVATE] {
            preview.receive(&mut pending, source);
        }
        preview.drain(pending);
        assert!(preview.active.get());
        assert_eq!(preview.sessions_started.get(), 1);
        assert_eq!(preview.ignored.get(), 0);
    }

    #[test]
    fn stop_or_desktop_change_cancels_a_coalesced_start() {
        for cancellation in [STOP_PREVIEW, ENVIRONMENT_CHANGED] {
            let preview = DeferredPreview::new(false);
            let mut pending = 0;
            preview.receive(&mut pending, ACTIVATE);
            preview.drain(pending | cancellation);
            assert!(!preview.active.get());
            assert_eq!(preview.sessions_started.get(), 0);
        }
    }
}
