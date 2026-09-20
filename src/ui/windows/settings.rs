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
            Controls::{EM_GETLINECOUNT, ShowScrollBar},
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
            Input::KeyboardAndMouse::{EnableWindow, SetFocus},
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, Error, PCWSTR, Result, w},
};

use super::{
    drawing::dip,
    theme::{self, Font, Theme, Tone},
};
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
const TITLE: usize = 15;
const SUBTITLE: usize = 16;
const COPY_HEADING: usize = 17;
const STATUS: usize = 14;
const CLIENT_WIDTH: i32 = 480;
const CLIENT_HEIGHT: i32 = 480;
const STYLE: WINDOW_STYLE =
    WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);
const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_APPWINDOW.0 | WS_EX_CONTROLPARENT.0);
const PANELS: [RECT; 2] = [
    RECT {
        left: 24,
        top: 88,
        right: 456,
        bottom: 232,
    },
    RECT {
        left: 24,
        top: 244,
        right: 456,
        bottom: 364,
    },
];

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
    theme: Theme,
    status_tone: Cell<Tone>,
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
    title: HWND,
    subtitle: HWND,
    hotkey_label: HWND,
    ctrl: HWND,
    alt: HWND,
    shift: HWND,
    key_label: HWND,
    key: HWND,
    hint: HWND,
    copy_heading: HWND,
    format_label: HWND,
    format: HWND,
    auto_copy: HWND,
    apply: HWND,
    close: HWND,
    status: HWND,
}

impl Controls {
    fn handles(&self) -> [HWND; 16] {
        [
            self.title,
            self.subtitle,
            self.hotkey_label,
            self.ctrl,
            self.alt,
            self.shift,
            self.key_label,
            self.key,
            self.hint,
            self.copy_heading,
            self.format_label,
            self.format,
            self.auto_copy,
            self.apply,
            self.close,
            self.status,
        ]
    }
}

