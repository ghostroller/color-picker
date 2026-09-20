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
            Controls::{
                EM_GETLINECOUNT, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
                ShowScrollBar, TBM_SETPAGESIZE, TBM_SETPOS, TBM_SETRANGEMAX, TBM_SETRANGEMIN,
                TBS_NOTICKS, TRACKBAR_CLASSW,
            },
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
            Input::KeyboardAndMouse::{
                EnableWindow, SetFocus, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SHIFT, VK_TAB,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
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
    app::config::{
        AppearanceConfig, Config, HotkeyConfig, MAX_BACKGROUND_TRANSPARENCY_PERCENT,
        MAX_BORDER_WIDTH_DIP,
    },
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
const BORDER_WIDTH: usize = 107;
const BACKGROUND_TRANSPARENCY: usize = 108;
const TITLE: usize = 15;
const SUBTITLE: usize = 16;
const COPY_HEADING: usize = 17;
const USAGE_HEADING: usize = 18;
const USAGE_HINT: usize = 19;
const APPEARANCE_HEADING: usize = 20;
const BORDER_LABEL: usize = 21;
const BORDER_VALUE: usize = 22;
const TRANSPARENCY_LABEL: usize = 23;
const TRANSPARENCY_VALUE: usize = 24;
const STATUS: usize = 14;
const CLIENT_WIDTH: i32 = 480;
const CLIENT_HEIGHT: i32 = 588;
const KEY_SUBCLASS: usize = 1;
// CommCtrl.h aliases TBM_GETPOS to WM_USER; windows-rs omits this alias.
const TBM_GETPOS: u32 = WM_USER;
const STYLE: WINDOW_STYLE =
    WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);
