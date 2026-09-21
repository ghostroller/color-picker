//! A one-time guide for explicit manual launches. Its marker is separate from
//! user settings, so showing help never rewrites a protected configuration.

use std::{cell::Cell, fs::OpenOptions, io, mem::size_of, path::Path};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, S_OK, WPARAM},
        UI::{
            Controls::{
                TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0,
                TD_INFORMATION_ICON, TDCBF_OK_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
                TDM_CLICK_BUTTON, TDN_CREATED, TDN_DESTROYED, TaskDialogIndirect,
            },
            WindowsAndMessaging::{IDCANCEL, IDOK, PostMessageW, SendMessageW},
        },
    },
    core::{HRESULT, PCWSTR, w},
};

use super::config_path::default_config_path;
use crate::app::diagnostics;

thread_local! {
    // Only this guide's exact HWND is retained; never search for or close an
    // unrelated application's dialog. Callbacks run on the host's UI thread.
    static ACTIVE_GUIDE: Cell<Option<HWND>> = const { Cell::new(None) };
    static GUIDE_CANCELLED: Cell<bool> = const { Cell::new(false) };
}

/// Release the guide's nested message loop when the host needs to exit or its
/// desktop becomes invalid. Programmatic dismissal never marks it as read.
pub(super) fn dismiss_pending() {
    GUIDE_CANCELLED.set(true);
    if let Some(hwnd) = ACTIVE_GUIDE.get() {
        let posted = unsafe {
            PostMessageW(
                Some(hwnd),
                TDM_CLICK_BUTTON.0 as u32,
                WPARAM(IDCANCEL.0 as usize),
                LPARAM(0),
            )
        };
        if posted.is_err() {
            // A full posted-message queue must not strand shutdown. No Rust
            // borrow is retained across the synchronous fallback callback.
            unsafe {
                SendMessageW(
                    hwnd,
                    TDM_CLICK_BUTTON.0 as u32,
                    Some(WPARAM(IDCANCEL.0 as usize)),
                    None,
                )
            };
        }
    }
}

unsafe extern "system" fn guide_callback(
    hwnd: HWND,
    notification: TASKDIALOG_NOTIFICATIONS,
    _wparam: WPARAM,
    _lparam: LPARAM,
    _data: isize,
) -> HRESULT {
    match notification {
        TDN_CREATED => {
            ACTIVE_GUIDE.set(Some(hwnd));
            // Cancellation can arrive through owner messages during creation,
            // before the dialog reports its own handle.
            if GUIDE_CANCELLED.get() {
                dismiss_pending();
            }
        }
        TDN_DESTROYED => ACTIVE_GUIDE.set(None),
        _ => {}
    }
    S_OK
}

fn show_guide(owner: HWND, text: &str) -> windows::core::Result<bool> {
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    GUIDE_CANCELLED.set(false);
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_OK_BUTTON,
        pszWindowTitle: w!("欢迎使用 Color Picker"),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_INFORMATION_ICON,
        },
        pszContent: PCWSTR(text.as_ptr()),
        pfCallback: Some(guide_callback),
        ..Default::default()
    };
    let mut button = 0;
    let result = unsafe { TaskDialogIndirect(&config, Some(&mut button), None, None) };
    // Also clear if native creation failed before TDN_DESTROYED was delivered.
    ACTIVE_GUIDE.set(None);
    result?;
    Ok(button == IDOK.0 && !GUIDE_CANCELLED.get())
}

