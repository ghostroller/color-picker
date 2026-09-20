//! Native settings editor. Only Apply returns a validated draft; the host owns
//! persistence and the hotkey transaction. Callbacks never perform either task.

use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

use windows::{
    Win32::{
        Foundation::{
            E_FAIL, ERROR_CLASS_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HWND, LPARAM, LRESULT,
            POINT, RECT, SetLastError, WPARAM,
        },
        Graphics::{
            Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute},
            Gdi::*,
        },
        System::{LibraryLoader::GetModuleHandleW, SystemServices::SS_NOPREFIX},
        UI::{
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
            Input::KeyboardAndMouse::{EnableWindow, SetFocus},
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, Error, PCWSTR, Result, w},
};

use super::drawing::dip;
use crate::{
    app::config::{Config, HotkeyConfig},
    core::format::ColorFormat,
};

pub const WM_SETTINGS_WAKE: u32 = WM_APP + 12;
const CLASS: PCWSTR = w!("ColorPicker.Settings.v1");
const APPLY: usize = 1;
const CLOSE: usize = 2;
const CTRL: usize = 101;
const ALT: usize = 102;
const SHIFT: usize = 103;
const KEY: usize = 104;
const FORMAT: usize = 105;
const AUTO_COPY: usize = 106;
const CLIENT_WIDTH: i32 = 560;
const CLIENT_HEIGHT: i32 = 408;
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0);
const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_APPWINDOW.0 | WS_EX_CONTROLPARENT.0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsAction {
    Apply(Config),
    Close,
}

#[derive(Clone, Copy, Default)]
struct Pending {
    close: bool,
    apply: bool,
    layout: bool,
    dpi_rect: Option<RECT>,
    default_style: bool,
}

struct CallbackState {
    notify: HWND,
    pending: Cell<Pending>,
    wake_posted: Cell<bool>,
    wake_failed: Cell<bool>,
    closing: Cell<bool>,
    default_id: Cell<usize>,
}

impl CallbackState {
    fn queue(&self, update: impl FnOnce(&mut Pending)) {
        if self.closing.get() {
            return;
        }
        let mut pending = self.pending.get();
        update(&mut pending);
        self.pending.set(pending);
        if !self.wake_posted.replace(true)
            && unsafe { PostMessageW(Some(self.notify), WM_SETTINGS_WAKE, WPARAM(0), LPARAM(0)) }
                .is_err()
        {
            self.wake_posted.set(false);
            self.wake_failed.set(true);
        }
    }
}

#[derive(Default)]
struct Controls {
    hotkey_label: HWND,
    ctrl: HWND,
    alt: HWND,
    shift: HWND,
    key_label: HWND,
    key: HWND,
    hint: HWND,
    format_label: HWND,
    format: HWND,
    auto_copy: HWND,
    apply: HWND,
    close: HWND,
    status: HWND,
}

impl Controls {
    fn handles(&self) -> [HWND; 13] {
        [
            self.hotkey_label,
            self.ctrl,
            self.alt,
            self.shift,
            self.key_label,
            self.key,
            self.hint,
            self.format_label,
            self.format,
            self.auto_copy,
            self.apply,
            self.close,
            self.status,
        ]
    }
}

