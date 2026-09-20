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
            Shell::{NIN_SELECT, NINF_KEY},
            WindowsAndMessaging::*,
        },
    },
    core::{Error, PCWSTR, Result, w},
};

use super::{
    hotkey,
    instance::{InstanceStatus, SingleInstance, instance_key},
    tray::{TrayCommand, TrayIcon},
};

pub const HOST_CLASS: PCWSTR = w!("ColorPicker.Host.v1");
pub const WM_ACTIVATE_PICKER: u32 = WM_APP + 1;
pub const WM_TRAY: u32 = WM_APP + 2;
/// Read-only diagnostics, enabled explicitly with --diagnostics. wParam selects
/// ready / activation count / hotkey registered / tray added / tray restorations.
pub const WM_DIAGNOSTICS: u32 = WM_APP + 3;

const ACTIVATE: u8 = 1;
const MENU: u8 = 2;
const EXIT: u8 = 4;
const RESTORE_TRAY: u8 = 8;
const ENVIRONMENT_CHANGED: u8 = 16;
// shellapi.h defines this expression; windows 0.62.2 does not emit that macro.
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

thread_local! {
    // Cells have no dynamic borrows, callbacks cannot alias the main-thread app.
    static PENDING: Cell<u8> = const { Cell::new(0) };
    static TASKBAR_MESSAGE: Cell<u32> = const { Cell::new(0) };
    static CALLBACK_FAILED: Cell<bool> = const { Cell::new(false) };
    static DIAGNOSTICS: Cell<bool> = const { Cell::new(false) };
    static READY: Cell<bool> = const { Cell::new(false) };
    static ACTIVATIONS: Cell<u32> = const { Cell::new(0) };
    static HOTKEY_REGISTERED: Cell<bool> = const { Cell::new(false) };
    static TRAY_ADDED: Cell<bool> = const { Cell::new(false) };
    static TRAY_RESTORATIONS: Cell<u32> = const { Cell::new(0) };
}

pub fn run(diagnostics: bool) -> Result<()> {
    let _instance = match SingleInstance::acquire()? {
        InstanceStatus::Primary(instance) => instance,
        InstanceStatus::Existing => return activate_existing(),
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
    let hotkey = match hotkey::register_default(window.0) {
        Ok(guard) => Some(guard),
        Err(error) => {
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
        Ok(guard) => Some(guard),
        Err(error) => {
            notify(
                &tray,
                "会话通知不可用",
                &format!("无法订阅锁屏和会话通知：{error}。当前仍可使用托盘。"),
            );
            None
        }
    };
    READY.set(true);

    let result = message_loop(&mut tray);
    READY.set(false);
    HOTKEY_REGISTERED.set(false);
    TRAY_ADDED.set(false);
    drop(session_notifications);
    drop(hotkey);
    drop(tray);
    drop(window);
    drop(class);
    result
}

fn message_loop(tray: &mut TrayIcon) -> Result<()> {
    loop {
        if CALLBACK_FAILED.get() {
            return Err(Error::new(E_FAIL, "Failed to wake the host message loop"));
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
                return Ok(());
            }
            if pending & RESTORE_TRAY != 0 {
                TRAY_ADDED.set(false);
                match tray.recreate() {
                    Ok(()) => {
                        TRAY_ADDED.set(true);
                        TRAY_RESTORATIONS.set(TRAY_RESTORATIONS.get().saturating_add(1));
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
            if pending & ACTIVATE != 0 {
                activate(tray);
            }
            if pending & MENU != 0 {
                match tray.show_menu()? {
                    Some(TrayCommand::Start) => activate(tray),
                    Some(TrayCommand::Settings) => notify(
                        tray,
                        "设置",
                        "当前为 M1 开发版本。快捷键和复制设置将在 M6 接入。",
                    ),
                    Some(TrayCommand::Exit) => return Ok(()),
                    None => {}
                }
            }
            // M1 owns no capture resources or monitor cache to invalidate.
            // M2/M6 will translate ENVIRONMENT_CHANGED into session cancellation.
        }
        let mut message = MSG::default();
        let status = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if status == -1 {
            return Err(Error::from_thread());
        }
        if status == 0 {
            return if CALLBACK_FAILED.get() {
                Err(Error::new(E_FAIL, "Failed to wake the host message loop"))
            } else {
                Ok(())
            };
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn activate(tray: &TrayIcon) {
    ACTIVATIONS.set(ACTIVATIONS.get().saturating_add(1));
    notify(
        tray,
        "color-picker 已激活",
        "M1 常驻外壳已就绪。实时取色将在 M2 接入。",
    );
}

fn notify(tray: &TrayIcon, title: &str, message: &str) {
    if let Err(error) = tray.notify(title, message) {
        show_error(&format!("{title}\n{message}\n\n托盘通知失败：{error}"));
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
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(Error::new(
        E_FAIL,
        "color-picker 已在运行，但现有实例尚未就绪；请稍后重试。",
    ))
}

fn enqueue(hwnd: HWND, action: u8) {
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
        WM_ACTIVATE_PICKER => enqueue(hwnd, ACTIVATE),
        WM_HOTKEY if wparam.0 == hotkey::DEFAULT_HOTKEY_ID as usize => enqueue(hwnd, ACTIVATE),
        WM_TRAY => match (lparam.0 as u32) & 0xffff {
            NIN_SELECT | NIN_KEYSELECT => enqueue(hwnd, ACTIVATE),
            WM_CONTEXTMENU => enqueue(hwnd, MENU),
            _ => {}
        },
        WM_DIAGNOSTICS if DIAGNOSTICS.get() => {
            return LRESULT(match wparam.0 {
                0 => READY.get() as isize,
                1 => ACTIVATIONS.get() as isize,
                2 => HOTKEY_REGISTERED.get() as isize,
                3 => TRAY_ADDED.get() as isize,
                4 => TRAY_RESTORATIONS.get() as isize,
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