const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_APPWINDOW.0 | WS_EX_CONTROLPARENT.0);
const PANELS: [RECT; 3] = [
    RECT {
        left: 24,
        top: 88,
        right: 456,
        bottom: 220,
    },
    RECT {
        left: 24,
        top: 232,
        right: 456,
        bottom: 338,
    },
    RECT {
        left: 24,
        top: 350,
        right: 456,
        bottom: 466,
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
    capture: bool,
    focus_key: bool,
    appearance: bool,
}

#[derive(Clone, Copy)]
enum CaptureHint {
    Ready,
    Listening,
    Invalid,
    Accepted,
    Cancelled,
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
    selected_key: Cell<u32>,
    recording: Cell<bool>,
    capture_hint: Cell<CaptureHint>,
    suppressed_keys: Cell<[u64; 4]>,
}

impl CallbackState {
    fn suppresses(&self, key: u32) -> bool {
        key < 256 && self.suppressed_keys.get()[key as usize / 64] & (1 << (key % 64)) != 0
    }

    fn suppress(&self, key: u32, pressed: bool) {
        if key < 256 {
            let mut keys = self.suppressed_keys.get();
            let mask = 1 << (key % 64);
            if pressed {
                keys[key as usize / 64] |= mask;
            } else {
                keys[key as usize / 64] &= !mask;
            }
            self.suppressed_keys.set(keys);
        }
    }

    fn begin_capture(&self) {
        self.recording.set(true);
        self.capture_hint.set(CaptureHint::Listening);
        self.queue(|pending| {
            pending.capture = true;
            pending.focus_key = true;
        });
    }

    fn cancel_capture(&self) {
        if self.recording.replace(false) {
            self.capture_hint.set(CaptureHint::Cancelled);
            self.queue(|pending| pending.capture = true);
        }
    }

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
    appearance_heading: HWND,
    border_label: HWND,
    border_width: HWND,
    border_value: HWND,
    transparency_label: HWND,
    background_transparency: HWND,
    transparency_value: HWND,
    usage_heading: HWND,
    usage_hint: HWND,
    apply: HWND,
    close: HWND,
    status: HWND,
}

impl Controls {
    fn handles(&self) -> [HWND; 25] {
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
            self.appearance_heading,
            self.border_label,
            self.border_width,
            self.border_value,
            self.transparency_label,
            self.background_transparency,
            self.transparency_value,
            self.usage_heading,
            self.usage_hint,
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
    _thread_affinity: PhantomData<Rc<()>>,
}

impl SettingsWindow {
    pub fn new(
        config: &Config,
        notify: HWND,
        notice: Option<&str>,
        save_allowed: bool,
    ) -> Result<Self> {
        let selected_key = (0x30..=0x5a)
            .chain(0x70..=0x7a)
            .find(|key| key_name(*key).as_deref() == Some(config.hotkey.key.as_str()))
            .ok_or_else(|| {
                Error::new(
                    E_FAIL,
                    "Configured hotkey is outside the supported key list",
                )
            })?;
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
            selected_key: Cell::new(selected_key),
            recording: Cell::new(false),
            capture_hint: Cell::new(CaptureHint::Ready),
            suppressed_keys: Cell::new([0; 4]),
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

    /// Run before IsDialogMessage: a key consumed during recording stays owned
    /// until release, including after Tab/click moves focus to another control.
    /// This is window-local message filtering, not a global keyboard hook.
    pub fn filter_key_message(&self, message: &MSG) -> bool {
        if message.hwnd != self.hwnd && !unsafe { IsChild(self.hwnd, message.hwnd) }.as_bool() {
            return false;
        }
        let key = message.wParam.0 as u32;
        if !self.callback.suppresses(key) {
            return false;
        }
        match message.message {
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if message.lParam.0 & (1 << 30) == 0 {
                    // Release may have gone to a different app. A fresh press
                    // is a new gesture and must never be lost to stale state.
                    self.callback.suppress(key, false);
                    false
                } else {
                    true
                }
            }
            WM_KEYUP | WM_SYSKEYUP => {
                self.callback.suppress(key, false);
                true
            }
            _ => false,
        }
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
            self.callback.recording.set(false);
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
        if pending.capture {
            self.update_capture_text()?;
        }
        if pending.focus_key && self.callback.recording.get() {
            unsafe { SetFocus(Some(self.controls.key))? };
        }
        if pending.appearance {
            self.update_appearance_text()?;
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
        if !unsafe {
            InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_BAR_CLASSES,
            })
        }
        .as_bool()
        {
            return Err(Error::new(E_FAIL, "Could not initialize settings sliders"));
        }
        let label = WINDOW_STYLE(SS_NOPREFIX.0);
        let check = WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32);
        let combo = WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32);
        let button = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32);
        let slider = WS_TABSTOP | WINDOW_STYLE(TBS_NOTICKS);
        self.controls.title = self.control(w!("STATIC"), "偏好设置", TITLE, label)?;
        self.controls.subtitle = self.control(
            w!("STATIC"),
            "自定义快捷键、复制方式和取色外观",
            SUBTITLE,
            label,
        )?;
        self.controls.hotkey_label = self.control(w!("STATIC"), "取色快捷键", 10, label)?;
        self.controls.ctrl = self.control(w!("BUTTON"), "Ctrl", CTRL, check)?;
        self.controls.alt = self.control(w!("BUTTON"), "Alt", ALT, check)?;
        self.controls.shift = self.control(w!("BUTTON"), "Shift", SHIFT, check)?;
        self.controls.key_label = self.control(w!("STATIC"), "主键", 11, label)?;
        self.controls.key = self.control(w!("BUTTON"), "", KEY, button)?;
        if !unsafe {
            SetWindowSubclass(
                self.controls.key,
                Some(key_button_proc),
                KEY_SUBCLASS,
                self.callback.as_ref() as *const CallbackState as usize,
            )
        }
        .as_bool()
        {
            return Err(Error::new(
                E_FAIL,
                "Could not prepare hotkey recording button",
            ));
        }
        self.controls.hint = self.control(w!("STATIC"), "", 12, label)?;
        self.controls.copy_heading = self.control(w!("STATIC"), "复制行为", COPY_HEADING, label)?;
        self.controls.format_label = self.control(w!("STATIC"), "默认格式", 13, label)?;
        self.controls.format = self.control(w!("COMBOBOX"), "", FORMAT, combo)?;
        self.controls.auto_copy = self.control(w!("BUTTON"), "取色后自动复制", AUTO_COPY, check)?;
        self.controls.appearance_heading =
            self.control(w!("STATIC"), "取色外观", APPEARANCE_HEADING, label)?;
        self.controls.border_label = self.control(w!("STATIC"), "边框粗细", BORDER_LABEL, label)?;
        self.controls.border_width =
            self.control(TRACKBAR_CLASSW, "边框粗细", BORDER_WIDTH, slider)?;
        self.controls.border_value = self.control(w!("STATIC"), "", BORDER_VALUE, label)?;
        self.controls.transparency_label =
            self.control(w!("STATIC"), "背景透明度", TRANSPARENCY_LABEL, label)?;
        self.controls.background_transparency = self.control(
            TRACKBAR_CLASSW,
            "背景透明度",
            BACKGROUND_TRANSPARENCY,
            slider,
        )?;
        self.controls.transparency_value =
            self.control(w!("STATIC"), "", TRANSPARENCY_VALUE, label)?;
        self.controls.usage_heading =
            self.control(w!("STATIC"), "操作提示", USAGE_HEADING, label)?;
        self.controls.usage_hint = self.control(
            w!("STATIC"),
            "左键确认 · 右键 / Esc 取消 · 窗外左键取消\r\n上滚轮冻结并放大 · 下滚轮缩小",
            USAGE_HINT,
            label,
        )?;
        self.controls.status = self.control(
            w!("EDIT"),
            "",
            STATUS,
            WS_TABSTOP | WINDOW_STYLE((ES_READONLY | ES_MULTILINE | ES_AUTOVSCROLL) as u32),
        )?;
        self.controls.close = self.control(w!("BUTTON"), "关闭", CLOSE, button)?;
        self.controls.apply = self.control(w!("BUTTON"), "应用", APPLY, button)?;
        for format in ColorFormat::ALL {
            append_choice(self.controls.format, format.label())?;
        }
        let format_index = ColorFormat::ALL
            .iter()
            .position(|format| *format == config.default_format)
            .ok_or_else(|| Error::new(E_FAIL, "Configured color format is not supported"))?;
        self.update_capture_text()?;
        set_choice(self.controls.format, format_index)?;
        set_checked(self.controls.ctrl, config.hotkey.ctrl);
        set_checked(self.controls.alt, config.hotkey.alt);
        set_checked(self.controls.shift, config.hotkey.shift);
        set_checked(self.controls.auto_copy, config.auto_copy_on_pick);
        set_slider(
            self.controls.border_width,
            MAX_BORDER_WIDTH_DIP,
            config.appearance.border_width_dip,
            1,
        );
        set_slider(
            self.controls.background_transparency,
            MAX_BACKGROUND_TRANSPARENCY_PERCENT,
            config.appearance.background_transparency_percent,
            5,
        );
        self.update_appearance_text()?;
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
        let format_index = selected_choice(self.controls.format)?;
        let key = key_name(self.callback.selected_key.get())
            .ok_or_else(|| Error::new(E_FAIL, "请选择有效的主键"))?;
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
            appearance: AppearanceConfig {
                border_width_dip: slider_value(self.controls.border_width)?,
                background_transparency_percent: slider_value(
                    self.controls.background_transparency,
                )?,
            },
        })
    }

    fn update_appearance_text(&self) -> Result<()> {
        for (hwnd, text) in [
            (
                self.controls.border_value,
                format!("{} DIP", slider_value(self.controls.border_width)?),
            ),
            (
                self.controls.transparency_value,
                format!("{}%", slider_value(self.controls.background_transparency)?),
            ),
        ] {
            unsafe { SetWindowTextW(hwnd, PCWSTR(wide(&text).as_ptr()))? };
        }
        Ok(())
    }

    fn update_capture_text(&self) -> Result<()> {
        let button = if self.callback.recording.get() {
            "按下主键…".to_owned()
        } else {
            format!(
                "{} · 点击修改",
                key_name(self.callback.selected_key.get())
                    .ok_or_else(|| Error::new(E_FAIL, "请选择有效的主键"))?
            )
        };
        let hint = match self.callback.capture_hint.get() {
            CaptureHint::Ready => "点击按钮后按下新主键\r\n至少选择 Ctrl 或 Alt",
            CaptureHint::Listening => "字母、数字或 F1–F11\r\nEsc 取消 · Tab 离开",
            CaptureHint::Invalid => "仅支持字母、数字和 F1–F11\r\n请重试 · Esc 取消",
            CaptureHint::Accepted => "主键已更新，应用后生效\r\n点击按钮可再次修改",
            CaptureHint::Cancelled => "已保留原主键\r\n点击按钮后重新录制",
        };
        unsafe {
            SetWindowTextW(self.controls.key, PCWSTR(wide(&button).as_ptr()))?;
            SetWindowTextW(self.controls.hint, PCWSTR(wide(hint).as_ptr()))?;
        }
        Ok(())
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
                (self.controls.appearance_heading, heading.0),
                (self.controls.usage_heading, small.0),
                (self.controls.subtitle, small.0),
                (self.controls.hint, small.0),
                (self.controls.usage_hint, small.0),
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
        place(self.controls.hotkey_label, 40, 100, 396, 24)?;
        place(self.controls.ctrl, 40, 128, 88, 26)?;
        place(self.controls.alt, 140, 128, 88, 26)?;
        place(self.controls.shift, 240, 128, 100, 26)?;
        place(self.controls.key_label, 40, 171, 48, 24)?;
        place(self.controls.key, 88, 164, 132, 36)?;
        place(self.controls.hint, 228, 164, 212, 40)?;
        place(self.controls.copy_heading, 40, 244, 396, 24)?;
        place(self.controls.format_label, 40, 279, 108, 24)?;
        place(self.controls.format, 156, 272, 180, 160)?;
        place(self.controls.auto_copy, 40, 304, 396, 26)?;
        place(self.controls.appearance_heading, 40, 362, 396, 24)?;
        place(self.controls.border_label, 40, 397, 112, 24)?;
        place(self.controls.border_width, 156, 390, 220, 30)?;
        place(self.controls.border_value, 388, 397, 56, 24)?;
        place(self.controls.transparency_label, 40, 433, 112, 24)?;
        place(self.controls.background_transparency, 156, 426, 220, 30)?;
        place(self.controls.transparency_value, 388, 433, 56, 24)?;
        place(self.controls.usage_heading, 24, 478, 64, 20)?;
        place(self.controls.usage_hint, 96, 478, 360, 40)?;
        place(self.controls.status, 24, 532, 228, 40)?;
        place(self.controls.close, 264, 532, 88, 36)?;
        place(self.controls.apply, 364, 532, 92, 36)?;
        unsafe {
            SendMessageW(
                self.controls.format,
                CB_SETITEMHEIGHT,
                Some(WPARAM(usize::MAX)),
                Some(LPARAM(dip(24, dpi) as isize)),
            );
            SendMessageW(
                self.controls.format,
                CB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(dip(24, dpi) as isize)),
            );
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
        for (id, hwnd) in [
            (APPLY, self.controls.apply),
            (CLOSE, self.controls.close),
            (KEY, self.controls.key),
        ] {
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

fn key_name(key: u32) -> Option<String> {
    match key {
        0x30..=0x39 | 0x41..=0x5a => char::from_u32(key).map(|key| key.to_string()),
        0x70..=0x7a => Some(format!("F{}", key - 0x70 + 1)),
        _ => None,
    }
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

fn set_slider(hwnd: HWND, maximum: u8, value: u8, page_size: u8) {
    unsafe {
        SendMessageW(hwnd, TBM_SETRANGEMIN, Some(WPARAM(0)), Some(LPARAM(0)));
        SendMessageW(
            hwnd,
            TBM_SETRANGEMAX,
            Some(WPARAM(0)),
            Some(LPARAM(isize::from(maximum))),
        );
        SendMessageW(
            hwnd,
            TBM_SETPAGESIZE,
            None,
            Some(LPARAM(isize::from(page_size))),
        );
        SendMessageW(
            hwnd,
            TBM_SETPOS,
            Some(WPARAM(1)),
            Some(LPARAM(isize::from(value))),
        );
    }
}

fn slider_value(hwnd: HWND) -> Result<u8> {
    let value = unsafe { SendMessageW(hwnd, TBM_GETPOS, None, None) }.0;
    u8::try_from(value).map_err(|_| Error::new(E_FAIL, "请选择有效的取色外观值"))
}

unsafe extern "system" fn key_button_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        key_button_dispatch(hwnd, message, wparam, lparam, subclass, reference)
    }))
    .unwrap_or_else(|_| std::process::abort())
}

