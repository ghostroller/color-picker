//! Main-thread notification icon and short-lived native context menus.

use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETFOCUS, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    NOTIFYICONDATAW_0, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, PostMessageW, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_NULL,
};
use windows::core::{Error, PCWSTR, Result};

use crate::app::i18n::tr;

use super::icon::AppIcon;

const ICON_ID: u32 = 1;
const COMMAND_START: usize = 1;
const COMMAND_SETTINGS: usize = 2;
const COMMAND_EXIT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Start,
    Stop,
    Settings,
    Exit,
}

/// Owns the shell registration and the HICON it borrows.
/// Drop this object before destroying its host window.
pub struct TrayIcon {
    data: NOTIFYICONDATAW,
    added: bool,
    icon: AppIcon,
}

impl TrayIcon {
    pub fn new(hwnd: HWND, callback_message: u32) -> Result<Self> {
        let icon = AppIcon::small_for_window(hwnd)?;
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: callback_message,
            hIcon: icon.handle(),
            Anonymous: NOTIFYICONDATAW_0 {
                uVersion: NOTIFYICON_VERSION_4,
            },
            ..Default::default()
        };
        copy_utf16(
            &mut data.szTip,
            tr(
                "Color Picker — 点击取色 · 右键打开菜单",
                "Color Picker — Click to pick · Right-click for menu",
            ),
        );
        let mut tray = Self {
            data,
            added: false,
            icon,
        };
        tray.add()?;
        Ok(tray)
    }

    /// Re-add after the host receives Explorer's registered TaskbarCreated message.
    pub fn recreate(&mut self) -> Result<()> {
        let icon = AppIcon::small_for_window(self.data.hWnd)?;
        self.remove();
        self.icon = icon;
        self.data.hIcon = self.icon.handle();
        self.add()
    }

    pub fn update_shortcut(&mut self, shortcut: &str, registered: bool) -> Result<()> {
        let text = crate::tr_format!(
            "Color Picker · {}\n点击取色 · 右键打开菜单",
            "Color Picker · {}\nClick to pick · Right-click for menu",
            shortcut_status(shortcut, registered)
        );
        copy_utf16(&mut self.data.szTip, &text);
        let mut data = self.data;
        data.uFlags = NIF_TIP | NIF_SHOWTIP;
        // SAFETY: data is initialized NOTIFYICONDATAW with bounded terminated strings; the registered host remains live.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
            Ok(())
        } else {
            Err(Error::new(
                E_FAIL,
                tr(
                    "无法更新托盘提示",
                    "Could not update the notification tooltip",
                ),
            ))
        }
    }

    fn add(&mut self) -> Result<()> {
        // Shell_NotifyIcon does not promise a useful GetLastError value.
        // SAFETY: The live host and owned icon outlive this registration; cbSize describes the initialized structure.
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &self.data) }.as_bool() {
            return Err(Error::new(
                E_FAIL,
                tr("无法添加托盘图标", "Could not add the notification icon"),
            ));
        }
        // SAFETY: The just-added registration owns no Rust pointer; its initialized version field requests v4.
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &self.data) }.as_bool() {
            // SAFETY: Undo only the registration just added, before its owned icon can be released.
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data) };
            return Err(Error::new(
                E_FAIL,
                tr(
                    "无法启用托盘图标",
                    "Could not enable notification icon version 4",
                ),
            ));
        }
        // Both operations must succeed before we advertise a usable registration.
        self.added = true;
        Ok(())
    }

    fn remove(&mut self) {
        if self.added {
            // SAFETY: Remove our own registration while host and icon remain alive.
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data) };
            self.added = false;
        }
    }

    /// Queue a shell notification. Windows may suppress it according to the
    /// user's notification settings; success only means the shell accepted it.
    pub fn notify(&self, title: &str, message: &str) -> Result<()> {
        if !self.added {
            return Err(Error::new(
                E_FAIL,
                tr(
                    "托盘图标尚未注册",
                    "The notification icon is not registered",
                ),
            ));
        }
        let mut data = self.data;
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        copy_utf16(&mut data.szInfoTitle, title);
        copy_utf16(&mut data.szInfo, message);
        // SAFETY: The shell synchronously copies initialized bounded UTF-16 notification fields from data.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
            Ok(())
        } else {
            Err(Error::new(
                E_FAIL,
                tr("无法发送托盘通知", "Could not queue the tray notification"),
            ))
        }
    }

    /// The host decodes the low 16 bits of a version-4 callback's lParam.
    /// Call for WM_CONTEXTMENU; NIN_SELECT/NIN_KEYSELECT may activate directly.
    pub fn show_menu(
        &self,
        preview_active: bool,
        shortcut: &str,
        registered: bool,
    ) -> Result<Option<TrayCommand>> {
        // Copy before TrackPopupMenu starts its nested message loop. This method
        // does not access the tray's Rust state while that loop is active.
        let data = self.data;
        // SAFETY: CreatePopupMenu returns a new menu immediately assigned to its unique guard.
        let menu = PopupMenu(unsafe { CreatePopupMenu()? });
        let status: Vec<u16> = crate::tr_format!(
            "快捷键：{}",
            "Shortcut: {}",
            shortcut_status(shortcut, registered)
        )
        .encode_utf16()
        .chain(Some(0))
        .collect();
        let start_label: Vec<u16> =
            crate::tr_format!("开始取色\t{shortcut}", "Pick a color\t{shortcut}")
                .encode_utf16()
                .chain(Some(0))
                .collect();
        let stop_label = wide(tr("停止预览", "Stop preview"));
        let settings_label = wide(tr("设置", "Settings"));
        let exit_label = wide(tr("退出", "Exit"));
        // SAFETY: menu owns the live HMENU; every label is terminated UTF-16 and AppendMenuW copies it synchronously.
        unsafe {
            AppendMenuW(
                menu.0,
                MF_STRING | MF_GRAYED,
                0,
                windows::core::PCWSTR(status.as_ptr()),
            )?;
            AppendMenuW(menu.0, MF_SEPARATOR, 0, None)?;
            let start_text = if preview_active {
                PCWSTR(stop_label.as_ptr())
            } else {
                windows::core::PCWSTR(start_label.as_ptr())
            };
            AppendMenuW(menu.0, MF_STRING, COMMAND_START, start_text)?;
            AppendMenuW(
                menu.0,
                MF_STRING,
                COMMAND_SETTINGS,
                PCWSTR(settings_label.as_ptr()),
            )?;
            AppendMenuW(menu.0, MF_STRING, COMMAND_EXIT, PCWSTR(exit_label.as_ptr()))?;
        }
        let mut point = POINT::default();
        // SAFETY: point is writable POINT storage and no pointer escapes this synchronous query.
        unsafe { GetCursorPos(&mut point)? };

        // A foreground owner lets clicks outside the menu dismiss it. The OS
        // controls whether foreground activation is allowed for this request.
        // SAFETY: The tray host is alive; this request passes a handle only and holds no callback-state borrow.
        let _ = unsafe { SetForegroundWindow(data.hWnd) };
        // SAFETY: menu and host stay alive during the nested loop; only copied tray data is used across reentry.
        let command = unsafe {
            TrackPopupMenu(
                menu.0,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                data.hWnd,
                None,
            )
        }
        .0;
        // Posting this benign message keeps repeated tray menus from immediately
        // dismissing after the foreground switch, as required by TrackPopupMenu.
        // SAFETY: WM_NULL has no pointer payload; the host owns its own message queue and remains live.
        let posted = unsafe { PostMessageW(Some(data.hWnd), WM_NULL, WPARAM(0), LPARAM(0)) };
        if command == 0 {
            // SAFETY: The initialized identity fields refer to this tray registration and no Rust pointer is retained.
            let _ = unsafe { Shell_NotifyIconW(NIM_SETFOCUS, &data) };
        }
        posted?;
        // With TPM_RETURNCMD, BOOL carries an item ID, not a success boolean.
        // Windows returns 0 for either cancellation or a tracking failure.
        match command as usize {
            0 => Ok(None),
            COMMAND_START => Ok(Some(if preview_active {
                TrayCommand::Stop
            } else {
                TrayCommand::Start
            })),
            COMMAND_SETTINGS => Ok(Some(TrayCommand::Settings)),
            COMMAND_EXIT => Ok(Some(TrayCommand::Exit)),
            _ => Err(Error::new(
                E_FAIL,
                tr(
                    "无法识别托盘菜单操作",
                    "Unexpected notification menu command",
                ),
            )),
        }
    }
}