struct Font(HFONT);
impl Drop for Font {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

#[derive(Default)]
struct FontState {
    dpi: u32,
    _font: Option<Font>,
}

pub struct SettingsWindow {
    hwnd: HWND,
    callback: Box<CallbackState>,
    controls: Controls,
    font: RefCell<FontState>,
    schema_version: u32,
    save_allowed: bool,
    keys: Vec<String>,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl SettingsWindow {
    pub fn new(
        config: &Config,
        notify: HWND,
        notice: Option<&str>,
        save_allowed: bool,
    ) -> Result<Self> {
        let instance = unsafe { GetModuleHandleW(None)? }.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
            hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
            lpszClassName: CLASS,
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0
            && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
        {
            return Err(Error::from_thread());
        }
        let callback = Box::new(CallbackState {
            notify,
            pending: Cell::new(Pending::default()),
            wake_posted: Cell::new(false),
            wake_failed: Cell::new(false),
            closing: Cell::new(false),
            default_id: Cell::new(if save_allowed { APPLY } else { CLOSE }),
        });
        let pointer = callback.as_ref() as *const CallbackState;
        let mut cursor = POINT::default();
        unsafe { GetCursorPos(&mut cursor)? };
        let hwnd = unsafe {
            CreateWindowExW(
                EX_STYLE,
                CLASS,
                w!("设置 — Color Picker"),
                STYLE,
                cursor.x,
                cursor.y,
                1,
                1,
                Some(notify),
                None,
                Some(instance),
                Some(pointer.cast()),
            )?
        };
        let mut window = Self {
            hwnd,
            callback,
            controls: Controls::default(),
            font: RefCell::new(FontState::default()),
            schema_version: config.schema_version,
            save_allowed,
            keys: key_choices(),
            _thread_affinity: PhantomData,
        };
        let disable_transitions = BOOL::from(true);
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&disable_transitions as *const BOOL).cast(),
                size_of::<BOOL>() as u32,
            )?;
        }
        window.create_controls(config)?;
        window.place_initially(cursor)?;
        window.layout()?;
        window.update_default_style();
        let default_notice = if save_allowed {
            "更改仅在点击“应用”后保存。关闭窗口会放弃尚未应用的更改。"
        } else {
            "当前配置只读，无法应用更改。"
        };
        let status = match (notice, save_allowed) {
            (Some(notice), false) => format!("{notice}\r\n{default_notice}"),
            (Some(notice), true) => notice.to_owned(),
            (None, _) => default_notice.to_owned(),
        };
        window.show_status(&status, false)?;
        unsafe {
            let _ = EnableWindow(window.controls.apply, save_allowed);
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(window.controls.ctrl));
        }
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn show_status(&self, text: &str, success: bool) -> Result<()> {
        let message = if success {
            format!("已应用：{text}")
        } else {
            text.to_owned()
        };
        let text = wide(&message);
        unsafe { SetWindowTextW(self.controls.status, PCWSTR(text.as_ptr())) }
    }

    /// The host commits Apply transactionally, then calls show_status. A failed
    /// save leaves the draft controls intact, so the user can correct and retry.
    pub fn process_pending(&self) -> Result<Option<SettingsAction>> {
        self.callback.wake_posted.set(false);
        if self.callback.wake_failed.replace(false) {
            return Err(Error::new(E_FAIL, "Could not queue settings-window work"));
        }
        let pending = self.callback.pending.take();
        if pending.close {
            self.callback.closing.set(true);
            return Ok(Some(SettingsAction::Close));
        }
        if let Some(rect) = pending.dpi_rect {
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right.saturating_sub(rect.left),
                    rect.bottom.saturating_sub(rect.top),
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )?;
            }
        }
        if pending.layout {
            self.layout()?;
        }
        if pending.default_style {
            self.update_default_style();
        }
        if pending.apply && self.save_allowed {
            match self.read_config() {
                Ok(config) => match config.validate() {
                    Ok(()) => return Ok(Some(SettingsAction::Apply(config))),
                    Err(error) => self.show_status(&error.to_string(), false)?,
                },
                Err(error) => self.show_status(&error.to_string(), false)?,
            }
        }
        Ok(None)
    }

    fn create_controls(&mut self, config: &Config) -> Result<()> {
        let label = WINDOW_STYLE(SS_NOPREFIX.0);
        let check = WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32);
        let combo = WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32);
        let button = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32);
        self.controls.hotkey_label = self.control(w!("STATIC"), "取色快捷键", 10, label)?;
        self.controls.ctrl = self.control(w!("BUTTON"), "Ctrl(&T)", CTRL, check)?;
        self.controls.alt = self.control(w!("BUTTON"), "Alt(&L)", ALT, check)?;
        self.controls.shift = self.control(w!("BUTTON"), "Shift(&S)", SHIFT, check)?;
        self.controls.key_label =
            self.control(w!("STATIC"), "主键(&K)", 11, WINDOW_STYLE::default())?;
        self.controls.key = self.control(w!("COMBOBOX"), "", KEY, combo)?;
        self.controls.hint = self.control(
            w!("STATIC"),
            "至少选择 Ctrl 或 Alt。\r\n支持字母、数字和 F1–F11。",
            12,
            label,
        )?;
        self.controls.format_label = self.control(
            w!("STATIC"),
            "默认复制格式(&F)",
            13,
            WINDOW_STYLE::default(),
        )?;
        self.controls.format = self.control(w!("COMBOBOX"), "", FORMAT, combo)?;
        self.controls.auto_copy =
            self.control(w!("BUTTON"), "取色后自动复制默认格式(&A)", AUTO_COPY, check)?;
        self.controls.apply = self.control(w!("BUTTON"), "应用(&Y)", APPLY, button)?;
        self.controls.close = self.control(w!("BUTTON"), "关闭(&C)", CLOSE, button)?;
        self.controls.status = self.control(
            w!("EDIT"),
            "",
            14,
            WS_TABSTOP
                | WS_VSCROLL
                | WINDOW_STYLE((ES_READONLY | ES_MULTILINE | ES_AUTOVSCROLL) as u32),
        )?;
        for key in &self.keys {
            append_choice(self.controls.key, key)?;
        }
        for format in ColorFormat::ALL {
            append_choice(self.controls.format, format.label())?;
        }
        let key_index = self
            .keys
            .iter()
            .position(|key| key == &config.hotkey.key)
            .ok_or_else(|| {
                Error::new(
                    E_FAIL,
                    "Configured hotkey is outside the supported key list",
                )
            })?;
        let format_index = ColorFormat::ALL
            .iter()
            .position(|format| *format == config.default_format)
            .ok_or_else(|| Error::new(E_FAIL, "Configured color format is not supported"))?;
        set_choice(self.controls.key, key_index)?;
        set_choice(self.controls.format, format_index)?;
        set_checked(self.controls.ctrl, config.hotkey.ctrl);
        set_checked(self.controls.alt, config.hotkey.alt);
        set_checked(self.controls.shift, config.hotkey.shift);
        set_checked(self.controls.auto_copy, config.auto_copy_on_pick);
        Ok(())
    }

    fn control(&self, class: PCWSTR, text: &str, id: usize, style: WINDOW_STYLE) -> Result<HWND> {
        let text = wide(text);
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                Some(self.hwnd),
                Some(HMENU(id as *mut _)),
                Some(GetModuleHandleW(None)?.into()),
                None,
            )
        }
    }

    fn read_config(&self) -> Result<Config> {
        let key_index = selected_choice(self.controls.key)?;
        let format_index = selected_choice(self.controls.format)?;
        let key = self
            .keys
            .get(key_index)
            .ok_or_else(|| Error::new(E_FAIL, "请选择有效的主键"))?
            .clone();
        let default_format = *ColorFormat::ALL
            .get(format_index)
            .ok_or_else(|| Error::new(E_FAIL, "请选择有效的颜色格式"))?;
        Ok(Config {
            schema_version: self.schema_version,
            hotkey: HotkeyConfig {
                ctrl: checked(self.controls.ctrl),
                alt: checked(self.controls.alt),
                shift: checked(self.controls.shift),
                key,
            },
            default_format,
            auto_copy_on_pick: checked(self.controls.auto_copy),
        })
    }

    fn dpi(&self) -> Result<u32> {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 || dpi > 9600 {
            Err(Error::new(
                E_FAIL,
                "Could not determine settings-window DPI",
            ))
        } else {
            Ok(dpi)
        }
    }

    fn place_initially(&self, cursor: POINT) -> Result<()> {
        let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return Err(Error::from_thread());
        }
        let work = info.rcWork;
        let center_x = ((i64::from(work.left) + i64::from(work.right)) / 2) as i32;
        let center_y = ((i64::from(work.top) + i64::from(work.bottom)) / 2) as i32;
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                center_x,
                center_y,
                1,
                1,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )?;
        }
        let dpi = self.dpi()?;
        let mut outer = RECT {
            right: dip(CLIENT_WIDTH, dpi),
            bottom: dip(CLIENT_HEIGHT, dpi),
            ..Default::default()
        };
        unsafe {
            AdjustWindowRectExForDpi(&mut outer, STYLE, false, EX_STYLE, dpi)?;
        }
        let width = outer.right - outer.left;
        let height = outer.bottom - outer.top;
        let x = (i64::from(center_x) - i64::from(width) / 2).max(i64::from(work.left)) as i32;
        let y = (i64::from(center_y) - i64::from(height) / 2).max(i64::from(work.top)) as i32;
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )?;
        }
        let mut pending = self.callback.pending.get();
        pending.dpi_rect = None;
        self.callback.pending.set(pending);
        Ok(())
    }

    fn layout(&self) -> Result<()> {
        let dpi = self.dpi()?;
        if self.font.borrow().dpi != dpi {
            let font = Font(unsafe {
                CreateFontW(
                    -dip(14, dpi),
                    0,
                    0,
                    0,
                    400,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    0,
                    w!("Segoe UI"),
                )
            });
            if font.0.0.is_null() {
                return Err(Error::new(E_FAIL, "Could not create settings font"));
            }
            for hwnd in self.controls.handles() {
                unsafe {
                    SendMessageW(
                        hwnd,
                        WM_SETFONT,
                        Some(WPARAM(font.0.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
            let old = self.font.replace(FontState {
                dpi,
                _font: Some(font),
            });
            drop(old);
        }
        let place = |hwnd, x, y, width, height| unsafe {
            MoveWindow(
                hwnd,
                dip(x, dpi),
                dip(y, dpi),
                dip(width, dpi),
                dip(height, dpi),
                true,
            )
        };
        place(self.controls.hotkey_label, 20, 20, 500, 24)?;
        place(self.controls.ctrl, 20, 54, 92, 28)?;
        place(self.controls.alt, 124, 54, 92, 28)?;
        place(self.controls.shift, 228, 54, 104, 28)?;
        place(self.controls.key_label, 20, 106, 76, 26)?;
        place(self.controls.key, 100, 100, 180, 230)?;
        place(self.controls.hint, 300, 100, 240, 48)?;
        place(self.controls.format_label, 20, 166, 170, 26)?;
        place(self.controls.format, 200, 160, 180, 160)?;
        place(self.controls.auto_copy, 20, 214, 520, 28)?;
        place(self.controls.apply, 20, 264, 120, 34)?;
        place(self.controls.close, 154, 264, 120, 34)?;
        place(self.controls.status, 20, 316, 520, 72)?;
        Ok(())
    }

    fn update_default_style(&self) {
        for (id, hwnd) in [(APPLY, self.controls.apply), (CLOSE, self.controls.close)] {
            let style = if id == self.callback.default_id.get() {
                BS_DEFPUSHBUTTON
            } else {
                BS_PUSHBUTTON
            };
            unsafe {
                SendMessageW(
                    hwnd,
                    BM_SETSTYLE,
                    Some(WPARAM(style as usize)),
                    Some(LPARAM(1)),
                );
            }
        }
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        self.callback.closing.set(true);
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn key_choices() -> Vec<String> {
    ('A'..='Z')
        .chain('0'..='9')
        .map(|key| key.to_string())
        .chain((1..=11).map(|key| format!("F{key}")))
        .collect()
}

fn append_choice(hwnd: HWND, text: &str) -> Result<()> {
    let text = wide(text);
    let result = unsafe {
        SendMessageW(
            hwnd,
            CB_ADDSTRING,
            None,
            Some(LPARAM(text.as_ptr() as isize)),
        )
    };
    if result.0 < 0 {
        Err(Error::new(E_FAIL, "Could not populate settings choices"))
    } else {
        Ok(())
    }
}

fn set_choice(hwnd: HWND, index: usize) -> Result<()> {
    if unsafe { SendMessageW(hwnd, CB_SETCURSEL, Some(WPARAM(index)), None) }.0 < 0 {
        Err(Error::new(E_FAIL, "Could not select settings value"))
    } else {
        Ok(())
    }
}

fn selected_choice(hwnd: HWND) -> Result<usize> {
    let result = unsafe { SendMessageW(hwnd, CB_GETCURSEL, None, None) }.0;
    usize::try_from(result).map_err(|_| Error::new(E_FAIL, "请选择有效的选项"))
}

fn set_checked(hwnd: HWND, value: bool) {
    unsafe {
        SendMessageW(hwnd, BM_SETCHECK, Some(WPARAM(usize::from(value))), None);
    }
}

fn checked(hwnd: HWND) -> bool {
    unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None) }.0 == 1
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        dispatch(hwnd, message, wparam, lparam)
    }))
    .unwrap_or_else(|_| std::process::abort())
}

unsafe fn dispatch(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe {
            SetLastError(ERROR_SUCCESS);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        if unsafe { GetLastError() } != ERROR_SUCCESS {
            return LRESULT(0);
        }
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const CallbackState;
    let Some(state) = (unsafe { pointer.as_ref() }) else {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_CLOSE => {
            state.queue(|pending| pending.close = true);
            LRESULT(0)
        }
        WM_COMMAND if (wparam.0 >> 16) as u32 == BN_CLICKED => {
            match wparam.0 & 0xffff {
                CLOSE => state.queue(|pending| pending.close = true),
                APPLY => state.queue(|pending| pending.apply = true),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(rect) = unsafe { (lparam.0 as *const RECT).as_ref() } {
                let rect = *rect;
                state.queue(|pending| {
                    pending.dpi_rect = Some(rect);
                    pending.layout = true;
                });
            }
            LRESULT(0)
        }
        WM_SIZE => {
            state.queue(|pending| pending.layout = true);
            LRESULT(0)
        }
        DM_GETDEFID => LRESULT(((DC_HASDEFID as usize) << 16 | state.default_id.get()) as isize),
        DM_SETDEFID => {
            state.default_id.set(wparam.0);
            state.queue(|pending| pending.default_style = true);
            LRESULT(1)
        }
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