unsafe fn key_button_dispatch(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    // The owner destroys all child windows before dropping this stable Box.
    let state = unsafe { &*(reference as *const CallbackState) };
    let key = wparam.0 as u32;
    match message {
        WM_NCDESTROY => {
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(key_button_proc), subclass) };
        }
        WM_GETDLGCODE => {
            let native = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
            // IsDialogMessage must dispatch Enter/Esc to the recording button,
            // while Tab keeps its normal navigation and cancels on focus loss.
            if (state.recording.get() && key != u32::from(VK_TAB.0)) || state.suppresses(key) {
                return LRESULT(native.0 | DLGC_WANTALLKEYS as isize);
            }
            return native;
        }
        WM_KILLFOCUS => {
            state.cancel_capture();
        }
        WM_KEYDOWN | WM_SYSKEYDOWN if state.suppresses(key) => {
            return LRESULT(0);
        }
        WM_KEYDOWN | WM_SYSKEYDOWN if state.recording.get() => {
            if key == u32::from(VK_TAB.0) {
                state.cancel_capture();
            } else {
                // Track every consumed key until its release. A held invalid
                // Enter must stay suppressed even if a later key is accepted.
                state.suppress(key, true);
                if key == u32::from(VK_ESCAPE.0) {
                    state.cancel_capture();
                } else if key_name(key).is_some() {
                    state.selected_key.set(key);
                    state.recording.set(false);
                    state.capture_hint.set(CaptureHint::Accepted);
                    state.queue(|pending| pending.capture = true);
                } else if ![VK_CONTROL, VK_MENU, VK_SHIFT]
                    .iter()
                    .any(|modifier| key == u32::from(modifier.0))
                {
                    state.capture_hint.set(CaptureHint::Invalid);
                    state.queue(|pending| pending.capture = true);
                }
                return LRESULT(0);
            }
        }
        WM_KEYUP | WM_SYSKEYUP if state.recording.get() || state.suppresses(key) => {
            state.suppress(key, false);
            return LRESULT(0);
        }
        // TranslateMessage may already have queued the character when its key
        // was accepted. The button has no text-input or mnemonic behavior.
        WM_CHAR | WM_SYSCHAR => return LRESULT(0),
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
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
                12 => (
                    true,
                    if matches!(state.capture_hint.get(), CaptureHint::Invalid) {
                        Tone::Error
                    } else {
                        Tone::Muted
                    },
                ),
                USAGE_HEADING | USAGE_HINT => (false, Tone::Muted),
                BORDER_VALUE | TRANSPARENCY_VALUE => (true, Tone::Accent),
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
                KEY => state.begin_capture(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            let id = unsafe { GetDlgCtrlID(HWND(lparam.0 as *mut _)) } as usize;
            if matches!(id, BORDER_WIDTH | BACKGROUND_TRANSPARENCY) {
                state.queue(|pending| pending.appearance = true);
            }
            LRESULT(0)
        }
        WM_ACTIVATE if wparam.0 as u32 & 0xffff == WA_INACTIVE => {
            state.cancel_capture();
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
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
