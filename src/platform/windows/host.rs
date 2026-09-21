//! Hidden top-level host. Callbacks enqueue small, coalesced intentions only;
//! application work runs after DispatchMessage returns, with no borrowed window
//! data surviving a reentrant Win32 call. No HWND userdata allocation is needed.

use std::{
    cell::Cell,
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, E_FAIL, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, HANDLE, HINSTANCE,
            HWND, LPARAM, LRESULT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
                WTSUnRegisterSessionNotification,
            },
            Threading::{
                INFINITE, OpenMutexW, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW,
                SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
            },
        },
        UI::{
            Shell::{NIN_BALLOONHIDE, NIN_BALLOONSHOW, NIN_BALLOONTIMEOUT, NIN_SELECT, NINF_KEY},
            WindowsAndMessaging::*,
        },
    },
    core::{Error, HRESULT, PCWSTR, PWSTR, Result, w},
};

use crate::{
    app::{controller::PreviewController, diagnostics},
    core::format::{ColorFormat, format_color},
    ui::windows::{
        result::{ResultAction, ResultWindow, WM_RESULT_WAKE},
        settings::{SettingsAction, SettingsWindow, WM_SETTINGS_WAKE},
    },
};

use super::{
    input::WM_INPUT_WAKE,
    instance::{InstanceStatus, SingleInstance, instance_key},
    settings::SettingsRuntime,
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
const ACTIVATE: u32 = 1;
const MENU: u32 = 2;
const EXIT: u32 = 4;
const RESTORE_TRAY: u32 = 8;
const ENVIRONMENT_CHANGED: u32 = 16;
const HOTKEY_ACTIVATE: u32 = 32;
const TRAY_ACTIVATE: u32 = 64;
const BALLOON_SHOW: u32 = 128;
const BALLOON_HIDE: u32 = 256;
const BALLOON_TIMEOUT: u32 = 512;
const SAMPLE_TICK: u32 = 1024;
const STOP_PREVIEW: u32 = 2048;
const START_REQUEST: u32 = 4096;
const ACTIVATION_IGNORED: u32 = 8192;
const INPUT_WAKE: u32 = 16384;
const RESULT_WAKE: u32 = 32768;
const SETTINGS_WAKE: u32 = 65536;
const ONBOARDING_REQUEST: u32 = 131072;
// shellapi.h defines this expression; windows 0.62.2 does not emit that macro.
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

thread_local! {
    // Cells have no dynamic borrows, callbacks cannot alias the main-thread app.
    static PENDING: Cell<u32> = const { Cell::new(0) };
    static TASKBAR_MESSAGE: Cell<u32> = const { Cell::new(0) };
    static CALLBACK_FAILED: Cell<bool> = const { Cell::new(false) };
    static DIAGNOSTICS: Cell<bool> = const { Cell::new(false) };
    static READY: Cell<bool> = const { Cell::new(false) };
    static ACTIVATIONS: Cell<u32> = const { Cell::new(0) };
    static HOTKEY_REGISTERED: Cell<bool> = const { Cell::new(false) };
    static ACCEPTED_HOTKEY: Cell<i32> = const { Cell::new(0) };
    static ACTIVATION_BLOCKED: Cell<bool> = const { Cell::new(false) };
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
    run_with_startup(diagnostics, false)
}

/// Automatic launches should never start picking in an already running process.
pub fn run_with_startup(diagnostics: bool, startup: bool) -> Result<()> {
    run_with_launch_options(diagnostics, startup, true)
}

pub fn run_with_launch_options(
    diagnostics: bool,
    startup: bool,
    allow_onboarding: bool,
) -> Result<()> {
    let _instance = match SingleInstance::acquire()? {
        InstanceStatus::Primary(instance) => {
            diagnostics::event(format_args!("instance.primary"));
            instance
        }
        InstanceStatus::Existing => {
            if startup {
                diagnostics::event(format_args!("instance.startup_already_running"));
                return Ok(());
            }
            diagnostics::event(format_args!(
                "instance.existing logging_applies_to_this_process_only; exit the old instance from its tray and restart with --log-file to trace hotkeys"
            ));
            return activate_existing(allow_onboarding);
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
    let mut settings = SettingsRuntime::load(window.0);
    publish_hotkey_status(&settings);
    update_tray_shortcut(&mut tray, &settings);
    if let Some(notice) = settings.notice.as_deref() {
        notify(&tray, "设置提示", notice);
    }
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
        "host.ready hotkey_registered={} stage=M6; left click picks, wheel zooms, right click or Esc cancels",
        settings.hotkey_id().is_some()
    ));

    let result = message_loop(
        window.0,
        &mut tray,
        &mut settings,
        allow_onboarding && !startup,
    );
    READY.set(false);
    HOTKEY_REGISTERED.set(false);
    ACCEPTED_HOTKEY.set(0);
    TRAY_ADDED.set(false);
    drop(session_notifications);
    drop(settings);
    drop(tray);
    drop(window);
    drop(class);
    diagnostics::event(format_args!("host.stopped resources_released=true"));
    result
}

fn message_loop(
    hwnd: HWND,
    tray: &mut TrayIcon,
    settings: &mut SettingsRuntime,
    mut pending_intro: bool,
) -> Result<()> {
    let mut controller = PreviewController::new(hwnd);
    controller.set_appearance(settings.config.appearance);
    let mut result_window: Option<ResultWindow> = None;
    let mut settings_window: Option<SettingsWindow> = None;
    let mut exiting = false;
    let mut deferred_menu = false;
    loop {
        if CALLBACK_FAILED.get() {
            return Err(Error::new(E_FAIL, "Failed to wake the host message loop"));
        }
        // Broadcasts/exit already received must invalidate a pending candidate
        // before worker completion can promote it to Result and auto-copy it.
        let cancellation = PENDING.get() & (EXIT | ENVIRONMENT_CHANGED | STOP_PREVIEW);
        if cancellation & EXIT != 0 {
            exiting = true;
            result_window.take();
            settings_window.take();
        }
        if cancellation != 0 {
            controller.stop("pending_cancellation");
        }
        if let Err(error) = controller.process_input() {
            diagnostics::event(format_args!("input.failed error={error}"));
            if !exiting {
                notify(tray, "取色已停止", &error.to_string());
            }
        }
        publish_preview_status(&controller);
        if let Some(picked) = controller.take_result()
            && !exiting
        {
            result_window.take();
            match ResultWindow::new_with_behavior(
                picked,
                hwnd,
                settings.config.default_format,
                settings.config.auto_copy_on_pick,
                settings.config.appearance,
                settings.config.quick_pick,
            ) {
                Ok(window) => {
                    result_window = Some(window);
                    diagnostics::event(format_args!(
                        "{} resources_released_before_copy=true",
                        if settings.config.quick_pick {
                            "result.quick_copy_started"
                        } else {
                            "result.shown"
                        }
                    ));
                }
                Err(error) => {
                    controller.close_result()?;
                    notify(
                        tray,
                        "结果窗口无法显示",
                        &format!(
                            "已取色 {}。{error}",
                            format_color(picked.rgb, ColorFormat::Hex)
                        ),
                    );
                }
            }
        }
        if let Some(window) = result_window.as_ref() {
            match window.process_pending() {
                Ok(Some(action)) => {
                    result_window.take();
                    controller.close_result()?;
                    if action == ResultAction::PickAgain && !exiting && cancellation == 0 {
                        activate(tray, &mut controller, &mut result_window);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    diagnostics::event(format_args!("result.failed error={error}"));
                    notify(tray, "结果窗口操作失败", &error.to_string());
                }
            }
        }
        if let Some(window) = settings_window.as_ref() {
            match window.process_pending() {
                Ok(Some(SettingsAction::Apply(config))) => match settings.apply(hwnd, config) {
                    Ok(old_hotkey) => {
                        controller.set_appearance(settings.config.appearance);
                        publish_hotkey_status(settings);
                        update_tray_shortcut(tray, settings);
                        drop(old_hotkey);
                        window.show_status("设置已保存，关闭此窗口后生效。", true)?;
                    }
                    Err(error) => {
                        diagnostics::event(format_args!("config.apply_failed error={error}"));
                        window.show_status(&error, false)?;
                    }
                },
                Ok(Some(SettingsAction::Close)) => {
                    settings_window.take();
                    controller.close_settings()?;
                }
                Ok(None) => {}
                Err(error) => {
                    diagnostics::event(format_args!("settings.failed error={error}"));
                    window.show_status(&error.to_string(), false)?;
                }
            }
        }
        publish_preview_status(&controller);

        // A manual launch after an automatic launch can also request the first
        // guide. Defer it while capture owns input or settings holds a draft.
        if pending_intro && !controller.active() && settings_window.is_none() && !exiting {
            pending_intro = false;
            super::onboarding::show_once(
                hwnd,
                &settings.config.hotkey.label(),
                settings.hotkey_id().is_some(),
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
                result_window.take();
                settings_window.take();
                controller.stop("host_exit");
                publish_preview_status(&controller);
            }
            if exiting {
                continue;
            }
            if pending & ONBOARDING_REQUEST != 0 {
                pending_intro = true;
                if !controller.active() && settings_window.is_none() {
                    pending_intro = false;
                    super::onboarding::show_once(
                        hwnd,
                        &settings.config.hotkey.label(),
                        settings.hotkey_id().is_some(),
                    );
                }
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
                diagnostics::event(format_args!(
                    "hotkey.received chord={}",
                    settings.config.hotkey.label()
                ));
            }
            if pending & TRAY_ACTIVATE != 0 {
                diagnostics::event(format_args!("tray.activation_received"));
            }
            if pending & ACTIVATE != 0 {
                diagnostics::event(format_args!("instance.activation_received"));
            }
            dispatch_activation_intent(
                pending,
                || activate(tray, &mut controller, &mut result_window),
                || {
                    diagnostics::event(format_args!(
                        "activation.ignored reason=picker_or_settings_active"
                    ));
                    explain_settings_block(settings_window.as_ref());
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
                match tray.show_menu(
                    false,
                    &settings.config.hotkey.label(),
                    settings.hotkey_id().is_some(),
                )? {
                    Some(TrayCommand::Start) => {
                        diagnostics::event(format_args!("tray.menu_selected command=start"));
                        if !explain_settings_block(settings_window.as_ref()) {
                            activate(tray, &mut controller, &mut result_window);
                        }
                    }
                    Some(TrayCommand::Stop) => {
                        controller.stop("tray_menu");
                        diagnostics::event(format_args!("tray.menu_selected command=stop"));
                    }
                    Some(TrayCommand::Settings) => {
                        if let Some(window) = settings_window.as_ref() {
                            unsafe {
                                let _ = ShowWindow(window.hwnd(), SW_RESTORE);
                                let _ = SetForegroundWindow(window.hwnd());
                            }
                        } else {
                            result_window.take();
                            controller.close_result()?;
                            controller.open_settings()?;
                            publish_preview_status(&controller);
                            match SettingsWindow::new(
                                &settings.config,
                                hwnd,
                                settings.notice.as_deref(),
                                settings.save_allowed,
                            ) {
                                Ok(window) => settings_window = Some(window),
                                Err(error) => {
                                    controller.close_settings()?;
                                    notify(tray, "无法打开设置", &error.to_string());
                                }
                            }
                        }
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
        // A thread HANDLE signals after the worker has actually exited. This
        // closes the gap between its final notification and JoinHandle completion,
        // including Frozen/Finishing where no application timer is running.
        let status = if let Some(worker) = controller.input_wait_handle() {
            let wait = unsafe {
                MsgWaitForMultipleObjectsEx(
                    Some(&[worker]),
                    INFINITE,
                    QS_ALLINPUT,
                    MWMO_INPUTAVAILABLE,
                )
            };
            if wait == WAIT_OBJECT_0 {
                continue;
            }
            if wait == WAIT_FAILED {
                return Err(Error::from_thread());
            }
            if !unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                continue;
            }
            if message.message == WM_QUIT { 0 } else { 1 }
        } else {
            unsafe { GetMessageW(&mut message, None, 0, 0) }.0
        };
        if status == -1 {
            return Err(Error::from_thread());
        }
        if status == 0 {
            exiting = true;
            result_window.take();
            settings_window.take();
            controller.stop("quit_message");
            continue;
        }
        if let Some(window) = result_window.as_ref()
            && unsafe { IsDialogMessageW(window.hwnd(), &message) }.as_bool()
        {
            continue;
        }
        if let Some(window) = settings_window.as_ref()
            && (window.filter_key_message(&message)
                || unsafe { IsDialogMessageW(window.hwnd(), &message) }.as_bool())
        {
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn explain_settings_block(window: Option<&SettingsWindow>) -> bool {
    let Some(window) = window else {
        return false;
    };
    // Preserve the draft and keep capture stopped. This also restores a
    // minimized/covered settings window when a hotkey or second launch arrives.
    if let Err(error) = window.show_status(
        "设置窗口打开时暂停取色。请先应用需要保存的更改，再关闭此窗口。",
        false,
    ) {
        diagnostics::event(format_args!(
            "settings.activation_notice_failed error={error}"
        ));
    }
    unsafe {
        let _ = ShowWindow(window.hwnd(), SW_RESTORE);
        let _ = SetForegroundWindow(window.hwnd());
    }
    diagnostics::event(format_args!(
        "settings.activation_blocked window_restored=true"
    ));
    true
}

fn activate(
    tray: &TrayIcon,
    controller: &mut PreviewController,
    result_window: &mut Option<ResultWindow>,
) {
    if !controller.activation_allowed() {
        diagnostics::event(format_args!(
            "activation.ignored reason=picker_or_settings_active"
        ));
        return;
    }
    if !controller.active() {
        // Destroy the result and flush before sampling so it cannot become part
        // of the next pick, including a hotkey pressed over that same window.
        result_window.take();
        if let Err(error) = super::session::flush_composition() {
            notify(tray, "无法开始取色", &error.to_string());
            let _ = controller.close_result();
            return;
        }
    }
    match controller.start() {
        Ok(true) => {
            ACTIVATIONS.set(ACTIVATIONS.get().saturating_add(1));
            diagnostics::event(format_args!(
                "activation.handled count={} stage=M6",
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
    ACTIVATION_BLOCKED.set(!controller.activation_allowed());
    ACTIVE_TIMER.set(controller.timer_id());
    PREVIEW_SESSION.set(controller.session_id());
    SAMPLE_ATTEMPTS.set(controller.sample_attempts());
    PICKER_STATE.set(controller.state_code());
}

fn publish_hotkey_status(settings: &SettingsRuntime) {
    ACCEPTED_HOTKEY.set(settings.hotkey_id().unwrap_or(0));
    HOTKEY_REGISTERED.set(settings.hotkey_id().is_some());
}

fn update_tray_shortcut(tray: &mut TrayIcon, settings: &SettingsRuntime) {
    if let Err(error) = tray.update_shortcut(
        &settings.config.hotkey.label(),
        settings.hotkey_id().is_some(),
    ) {
        diagnostics::event(format_args!("tray.shortcut_update_failed error={error}"));
    }
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

/// Installer control: close only the resident executable at our own path.
/// This never acquires the single-instance marker or starts the application.
pub fn quit_current_installation() -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let key = instance_key()?;
    let title = wide(&key);
    let marker = wide(&format!("Local\\{key}.Instance"));
    let Some(hwnd) = wait_for_existing_host(&title, &marker, deadline)? else {
        return Ok(());
    };
    let mut pid = 0;
    if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) } == 0 {
        // The host can disappear between FindWindow and this lookup.
        return Ok(());
    }
    let process = match unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    } {
        Ok(handle) => OwnedHandle(handle),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if unsafe { WaitForSingleObject(process.0, 0) } == WAIT_OBJECT_0 {
        return Ok(());
    }
    let mut path = vec![0_u16; 32768];
    let mut length = path.len() as u32;
    if let Err(error) = unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
    } {
        return if unsafe { WaitForSingleObject(process.0, 0) } == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(error)
        };
    }
    let resident_path = PathBuf::from(OsString::from_wide(&path[..length as usize]));
    let own_path = std::env::current_exe()
        .map_err(|error| Error::new(E_FAIL, format!("无法确定安装程序路径：{error}")))?;
    if !same_installation(&own_path, &resident_path)
        .map_err(|error| Error::new(E_FAIL, format!("无法核对运行中的程序路径：{error}")))?
    {
        diagnostics::event(format_args!("instance.quit_skipped reason=different_path"));
        return Ok(());
    }
    // Keep the process HANDLE open while rechecking HWND ownership, so a stale
    // lookup cannot turn a reused process ID into a request to another process.
    let mut current_pid = 0;
    if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut current_pid)) } != 0 {
        if current_pid != pid {
            return Err(Error::new(E_FAIL, "程序窗口已改变，请重试退出操作"));
        }
        if let Err(error) = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }
            && unsafe { WaitForSingleObject(process.0, 0) } != WAIT_OBJECT_0
        {
            return Err(error);
        }
    }
    let remaining_ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis() as u32;
    match unsafe { WaitForSingleObject(process.0, remaining_ms) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(Error::new(
            E_FAIL,
            "color-picker 未在 10 秒内退出，请通过托盘退出后重试",
        )),
        _ => Err(Error::from_thread()),
    }
}

fn wait_for_existing_host(
    title: &[u16],
    marker: &[u16],
    deadline: Instant,
) -> Result<Option<HWND>> {
    loop {
        if !instance_marker_exists(marker)? {
            return Ok(None);
        }
        if let Ok(hwnd) = unsafe { FindWindowW(HOST_CLASS, PCWSTR(title.as_ptr())) } {
            return Ok(Some(hwnd));
        }
        // The first process owns its marker before it creates the host. Never
        // tell an installer that process is absent during this startup gap.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::new(
                E_FAIL,
                "color-picker 已在运行，但控制窗口未在 10 秒内就绪，请退出后重试",
            ));
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

fn instance_marker_exists(name: &[u16]) -> Result<bool> {
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) } {
        Ok(handle) => {
            // Close on every probe: retaining this handle could itself keep an
            // exited process's existence marker alive. Never acquire ownership.
            let _marker = OwnedHandle(handle);
            Ok(true)
        }
        Err(error) if error.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => Ok(false),
        Err(error) => Err(error),
    }
}

fn same_installation(own_path: &Path, resident_path: &Path) -> std::io::Result<bool> {
    // Resolve relative components, junctions and long/short path aliases before
    // comparison. Do not accept a matching executable name in another folder.
    Ok(own_path.canonicalize()? == resident_path.canonicalize()?)
}

fn activate_existing(allow_onboarding: bool) -> Result<()> {
    let title = wide(&instance_key()?);
    // A second launch can race the first process's window creation. This is a
    // bounded startup retry, never a resident timer or a background worker.
    for _ in 0..20 {
        if let Ok(hwnd) = unsafe { FindWindowW(HOST_CLASS, PCWSTR(title.as_ptr())) }
            && unsafe {
                PostMessageW(
                    Some(hwnd),
                    WM_ACTIVATE_PICKER,
                    WPARAM(usize::from(allow_onboarding)),
                    LPARAM(0),
                )
            }
            .is_ok()
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

fn enqueue(hwnd: HWND, action: u32) {
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
fn activation_intent(source: u32, preview_active: bool) -> u32 {
    source
        | if preview_active {
            ACTIVATION_IGNORED
        } else {
            START_REQUEST
        }
}

fn dispatch_activation_intent(pending: u32, start: impl FnOnce(), ignored: impl FnOnce()) {
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
        WM_RESULT_WAKE => enqueue(hwnd, RESULT_WAKE),
        WM_SETTINGS_WAKE => enqueue(hwnd, SETTINGS_WAKE),
        WM_TIMER if wparam.0 != 0 && wparam.0 == ACTIVE_TIMER.get() => {
            PENDING_TIMER.set(wparam.0);
            enqueue(hwnd, SAMPLE_TICK);
        }
        WM_STOP_PREVIEW if DIAGNOSTICS.get() => enqueue(hwnd, STOP_PREVIEW),
        WM_ACTIVATE_PICKER => enqueue(
            hwnd,
            activation_intent(ACTIVATE, ACTIVATION_BLOCKED.get())
                | if wparam.0 == 1 { ONBOARDING_REQUEST } else { 0 },
        ),
        WM_HOTKEY if ACCEPTED_HOTKEY.get() != 0 && wparam.0 == ACCEPTED_HOTKEY.get() as usize => {
            enqueue(
                hwnd,
                activation_intent(HOTKEY_ACTIVATE, ACTIVATION_BLOCKED.get()),
            )
        }
        WM_TRAY => match (lparam.0 as u32) & 0xffff {
            NIN_SELECT | NIN_KEYSELECT => enqueue(
                hwnd,
                activation_intent(TRAY_ACTIVATE, ACTIVATION_BLOCKED.get()),
            ),
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
        WM_CLOSE => {
            enqueue(hwnd, EXIT);
            // Return from TrackPopupMenu's nested loop so the owner can drain
            // input and release resources even while the tray menu is open.
            let _ = unsafe { EndMenu() };
        }
        WM_QUERYENDSESSION => return LRESULT(1),
        WM_ENDSESSION if wparam.0 != 0 => {
            enqueue(hwnd, EXIT);
            let _ = unsafe { EndMenu() };
        }
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

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
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

    #[test]
    #[ignore = "requires an interactive Windows desktop; restores its own settings window"]
    fn blocked_pick_restores_settings_and_explains_without_applying() {
        let config = crate::app::config::Config::default();
        let window = SettingsWindow::new(&config, HWND::default(), None, true).unwrap();
        let _ = unsafe { ShowWindow(window.hwnd(), SW_MINIMIZE) };
        assert!(unsafe { IsIconic(window.hwnd()) }.as_bool());
        assert!(explain_settings_block(Some(&window)));
        assert!(!unsafe { IsIconic(window.hwnd()) }.as_bool());
        assert_eq!(window.process_pending().unwrap(), None);
        let status = unsafe { GetDlgItem(Some(window.hwnd()), 14) }.unwrap();
        let mut text = [0_u16; 256];
        let length = unsafe { GetWindowTextW(status, &mut text) };
        assert!(String::from_utf16_lossy(&text[..length as usize]).contains("暂停取色"));
        assert!(!explain_settings_block(None));
    }

    #[test]
    fn quit_waits_for_a_starting_instance_without_creating_or_retaining_its_marker() {
        use windows::Win32::System::Threading::CreateMutexW;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let title = wide(&format!(
            "color-picker-test-control-{}-{unique}",
            std::process::id(),
        ));
        let marker = wide(&format!(
            "Local\\color-picker-test-control-{}-{unique}.Instance",
            std::process::id(),
        ));
        assert!(
            wait_for_existing_host(&title, &marker, Instant::now())
                .unwrap()
                .is_none()
        );
        assert!(!instance_marker_exists(&marker).unwrap());
        let running =
            OwnedHandle(unsafe { CreateMutexW(None, false, PCWSTR(marker.as_ptr())).unwrap() });
        assert!(instance_marker_exists(&marker).unwrap());
        // A known process without a host window must fail when its shared
        // deadline expires, rather than let installation proceed as if absent.
        assert!(wait_for_existing_host(&title, &marker, Instant::now()).is_err());
        drop(running);
        assert!(!instance_marker_exists(&marker).unwrap());
    }

    #[test]
    fn quit_matches_the_canonical_executable_path_not_another_installation() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "color-picker-host-paths-{}-{unique}",
            std::process::id(),
        ));
        let installed = root.join("安装目录");
        let portable = root.join("portable");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&installed).unwrap();
        std::fs::create_dir(&portable).unwrap();
        let own = installed.join("color-picker.exe");
        let other = portable.join("color-picker.exe");
        std::fs::write(&own, b"installed").unwrap();
        std::fs::write(&other, b"portable").unwrap();
        assert!(
            same_installation(&own, &installed.join("..\\安装目录\\color-picker.exe")).unwrap()
        );
        assert!(!same_installation(&own, &other).unwrap());
        assert!(same_installation(&own, &root.join("missing.exe")).is_err());
        std::fs::remove_file(own).unwrap();
        std::fs::remove_file(other).unwrap();
        std::fs::remove_dir(installed).unwrap();
        std::fs::remove_dir(portable).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

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

        fn receive(&self, pending: &mut u32, source: u32) {
            *pending |= activation_intent(source, self.active.get());
        }

        fn stop(&self) {
            self.active.set(false);
        }

        fn drain(&self, pending: u32) {
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