fn shortcut_status(shortcut: &str, registered: bool) -> String {
    crate::tr_format!(
        "{shortcut}（{}）",
        "{shortcut} ({})",
        if registered {
            tr("已注册", "registered")
        } else {
            tr("不可用，请在设置中修改", "unavailable; change in Settings")
        }
    )
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.remove();
    }
}

struct PopupMenu(HMENU);

impl Drop for PopupMenu {
    fn drop(&mut self) {
        // Runs for successful selection, cancellation and every setup failure.
        // SAFETY: The PopupMenu guard uniquely owns this CreatePopupMenu result after all nested tracking returned.
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

/// Leave a terminator and truncate at a Unicode scalar boundary, so the shell
/// never receives only one half of an encoded surrogate pair.
fn copy_utf16<const N: usize>(buffer: &mut [u16; N], text: &str) {
    buffer.fill(0);
    let mut written = 0;
    for character in text.chars().take_while(|character| *character != '\0') {
        let mut units = [0; 2];
        let encoded = character.encode_utf16(&mut units);
        if written + encoded.len() >= N {
            break;
        }
        buffer[written..written + encoded.len()].copy_from_slice(encoded);
        written += encoded.len();
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_utf16, shortcut_status};

    #[test]
    fn shortcut_status_reports_the_current_chord_and_registration_failure() {
        use crate::app::i18n::{Language, set_language};
        set_language(Language::SimplifiedChinese);
        assert_eq!(shortcut_status("Alt + F7", true), "Alt + F7（已注册）");
        assert_eq!(
            shortcut_status("Ctrl + Shift + C", false),
            "Ctrl + Shift + C（不可用，请在设置中修改）"
        );
        set_language(Language::English);
        assert_eq!(shortcut_status("Alt + F7", true), "Alt + F7 (registered)");
        assert_eq!(
            shortcut_status("Ctrl + Shift + C", false),
            "Ctrl + Shift + C (unavailable; change in Settings)"
        );
    }

    #[test]
    fn tray_text_truncates_without_splitting_surrogates() {
        let mut buffer = [99; 4];
        copy_utf16(&mut buffer, "ab😀c");
        assert_eq!(buffer, [u16::from(b'a'), u16::from(b'b'), 0, 0]);
        copy_utf16(&mut buffer, "a😀c");
        assert_eq!(buffer, [u16::from(b'a'), 0xd83d, 0xde00, 0]);
        copy_utf16(&mut buffer, "a\0b");
        assert_eq!(buffer, [u16::from(b'a'), 0, 0, 0]);
    }
}