#[derive(Default)]
struct FontState {
    dpi: u32,
    small_line_height: i32,
    _fonts: Vec<Font>,
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
            theme: Theme::new()?,
            status_tone: Cell::new(Tone::Muted),
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
        theme::configure_window(hwnd);
        window.create_controls(config)?;
        window.place_initially(cursor)?;
        window.layout()?;
        window.update_default_style();
        let default_notice = if save_allowed {
            "点击“应用”保存更改。"
        } else {
            "当前配置只读，无法应用更改。"
        };
        let status = match (notice, save_allowed) {
            (Some(notice), false) => format!("{notice}\r\n{default_notice}"),
            (Some(notice), true) => notice.to_owned(),
            (None, _) => default_notice.to_owned(),
        };
        window.set_status(
            &status,
            if notice.is_some() || !save_allowed {
                Tone::Error
            } else {
                Tone::Muted
            },
        )?;
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
        self.set_status(text, if success { Tone::Success } else { Tone::Error })
    }

    fn set_status(&self, text: &str, tone: Tone) -> Result<()> {
        self.callback.status_tone.set(tone);
        let text = wide(text);
        unsafe {
            SetWindowTextW(self.controls.status, PCWSTR(text.as_ptr()))?;
        }
        self.update_status_scrollbar()
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
        self.controls.title = self.control(w!("STATIC"), "偏好设置", TITLE, label)?;
        self.controls.subtitle =
            self.control(w!("STATIC"), "自定义取色快捷键和复制方式", SUBTITLE, label)?;
        self.controls.hotkey_label = self.control(w!("STATIC"), "取色快捷键", 10, label)?;
        self.controls.ctrl = self.control(w!("BUTTON"), "Ctrl", CTRL, check)?;
        self.controls.alt = self.control(w!("BUTTON"), "Alt", ALT, check)?;
        self.controls.shift = self.control(w!("BUTTON"), "Shift", SHIFT, check)?;
        self.controls.key_label = self.control(w!("STATIC"), "主键", 11, label)?;
        self.controls.key = self.control(w!("COMBOBOX"), "", KEY, combo)?;
        self.controls.hint = self.control(
            w!("STATIC"),
            "至少选择 Ctrl 或 Alt\r\n支持字母、数字和 F1–F11",
            12,
            label,
        )?;
        self.controls.copy_heading = self.control(w!("STATIC"), "复制行为", COPY_HEADING, label)?;
        self.controls.format_label = self.control(w!("STATIC"), "默认格式", 13, label)?;
        self.controls.format = self.control(w!("COMBOBOX"), "", FORMAT, combo)?;
        self.controls.auto_copy = self.control(w!("BUTTON"), "取色后自动复制", AUTO_COPY, check)?;
        self.controls.status = self.control(
            w!("EDIT"),
            "",
            STATUS,
            WS_TABSTOP | WINDOW_STYLE((ES_READONLY | ES_MULTILINE | ES_AUTOVSCROLL) as u32),
        )?;
        self.controls.close = self.control(w!("BUTTON"), "关闭", CLOSE, button)?;
        self.controls.apply = self.control(w!("BUTTON"), "应用", APPLY, button)?;
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
            let body = Font::new(14, dpi, 400, false)?;
            let title = Font::new(20, dpi, 600, false)?;
            let heading = Font::new(14, dpi, 600, false)?;
            let small = Font::new(12, dpi, 400, false)?;
            let small_line_height = font_line_height(self.controls.status, small.0, dpi);
            for hwnd in self.controls.handles() {
                unsafe {
                    SendMessageW(
                        hwnd,
                        WM_SETFONT,
                        Some(WPARAM(body.0.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
            for (hwnd, font) in [
                (self.controls.title, title.0),
                (self.controls.hotkey_label, heading.0),
                (self.controls.copy_heading, heading.0),
                (self.controls.subtitle, small.0),
                (self.controls.hint, small.0),
                (self.controls.status, small.0),
            ] {
                unsafe {
                    SendMessageW(
                        hwnd,
                        WM_SETFONT,
                        Some(WPARAM(font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
            let old = self.font.replace(FontState {
                dpi,
                small_line_height,
                _fonts: vec![body, title, heading, small],
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
        place(self.controls.title, 24, 22, 432, 30)?;
        place(self.controls.subtitle, 24, 58, 432, 20)?;
        place(self.controls.hotkey_label, 40, 104, 396, 24)?;
        place(self.controls.ctrl, 40, 136, 88, 26)?;
        place(self.controls.alt, 140, 136, 88, 26)?;
        place(self.controls.shift, 240, 136, 100, 26)?;
        place(self.controls.key_label, 40, 183, 48, 24)?;
        place(self.controls.key, 92, 176, 120, 210)?;
        place(self.controls.hint, 228, 176, 212, 40)?;
        place(self.controls.copy_heading, 40, 260, 396, 24)?;
        place(self.controls.format_label, 40, 297, 108, 24)?;
        place(self.controls.format, 156, 290, 180, 160)?;
        place(self.controls.auto_copy, 40, 330, 396, 26)?;
        place(self.controls.status, 24, 376, 432, 32)?;
        place(self.controls.close, 264, 420, 88, 36)?;
        place(self.controls.apply, 364, 420, 92, 36)?;
        for hwnd in [self.controls.key, self.controls.format] {
            unsafe {
                SendMessageW(
                    hwnd,
                    CB_SETITEMHEIGHT,
                    Some(WPARAM(usize::MAX)),
                    Some(LPARAM(dip(24, dpi) as isize)),
                );
                SendMessageW(
                    hwnd,
                    CB_SETITEMHEIGHT,
                    Some(WPARAM(0)),
                    Some(LPARAM(dip(24, dpi) as isize)),
                );
            }
        }
        self.update_status_scrollbar()?;
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        Ok(())
    }

    fn update_status_scrollbar(&self) -> Result<()> {
        let line_height = self.font.borrow().small_line_height.max(1);
        // Measure wrapping at the full width. Long errors can scroll, while the
        // usual short hint remains a quiet, borderless line without a scrollbar.
        unsafe {
            ShowScrollBar(self.controls.status, SB_VERT, false)?;
        }
        let mut client = RECT::default();
        unsafe {
            GetClientRect(self.controls.status, &mut client)?;
        }
        let lines = unsafe { SendMessageW(self.controls.status, EM_GETLINECOUNT, None, None) }.0;
        let visible_lines = ((client.bottom - client.top) / line_height).max(1);
        if lines > visible_lines as isize {
            unsafe {
                ShowScrollBar(self.controls.status, SB_VERT, true)?;
            }
        }
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

fn font_line_height(hwnd: HWND, font: HFONT, dpi: u32) -> i32 {
    let dc = unsafe { GetDC(Some(hwnd)) };
    if dc.is_invalid() {
        return dip(16, dpi).max(1);
    }
    let old = unsafe { SelectObject(dc, HGDIOBJ(font.0)) };
    if old.is_invalid() {
        unsafe {
            ReleaseDC(Some(hwnd), dc);
        }
        return dip(16, dpi).max(1);
    }
    let mut metric = TEXTMETRICW::default();
    let measured = unsafe { GetTextMetricsW(dc, &mut metric) }.as_bool();
    unsafe {
        SelectObject(dc, old);
        ReleaseDC(Some(hwnd), dc);
    }
    if measured {
        metric.tmHeight.max(1)
    } else {
        dip(16, dpi).max(1)
    }
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
        WM_PAINT => state.theme.paint(hwnd, &PANELS),
        WM_ERASEBKGND => LRESULT(1),
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX => {
            let id = unsafe { GetDlgCtrlID(HWND(lparam.0 as *mut _)) } as usize;
            let (panel, tone) = match id {
                TITLE => (false, Tone::Text),
                SUBTITLE => (false, Tone::Muted),
                STATUS => (false, state.status_tone.get()),
                10 => (true, Tone::Accent),
                12 => (true, Tone::Muted),
                APPLY | CLOSE => (false, Tone::Text),
                _ => (true, Tone::Text),
            };
            state
                .theme
                .control_color(HDC(wparam.0 as *mut _), panel, tone)
        }
        WM_NOTIFY => theme::custom_draw(lparam, APPLY)
            .unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }),
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