pub(super) fn show_once(owner: HWND, shortcut: &str, registered: bool) {
    let marker = match default_config_path() {
        Ok(path) => path.with_file_name("welcome-v1.seen"),
        Err(error) => {
            diagnostics::event(format_args!("welcome.path_failed error={error}"));
            return;
        }
    };
    let result = offer_once(&marker, || {
        let shortcut = if registered {
            format!("按 {shortcut} 开始取色，或点击系统托盘中的滴管图标。")
        } else {
            format!("{shortcut} 当前注册失败。请点击托盘图标取色，或右键打开设置更换快捷键。")
        };
        let text = format!(
            "Color Picker 已在系统托盘运行。\n\n{shortcut}\n\n左键确认颜色；右键或 Esc 取消。\n向上滚动可冻结并放大，选取精确像素。\n取色后可复制 HEX、RGB、CSS RGB 或 HSL。\n\n右键托盘图标可打开设置或退出；图标也可能在托盘的隐藏区域中。\n此指引仅显示一次，登录 Windows 时启动不会弹出。"
        );
        let shown = match show_guide(owner, &text) {
            Ok(shown) => shown,
            Err(error) => {
                diagnostics::event(format_args!("welcome.show_failed error={error}"));
                false
            }
        };
        if shown {
            diagnostics::event(format_args!("welcome.acknowledged"));
        }
        shown
    });
    if let Err(error) = result {
        diagnostics::event(format_args!("welcome.marker_failed error={error}"));
    }
}

fn offer_once(marker: &Path, show: impl FnOnce() -> bool) -> io::Result<()> {
    if marker.try_exists()? || !show() {
        return Ok(());
    }
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Never truncate an existing marker, and only remember acknowledged help.
    match OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        Win32::{
            Foundation::{HINSTANCE, LRESULT},
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, KillTimer, RegisterClassW,
                SetTimer, UnregisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_TIMER,
                WNDCLASSW,
            },
        },
        core::Result,
    };

    thread_local! {
        static EXIT_RECEIVED: Cell<bool> = const { Cell::new(false) };
    }

    unsafe extern "system" fn test_owner_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_TIMER if ACTIVE_GUIDE.get().is_some() => {
                let _ = unsafe { KillTimer(Some(hwnd), wparam.0) };
                // Reproduce the installer's exact host-close request after
                // the real guide has entered its native message loop.
                let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
                LRESULT(0)
            }
            WM_CLOSE => {
                EXIT_RECEIVED.set(true);
                dismiss_pending();
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    struct TestOwner(HWND, HINSTANCE);

    impl TestOwner {
        const CLASS: PCWSTR = w!("ColorPicker.OnboardingLifecycleTest.v1");

        fn new() -> Result<Self> {
            let instance = unsafe { GetModuleHandleW(None)? }.into();
            let class = WNDCLASSW {
                lpfnWndProc: Some(test_owner_proc),
                hInstance: instance,
                lpszClassName: Self::CLASS,
                ..Default::default()
            };
            assert_ne!(unsafe { RegisterClassW(&class) }, 0);
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    Self::CLASS,
                    w!("Isolated onboarding lifecycle test"),
                    WINDOW_STYLE::default(),
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(instance),
                    None,
                )?
            };
            Ok(Self(hwnd, instance))
        }
    }

    impl Drop for TestOwner {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
                let _ = UnregisterClassW(Self::CLASS, Some(self.1));
            }
        }
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; briefly shows and programmatically dismisses its own guide"]
    fn host_exit_dismisses_the_real_guide_without_marking_it_read() {
        let directory = std::env::temp_dir().join(format!(
            "color-picker-guide-exit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let marker = directory.join("welcome-v1.seen");
        let owner = TestOwner::new().unwrap();
        EXIT_RECEIVED.set(false);
        assert_ne!(unsafe { SetTimer(Some(owner.0), 1, 30, None) }, 0);
        let started = std::time::Instant::now();
        offer_once(&marker, || {
            show_guide(owner.0, "此测试指引将通过所属窗口的退出请求自动关闭。").unwrap()
        })
        .unwrap();
        assert!(EXIT_RECEIVED.get());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(ACTIVE_GUIDE.get().is_none());
        assert!(!marker.exists());
        assert!(!directory.exists());
    }

    #[test]
    fn failed_or_suppressed_guide_does_not_consume_the_first_manual_launch() {
        let directory = std::env::temp_dir().join(format!(
            "color-picker-welcome-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let marker = directory.join("welcome-v1.seen");
        offer_once(&marker, || false).unwrap();
        assert!(!marker.exists());
        offer_once(&marker, || true).unwrap();
        assert!(marker.is_file());
        offer_once(&marker, || {
            panic!("acknowledged help must not appear again")
        })
        .unwrap();
        std::fs::remove_file(marker).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
