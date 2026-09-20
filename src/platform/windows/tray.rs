//! Main-thread notification icon and short-lived native context menus.

use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETFOCUS, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    NOTIFYICONDATAW_0, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, IDI_APPLICATION, LoadIconW,
    MF_STRING, PostMessageW, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_NULL,
};
use windows::core::{Error, Result, w};

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

/// Owns the shell registration, but never owns the shared system HICON.
/// Drop this object before destroying its host window.
pub struct TrayIcon {
    data: NOTIFYICONDATAW,
    added: bool,
}

impl TrayIcon {
    pub fn new(hwnd: HWND, callback_message: u32) -> Result<Self> {
        // LoadIconW with no module returns a shared icon: do not DestroyIcon.
        let icon = unsafe { LoadIconW(None, IDI_APPLICATION)? };
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: callback_message,
            hIcon: icon,
            Anonymous: NOTIFYICONDATAW_0 {
                uVersion: NOTIFYICON_VERSION_4,
            },
            ..Default::default()
        };
        copy_utf16(&mut data.szTip, "color-picker");
        let mut tray = Self { data, added: false };
        tray.add()?;
        Ok(tray)
    }

    /// Re-add after the host receives Explorer's registered TaskbarCreated message.
    pub fn recreate(&mut self) -> Result<()> {
        self.remove();
        self.add()
    }

    fn add(&mut self) -> Result<()> {
        // Shell_NotifyIcon does not promise a useful GetLastError value.
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &self.data) }.as_bool() {
            return Err(Error::new(E_FAIL, "Could not add the notification icon"));
        }
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &self.data) }.as_bool() {
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data) };
            return Err(Error::new(
                E_FAIL,
                "Could not enable notification icon version 4",
            ));
        }
        // Both operations must succeed before we advertise a usable registration.
        self.added = true;
        Ok(())
    }

    fn remove(&mut self) {
        if self.added {
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
                "The notification icon is not registered",
            ));
        }
        let mut data = self.data;
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        copy_utf16(&mut data.szInfoTitle, title);
        copy_utf16(&mut data.szInfo, message);
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
            Ok(())
        } else {
            Err(Error::new(E_FAIL, "Could not queue the tray notification"))
        }
    }

    /// The host decodes the low 16 bits of a version-4 callback's lParam.
    /// Call for WM_CONTEXTMENU; NIN_SELECT/NIN_KEYSELECT may activate directly.
    pub fn show_menu(&self, preview_active: bool) -> Result<Option<TrayCommand>> {
        // Copy before TrackPopupMenu starts its nested message loop. This method
        // does not access the tray's Rust state while that loop is active.
        let data = self.data;
        let menu = PopupMenu(unsafe { CreatePopupMenu()? });
        unsafe {
            let start_text = if preview_active {
                w!("停止预览")
            } else {
                w!("开始取色")
            };
            AppendMenuW(menu.0, MF_STRING, COMMAND_START, start_text)?;
            AppendMenuW(menu.0, MF_STRING, COMMAND_SETTINGS, w!("设置"))?;
            AppendMenuW(menu.0, MF_STRING, COMMAND_EXIT, w!("退出"))?;
        }
        let mut point = POINT::default();
        unsafe { GetCursorPos(&mut point)? };

        // A foreground owner lets clicks outside the menu dismiss it. The OS
        // controls whether foreground activation is allowed for this request.
        let _ = unsafe { SetForegroundWindow(data.hWnd) };
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
        let posted = unsafe { PostMessageW(Some(data.hWnd), WM_NULL, WPARAM(0), LPARAM(0)) };
        if command == 0 {
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
            _ => Err(Error::new(E_FAIL, "Unexpected notification menu command")),
        }
    }
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
    use super::copy_utf16;

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
