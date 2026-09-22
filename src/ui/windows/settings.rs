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
            COLORREF, E_FAIL, ERROR_CLASS_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HWND,
            LPARAM, LRESULT, POINT, RECT, SetLastError, WPARAM,
        },
        Graphics::{
            Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute},
            Gdi::*,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            SystemServices::{SS_NOPREFIX, SS_OWNERDRAW},
        },
        UI::{
            Controls::{
                DRAWITEMSTRUCT, EM_GETLINECOUNT, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX,
                InitCommonControlsEx, SetScrollInfo, ShowScrollBar, TBM_SETPAGESIZE, TBM_SETPOS,
                TBM_SETRANGEMAX, TBM_SETRANGEMIN, TBS_NOTICKS, TRACKBAR_CLASSW,
            },
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
            Input::KeyboardAndMouse::{
                EnableWindow, GetFocus, SetFocus, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SHIFT, VK_TAB,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, Error, PCWSTR, Result, w},
};

use super::{
    drawing::dip,
    layout::{self, SheetLayout, fit_to_work_area, reveal_offset},
    theme::{self, Font, Theme, Tone},
};
use crate::{
    app::{
        config::{
            AppearanceConfig, Config, HotkeyConfig, MAX_BACKGROUND_TRANSPARENCY_PERCENT,
            MAX_BORDER_WIDTH_DIP,
        },
        i18n::{Language, tr},
    },
    core::format::ColorFormat,
};

#[path = "settings_preview.rs"]
mod settings_preview;
use settings_preview::AppearancePreview;

pub const WM_SETTINGS_WAKE: u32 = WM_APP + 12;
const CLASS: PCWSTR = w!("ColorPicker.Settings.v1");
const CONTENT_CLASS: PCWSTR = w!("ColorPicker.SettingsContent.v1");
const CONTENT: usize = 200;
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
const QUICK_PICK: usize = 109;
const LANGUAGE: usize = 110;
const LANGUAGE_LABEL: usize = 28;
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
const PREVIEW_HEADING: usize = 25;
const APPEARANCE_PREVIEW: usize = 26;
const PREVIEW_HINT: usize = 27;
const STATUS: usize = 14;
const CLIENT_WIDTH: i32 = 500;
const CLIENT_HEIGHT: i32 = 750;
const KEY_SUBCLASS: usize = 1;
const SCROLL_SUBCLASS: usize = 2;
// CommCtrl.h aliases TBM_GETPOS to WM_USER; windows-rs omits this alias.
const TBM_GETPOS: u32 = WM_USER;
const STYLE: WINDOW_STYLE =
    WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);
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
    capture: bool,
    focus_key: bool,
    appearance: bool,
    scroll: Option<i32>,
    reveal: Option<HWND>,
    fit_work_area: bool,
    copy_behavior: bool,
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
    viewport: Cell<HWND>,
    scroll_offset: Cell<i32>,
    viewport_height: Cell<i32>,
    content_height: Cell<i32>,
    content_width_dip: Cell<i32>,
    wheel_remainder: Cell<i32>,
    preview: RefCell<Option<AppearancePreview>>,
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
    language_label: HWND,
    language: HWND,
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
    quick_pick: HWND,
    appearance_heading: HWND,
    border_label: HWND,
    border_width: HWND,
    border_value: HWND,
    transparency_label: HWND,
    background_transparency: HWND,
    transparency_value: HWND,
    preview_heading: HWND,
    preview: HWND,
    preview_hint: HWND,
    usage_heading: HWND,
    usage_hint: HWND,
    apply: HWND,
    close: HWND,
    status: HWND,
}

impl Controls {
    fn handles(&self) -> [HWND; 31] {
        [
            self.title,
            self.subtitle,
            self.language_label,
            self.language,
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
            self.quick_pick,
            self.appearance_heading,
            self.border_label,
            self.border_width,
            self.border_value,
            self.transparency_label,
            self.background_transparency,
            self.transparency_value,
            self.preview_heading,
            self.preview,
            self.preview_hint,
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
    // Keep the bilingual selector's CJK font when localized body fonts change.
    // Resetting its font can leave the native popup scrolled past the first
    // choice on the next opening. Rebuild this font only for DPI changes.
    language_font: RefCell<Option<(u32, Font)>>,
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
                    tr(
                        "配置的快捷键不在支持的按键列表中",
                        "Configured hotkey is outside the supported key list",
                    ),
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
        for name in [CLASS, CONTENT_CLASS] {
            let class = WNDCLASSW {
                lpszClassName: name,
                ..class
            };
            if unsafe { RegisterClassW(&class) } == 0
                && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
            {
                return Err(Error::from_thread());
            }
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
            viewport: Cell::new(HWND::default()),
            scroll_offset: Cell::new(0),
            viewport_height: Cell::new(0),
            content_height: Cell::new(0),
            content_width_dip: Cell::new(480),
            wheel_remainder: Cell::new(0),
            preview: RefCell::new(None),
        });
        let pointer = callback.as_ref() as *const CallbackState;
        let mut cursor = POINT::default();
        unsafe { GetCursorPos(&mut cursor)? };
        let hwnd = unsafe {
            CreateWindowExW(
                EX_STYLE,
                CLASS,
                PCWSTR(wide(tr("设置 — Color Picker", "Settings — Color Picker")).as_ptr()),
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
            language_font: RefCell::new(None),
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
        theme::configure_window(hwnd, &window.callback.theme);
        let viewport = unsafe {
            CreateWindowExW(
                WS_EX_CONTROLPARENT,
                CONTENT_CLASS,
                PCWSTR(wide(tr("设置内容", "Settings content")).as_ptr()),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_VSCROLL,
                0,
                0,
                1,
                1,
                Some(hwnd),
                Some(HMENU(CONTENT as *mut _)),
                Some(instance),
                Some(pointer.cast()),
            )?
        };
        window.callback.viewport.set(viewport);
        window.create_controls(config)?;
        window.place_initially(cursor)?;
        window.refresh_language()?;
        let default_notice = if save_allowed {
            tr("点击“应用”保存更改。", "Click Apply to save changes.")
        } else {
            tr(
                "当前配置只读，无法应用更改。",
                "Settings are read-only; changes cannot be applied.",
            )
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

    /// Relabel the existing draft only after the host successfully applies it.
    /// Controls, selections and scroll position stay intact during the switch.
    pub fn refresh_language(&self) -> Result<()> {
        for (hwnd, text) in [
            (
                self.hwnd,
                tr("设置 — Color Picker", "Settings — Color Picker"),
            ),
            (
                self.callback.viewport.get(),
                tr("设置内容", "Settings content"),
            ),
            (self.controls.title, tr("偏好设置", "Preferences")),
            (
                self.controls.subtitle,
                tr(
                    "自定义快捷键、复制方式和取色外观",
                    "Customize the shortcut, copying and appearance",
                ),
            ),
            (self.controls.language_label, tr("语言", "Language")),
            (
                self.controls.hotkey_label,
                tr("取色快捷键", "Picking shortcut"),
            ),
            (self.controls.key_label, tr("主键", "Key")),
            (self.controls.copy_heading, tr("复制行为", "Copying")),
            (self.controls.format_label, tr("默认格式", "Default format")),
            (
                self.controls.quick_pick,
                tr(
                    "快速取色（复制后不打开结果窗口）",
                    "Quick pick (copy without opening the result)",
                ),
            ),
            (
                self.controls.auto_copy,
                tr(
                    "普通取色后自动复制",
                    "Automatically copy after a normal pick",
                ),
            ),
            (
                self.controls.appearance_heading,
                tr("取色外观", "Picker appearance"),
            ),
            (self.controls.border_label, tr("边框粗细", "Border width")),
            (self.controls.border_width, tr("边框粗细", "Border width")),
            (
                self.controls.transparency_label,
                tr("背景透明度", "Transparency"),
            ),
            (
                self.controls.background_transparency,
                tr("背景透明度", "Background transparency"),
            ),
            (
                self.controls.preview_heading,
                tr("实时预览 · 示例颜色", "Live preview · Sample colors"),
            ),
            (
                self.controls.preview_hint,
                tr(
                    "拖动滑块预览，点击“应用”后生效。",
                    "Drag to preview. Click Apply to save.",
                ),
            ),
            (self.controls.usage_heading, tr("操作提示", "Tips")),
            (
                self.controls.usage_hint,
                tr(
                    "左键确认 · 右键 / Esc 取消 · 窗外左键取消\r\n上滚轮冻结并放大 · 下滚轮缩小",
                    "Click to pick · Right-click / Esc / outside to cancel\r\nScroll up to freeze / zoom in · Down to zoom out",
                ),
            ),
            (self.controls.close, tr("关闭", "Close")),
            (self.controls.apply, tr("应用", "Apply")),
        ] {
            unsafe { SetWindowTextW(hwnd, PCWSTR(wide(text).as_ptr()))? };
        }
        self.update_capture_text()?;
        self.font.borrow_mut().dpi = 0;
        self.callback.preview.replace(None);
        self.layout()?;
        self.update_default_style();
        Ok(())
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
            return Err(Error::new(
                E_FAIL,
                tr(
                    "无法处理设置窗口操作",
                    "Could not queue settings-window work",
                ),
            ));
        }
        let pending = self.callback.pending.take();
        if pending.close {
            self.callback.recording.set(false);
            self.callback.closing.set(true);
            return Ok(Some(SettingsAction::Close));
        }
        if let Some(rect) = pending.dpi_rect {
            let rect = fit_to_work_area(rect, self.minimum_size()?)?;
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
        if pending.fit_work_area {
            let mut rect = RECT::default();
            unsafe { GetWindowRect(self.hwnd, &mut rect)? };
            let rect = fit_to_work_area(rect, self.minimum_size()?)?;
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )?;
            }
        }
        if let Some(offset) = pending.scroll {
            self.callback.scroll_offset.set(offset);
        }
        if pending.layout || pending.scroll.is_some() || pending.fit_work_area {
            self.layout()?;
            if pending.scroll.is_none() {
                self.reveal_control(unsafe { GetFocus() })?;
            }
        }
        if let Some(hwnd) = pending.reveal {
            self.reveal_control(hwnd)?;
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
        if pending.copy_behavior {
            self.update_copy_behavior();
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
            return Err(Error::new(
                E_FAIL,
                tr(
                    "无法初始化设置滑块",
                    "Could not initialize settings sliders",
                ),
            ));
        }
        let label = WINDOW_STYLE(SS_NOPREFIX.0);
        let check = WS_TABSTOP | WINDOW_STYLE((BS_AUTOCHECKBOX | BS_MULTILINE) as u32);
        let combo = WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32);
        let button = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32);
        let slider = WS_TABSTOP | WINDOW_STYLE(TBS_NOTICKS);
        self.controls.title = self.control(w!("STATIC"), "", TITLE, label)?;
        self.controls.subtitle = self.control(w!("STATIC"), "", SUBTITLE, label)?;
        self.controls.language_label = self.control(w!("STATIC"), "", LANGUAGE_LABEL, label)?;
        self.controls.language = self.control(w!("COMBOBOX"), "", LANGUAGE, combo)?;
        for language in ["简体中文", "English"] {
            append_choice(self.controls.language, language)?;
        }
        set_choice(
            self.controls.language,
            match config.language {
                Language::SimplifiedChinese => 0,
                Language::English => 1,
            },
        )?;
        self.controls.hotkey_label = self.control(w!("STATIC"), "", 10, label)?;
        self.controls.ctrl = self.control(w!("BUTTON"), "Ctrl", CTRL, check)?;
        self.controls.alt = self.control(w!("BUTTON"), "Alt", ALT, check)?;
        self.controls.shift = self.control(w!("BUTTON"), "Shift", SHIFT, check)?;
        self.controls.key_label = self.control(w!("STATIC"), "", 11, label)?;
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
                tr(
                    "无法初始化快捷键录制按钮",
                    "Could not prepare hotkey recording button",
                ),
            ));
        }
        self.controls.hint = self.control(w!("STATIC"), "", 12, label)?;
        self.controls.copy_heading = self.control(w!("STATIC"), "", COPY_HEADING, label)?;
        self.controls.format_label = self.control(w!("STATIC"), "", 13, label)?;
        self.controls.format = self.control(w!("COMBOBOX"), "", FORMAT, combo)?;
        self.controls.quick_pick = self.control(w!("BUTTON"), "", QUICK_PICK, check)?;
        self.controls.auto_copy = self.control(w!("BUTTON"), "", AUTO_COPY, check)?;
        self.controls.appearance_heading =
            self.control(w!("STATIC"), "", APPEARANCE_HEADING, label)?;
        self.controls.border_label = self.control(w!("STATIC"), "", BORDER_LABEL, label)?;
        self.controls.border_width = self.control(TRACKBAR_CLASSW, "", BORDER_WIDTH, slider)?;
        self.controls.border_value = self.control(w!("STATIC"), "", BORDER_VALUE, label)?;
        self.controls.transparency_label =
            self.control(w!("STATIC"), "", TRANSPARENCY_LABEL, label)?;
        self.controls.background_transparency =
            self.control(TRACKBAR_CLASSW, "", BACKGROUND_TRANSPARENCY, slider)?;
        self.controls.transparency_value =
            self.control(w!("STATIC"), "", TRANSPARENCY_VALUE, label)?;
        self.controls.preview_heading = self.control(w!("STATIC"), "", PREVIEW_HEADING, label)?;
        self.controls.preview = self.control(
            w!("STATIC"),
            "",
            APPEARANCE_PREVIEW,
            WINDOW_STYLE(SS_OWNERDRAW.0),
        )?;
        self.controls.preview_hint = self.control(w!("STATIC"), "", PREVIEW_HINT, label)?;
        self.controls.usage_heading = self.control(w!("STATIC"), "", USAGE_HEADING, label)?;
        self.controls.usage_hint = self.control(w!("STATIC"), "", USAGE_HINT, label)?;
        self.controls.status = self.control(
            w!("EDIT"),
            "",
            STATUS,
            WS_TABSTOP | WINDOW_STYLE((ES_READONLY | ES_MULTILINE | ES_AUTOVSCROLL) as u32),
        )?;
        self.controls.close = self.control(w!("BUTTON"), "", CLOSE, button)?;
        self.controls.apply = self.control(w!("BUTTON"), "", APPLY, button)?;
        for format in ColorFormat::ALL {
            append_choice(self.controls.format, format.label())?;
        }
        let format_index = ColorFormat::ALL
            .iter()
            .position(|format| *format == config.default_format)
            .ok_or_else(|| {
                Error::new(
                    E_FAIL,
                    tr(
                        "不支持配置的颜色格式",
                        "Configured color format is not supported",
                    ),
                )
            })?;
        self.update_capture_text()?;
        set_choice(self.controls.format, format_index)?;
        set_checked(self.controls.ctrl, config.hotkey.ctrl);
        set_checked(self.controls.alt, config.hotkey.alt);
        set_checked(self.controls.shift, config.hotkey.shift);
        set_checked(self.controls.auto_copy, config.auto_copy_on_pick);
        set_checked(self.controls.quick_pick, config.quick_pick);
        self.update_copy_behavior();
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
        let footer = matches!(id, APPLY | CLOSE | STATUS);
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                Some(if footer {
                    self.hwnd
                } else {
                    self.callback.viewport.get()
                }),
                Some(HMENU(id as *mut _)),
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?
        };
        if !footer
            && !unsafe {
                SetWindowSubclass(
                    hwnd,
                    Some(scroll_control_proc),
                    SCROLL_SUBCLASS,
                    self.callback.as_ref() as *const CallbackState as usize,
                )
            }
            .as_bool()
        {
            return Err(Error::new(
                E_FAIL,
                tr(
                    "无法初始化设置滚动区域",
                    "Could not prepare settings focus scrolling",
                ),
            ));
        }
        Ok(hwnd)
    }

    fn read_config(&self) -> Result<Config> {
        let format_index = selected_choice(self.controls.format)?;
        let key = key_name(self.callback.selected_key.get())
            .ok_or_else(|| Error::new(E_FAIL, tr("请选择有效的主键", "Choose a valid key")))?;
        let default_format = *ColorFormat::ALL.get(format_index).ok_or_else(|| {
            Error::new(
                E_FAIL,
                tr("请选择有效的颜色格式", "Choose a valid color format"),
            )
        })?;
        let language = match selected_choice(self.controls.language)? {
            0 => Language::SimplifiedChinese,
            1 => Language::English,
            _ => {
                return Err(Error::new(
                    E_FAIL,
                    tr("请选择有效的语言", "Choose a valid language"),
                ));
            }
        };
        Ok(Config {
            schema_version: self.schema_version,
            language,
            hotkey: HotkeyConfig {
                ctrl: checked(self.controls.ctrl),
                alt: checked(self.controls.alt),
                shift: checked(self.controls.shift),
                key,
            },
            default_format,
            auto_copy_on_pick: checked(self.controls.auto_copy),
            quick_pick: checked(self.controls.quick_pick),
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
        self.update_appearance_preview()
    }

    fn update_copy_behavior(&self) {
        // Keep the ordinary-mode preference intact, even when quick picking
        // makes copying mandatory. Unchecking quick mode restores this choice.
        let _ =
            unsafe { EnableWindow(self.controls.auto_copy, !checked(self.controls.quick_pick)) };
    }

    fn update_appearance_preview(&self) -> Result<()> {
        let dpi = self.dpi()?;
        let appearance = AppearanceConfig {
            border_width_dip: slider_value(self.controls.border_width)?,
            background_transparency_percent: slider_value(self.controls.background_transparency)?,
        };
        let rebuild = self
            .callback
            .preview
            .borrow()
            .as_ref()
            .is_none_or(|preview| !preview.matches(dpi, appearance));
        if rebuild {
            self.callback
                .preview
                .replace(Some(AppearancePreview::new(dpi, appearance)?));
            let description = crate::tr_format!(
                "取色外观示例：#49A7C6，X 1280 Y 720，边框 {} DIP，背景透明度 {}%",
                "Appearance sample: #49A7C6, X 1280 Y 720, border {} DIP, transparency {}%",
                appearance.border_width_dip,
                appearance.background_transparency_percent
            );
            unsafe {
                SetWindowTextW(self.controls.preview, PCWSTR(wide(&description).as_ptr()))?;
                let _ = InvalidateRect(Some(self.controls.preview), None, false);
            }
        }
        Ok(())
    }

    fn update_capture_text(&self) -> Result<()> {
        let button = if self.callback.recording.get() {
            tr("按下主键…", "Press a key…").to_owned()
        } else {
            crate::tr_format!(
                "{} · 点击修改",
                "{} · Change",
                key_name(self.callback.selected_key.get()).ok_or_else(|| Error::new(
                    E_FAIL,
                    tr("请选择有效的主键", "Choose a valid key")
                ))?
            )
        };
        let hint = match self.callback.capture_hint.get() {
            CaptureHint::Ready => tr(
                "点击按钮后按下新主键\r\n至少选择 Ctrl 或 Alt",
                "Click, then press a new key\r\nSelect Ctrl or Alt (or both)",
            ),
            CaptureHint::Listening => tr(
                "字母、数字或 F1–F11\r\nEsc 取消 · Tab 离开",
                "Letters, digits or F1–F11\r\nEsc to cancel · Tab to leave",
            ),
            CaptureHint::Invalid => tr(
                "仅支持字母、数字和 F1–F11\r\n请重试 · Esc 取消",
                "Use letters, digits or F1–F11\r\nTry again · Esc to cancel",
            ),
            CaptureHint::Accepted => tr(
                "主键已更新，应用后生效\r\n点击按钮可再次修改",
                "Key updated; Apply to save\r\nClick to change it again",
            ),
            CaptureHint::Cancelled => tr(
                "已保留原主键\r\n点击按钮后重新录制",
                "Original key kept\r\nClick to record again",
            ),
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
                tr(
                    "无法获取设置窗口的显示缩放比例",
                    "Could not determine settings-window DPI",
                ),
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
        let x = (i64::from(center_x) - i64::from(width) / 2) as i32;
        let y = (i64::from(center_y) - i64::from(height) / 2) as i32;
        // Keep the monitor used to choose DPI even when the preferred rectangle
        // extends farther into a neighbouring display than this work area.
        let rect = layout::fit_rect(
            RECT {
                left: x,
                top: y,
                right: x + width,
                bottom: y + height,
            },
            work,
            self.minimum_size()?,
        )
        .ok_or_else(layout::unavailable)?;
        let (x, y, width, height) = (
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        );
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

    fn minimum_size(&self) -> Result<(i32, i32)> {
        let dpi = self.dpi()?;
        let mut outer = RECT {
            right: dip(280, dpi),
            bottom: dip(180, dpi),
            ..Default::default()
        };
        unsafe { AdjustWindowRectExForDpi(&mut outer, STYLE, false, EX_STYLE, dpi)? };
        Ok((outer.right - outer.left, outer.bottom - outer.top))
    }

    fn layout(&self) -> Result<()> {
        self.callback.theme.update_window_icons(self.hwnd);
        let dpi = self.dpi()?;
        if self
            .language_font
            .borrow()
            .as_ref()
            .is_none_or(|(font_dpi, _)| *font_dpi != dpi)
        {
            let font = Font::for_language(14, dpi, 400, false, Language::SimplifiedChinese)?;
            unsafe {
                SendMessageW(
                    self.controls.language,
                    WM_SETFONT,
                    Some(WPARAM(font.0.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            self.language_font.replace(Some((dpi, font)));
        }
        if self.font.borrow().dpi != dpi {
            let body = Font::new(14, dpi, 400, false)?;
            let title = Font::new(20, dpi, 600, false)?;
            let heading = Font::new(14, dpi, 600, false)?;
            let small = Font::new(12, dpi, 400, false)?;
            let small_line_height = font_line_height(self.controls.status, small.0, dpi);
            for hwnd in self.controls.handles() {
                if hwnd == self.controls.language {
                    continue;
                }
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
                (self.controls.preview_heading, small.0),
                (self.controls.preview_hint, small.0),
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
        let mut client = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut client)? };
        let narrow = client.right < dip(480, dpi);
        let panes = SheetLayout::new(
            client.right,
            client.bottom,
            0,
            dip(if narrow { 100 } else { 64 }, dpi),
            dip(80, dpi),
        )?;
        let viewport_height = panes.viewport.bottom;
        let viewport = self.callback.viewport.get();
        unsafe { MoveWindow(viewport, 0, 0, client.right, viewport_height, true)? };
        self.callback.viewport_height.set(viewport_height);
        // Keep a stable scrollbar gutter while measuring responsive content.
        let content_width = ((i64::from(client.right) * 96) / i64::from(dpi)) as i32 - 20;
        self.callback.content_width_dip.set(content_width);
        let geometry = settings_content_layout(content_width);
        let content_height = dip(geometry.height, dpi);
        self.callback.content_height.set(content_height);
        let offset = self
            .callback
            .scroll_offset
            .get()
            .clamp(0, (content_height - viewport_height).max(0));
        self.callback.scroll_offset.set(offset);
        let scroll = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: content_height - 1,
            nPage: viewport_height as u32,
            nPos: offset,
            ..Default::default()
        };
        unsafe {
            SetScrollInfo(viewport, SB_VERT, &scroll, true);
        }
        for (id, rect) in geometry.controls {
            let control = unsafe { GetDlgItem(Some(viewport), id as i32)? };
            // COMBOBOX uses the requested height for its opened dropdown.
            let height = if matches!(id, LANGUAGE | FORMAT) {
                160
            } else {
                rect.bottom - rect.top
            };
            unsafe {
                MoveWindow(
                    control,
                    dip(rect.left, dpi),
                    dip(rect.top, dpi) - offset,
                    dip(rect.right - rect.left, dpi),
                    dip(height, dpi),
                    true,
                )?;
            }
        }
        let footer_top = panes.actions.top + dip(8, dpi);
        let button_width = dip(92, dpi);
        let gap = dip(12, dpi);
        let margin = dip(16, dpi);
        let apply_x = client.right - margin - button_width;
        let close_x = apply_x - gap - button_width;
        let button_y = footer_top + if narrow { dip(44, dpi) } else { 0 };
        unsafe {
            MoveWindow(
                self.controls.status,
                margin,
                footer_top,
                if narrow {
                    client.right - 2 * margin
                } else {
                    close_x - gap - margin
                },
                dip(40, dpi),
                true,
            )?;
            MoveWindow(
                self.controls.close,
                close_x,
                button_y,
                button_width,
                dip(36, dpi),
                true,
            )?;
            MoveWindow(
                self.controls.apply,
                apply_x,
                button_y,
                button_width,
                dip(36, dpi),
                true,
            )?;
        }
        for combo in [self.controls.language, self.controls.format] {
            unsafe {
                SendMessageW(
                    combo,
                    CB_SETITEMHEIGHT,
                    Some(WPARAM(usize::MAX)),
                    Some(LPARAM(dip(24, dpi) as isize)),
                );
                SendMessageW(
                    combo,
                    CB_SETITEMHEIGHT,
                    Some(WPARAM(0)),
                    Some(LPARAM(dip(24, dpi) as isize)),
                );
            }
        }
        self.update_status_scrollbar()?;
        self.update_appearance_preview()?;
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        let _ = unsafe { InvalidateRect(Some(viewport), None, false) };
        Ok(())
    }

    fn reveal_control(&self, hwnd: HWND) -> Result<()> {
        let viewport = self.callback.viewport.get();
        if !unsafe { IsChild(viewport, hwnd) }.as_bool() {
            return Ok(());
        }
        let mut rect = RECT::default();
        let mut origin = POINT::default();
        unsafe {
            GetWindowRect(hwnd, &mut rect)?;
            if !ClientToScreen(viewport, &mut origin).as_bool() {
                return Err(Error::from_thread());
            }
        }
        let offset = reveal_offset(
            self.callback.scroll_offset.get(),
            rect.top - origin.y,
            rect.bottom - origin.y,
            self.callback.viewport_height.get(),
            self.callback.content_height.get(),
            dip(8, self.dpi()?),
        );
        if self.callback.scroll_offset.replace(offset) != offset {
            self.layout()?;
        }
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

struct SettingsContentLayout {
    controls: Vec<(usize, RECT)>,
    panels: [RECT; 3],
    height: i32,
}

fn settings_content_layout(width: i32) -> SettingsContentLayout {
    let rect = |x, y, w, h| RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    if width >= 460 {
        let rows = [
            (TITLE, 24, 22, 220, 30),
            (LANGUAGE_LABEL, 252, 29, 72, 24),
            (LANGUAGE, 326, 22, 130, 28),
            (SUBTITLE, 24, 58, 432, 20),
            (10, 40, 100, 396, 24),
            (CTRL, 40, 128, 88, 26),
            (ALT, 140, 128, 88, 26),
            (SHIFT, 240, 128, 100, 26),
            (11, 40, 171, 48, 24),
            (KEY, 88, 164, 132, 36),
            (12, 228, 164, 212, 40),
            (COPY_HEADING, 40, 244, 396, 24),
            (13, 40, 279, 108, 24),
            (FORMAT, 156, 272, 180, 28),
            (QUICK_PICK, 40, 304, 396, 26),
            (AUTO_COPY, 40, 334, 396, 26),
            (APPEARANCE_HEADING, 40, 392, 396, 24),
            (BORDER_LABEL, 40, 427, 112, 24),
            (BORDER_WIDTH, 156, 420, 220, 30),
            (BORDER_VALUE, 388, 427, 56, 24),
            (TRANSPARENCY_LABEL, 40, 463, 112, 24),
            (BACKGROUND_TRANSPARENCY, 156, 456, 220, 30),
            (TRANSPARENCY_VALUE, 388, 463, 56, 24),
            (PREVIEW_HEADING, 40, 494, 396, 20),
            (
                APPEARANCE_PREVIEW,
                40,
                518,
                settings_preview::WIDTH,
                settings_preview::HEIGHT,
            ),
            (PREVIEW_HINT, 40, 594, 396, 20),
            (USAGE_HEADING, 24, 640, 64, 20),
            (USAGE_HINT, 96, 640, 360, 40),
        ];
        SettingsContentLayout {
            controls: rows
                .into_iter()
                .map(|(id, x, y, w, h)| (id, rect(x, y, w, h)))
                .collect(),
            panels: [
                rect(24, 88, 432, 132),
                rect(24, 232, 432, 136),
                rect(24, 380, 432, 248),
            ],
            height: 686,
        }
    } else {
        let inner = width - 56;
        let full = width - 32;
        let rows = [
            (TITLE, 16, 22, full, 30),
            (SUBTITLE, 16, 58, full, 40),
            (LANGUAGE_LABEL, 16, 104, full, 24),
            (LANGUAGE, 16, 132, 180.min(full), 28),
            (10, 28, 188, inner, 24),
            (CTRL, 28, 220, 76, 26),
            (ALT, 112, 220, 72, 26),
            (SHIFT, 28, 252, 100, 26),
            (11, 28, 288, inner, 24),
            (KEY, 28, 316, 132, 36),
            (12, 28, 360, inner, 64),
            (COPY_HEADING, 28, 460, inner, 24),
            (13, 28, 492, inner, 24),
            (FORMAT, 28, 524, 180.min(inner), 28),
            (QUICK_PICK, 28, 560, inner, 52),
            (AUTO_COPY, 28, 620, inner, 52),
            (APPEARANCE_HEADING, 28, 708, inner, 24),
            (BORDER_LABEL, 28, 740, inner - 68, 24),
            (BORDER_VALUE, width - 84, 740, 56, 24),
            (BORDER_WIDTH, 28, 772, inner, 30),
            (TRANSPARENCY_LABEL, 28, 812, inner - 68, 24),
            (TRANSPARENCY_VALUE, width - 84, 812, 56, 24),
            (BACKGROUND_TRANSPARENCY, 28, 844, inner, 30),
            (PREVIEW_HEADING, 28, 884, inner, 40),
            (APPEARANCE_PREVIEW, 28, 930, inner, 72),
            (PREVIEW_HINT, 28, 1010, inner, 40),
            (USAGE_HEADING, 16, 1076, full, 24),
            (USAGE_HINT, 16, 1108, full, 88),
        ];
        SettingsContentLayout {
            controls: rows
                .into_iter()
                .map(|(id, x, y, w, h)| (id, rect(x, y, w, h)))
                .collect(),
            panels: [
                rect(16, 176, full, 260),
                rect(16, 448, full, 236),
                rect(16, 696, full, 366),
            ],
            height: 1212,
        }
    }
}

fn paint_content(hwnd: HWND, offset: i32, width: i32) -> LRESULT {
    let mut paint = PAINTSTRUCT::default();
    let dc = unsafe { BeginPaint(hwnd, &mut paint) };
    if !dc.is_invalid() {
        let saved = unsafe { SaveDC(dc) };
        if saved != 0 {
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            let mut client = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut client);
                SelectObject(dc, GetStockObject(DC_BRUSH));
                SelectObject(dc, GetStockObject(DC_PEN));
                SetDCBrushColor(dc, COLORREF(0xfaf7f5));
                FillRect(dc, &client, HBRUSH(GetStockObject(DC_BRUSH).0));
                SetDCBrushColor(dc, COLORREF(0xffffff));
                SetDCPenColor(dc, COLORREF(0xf0e8e2));
                for panel in settings_content_layout(width).panels {
                    let _ = RoundRect(
                        dc,
                        dip(panel.left, dpi),
                        dip(panel.top, dpi) - offset,
                        dip(panel.right, dpi),
                        dip(panel.bottom, dpi) - offset,
                        dip(16, dpi),
                        dip(16, dpi),
                    );
                }
                let _ = RestoreDC(dc, saved);
            }
        }
    }
    let _ = unsafe { EndPaint(hwnd, &paint) };
    LRESULT(0)
}

unsafe extern "system" fn scroll_control_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        let state = unsafe { &*(reference as *const CallbackState) };
        match message {
            WM_SETFOCUS => state.queue(|pending| pending.reveal = Some(hwnd)),
            WM_NCDESTROY => {
                let _ = unsafe { RemoveWindowSubclass(hwnd, Some(scroll_control_proc), subclass) };
            }
            _ => {}
        }
        // Native sliders and combos retain their wheel/arrow semantics. Other
        // controls let DefWindowProc propagate an unhandled wheel to the pane.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }))
    .unwrap_or_else(|_| std::process::abort())
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
        Err(Error::new(
            E_FAIL,
            tr("无法填充设置选项", "Could not populate settings choices"),
        ))
    } else {
        Ok(())
    }
}

fn set_choice(hwnd: HWND, index: usize) -> Result<()> {
    if unsafe { SendMessageW(hwnd, CB_SETCURSEL, Some(WPARAM(index)), None) }.0 < 0 {
        Err(Error::new(
            E_FAIL,
            tr("无法选择设置值", "Could not select settings value"),
        ))
    } else {
        Ok(())
    }
}

fn selected_choice(hwnd: HWND) -> Result<usize> {
    let result = unsafe { SendMessageW(hwnd, CB_GETCURSEL, None, None) }.0;
    usize::try_from(result)
        .map_err(|_| Error::new(E_FAIL, tr("请选择有效的选项", "Choose a valid option")))
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
    u8::try_from(value).map_err(|_| {
        Error::new(
            E_FAIL,
            tr("请选择有效的取色外观值", "Choose a valid appearance value"),
        )
    })
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
        WM_PAINT if hwnd == state.viewport.get() => paint_content(
            hwnd,
            state.scroll_offset.get(),
            state.content_width_dip.get(),
        ),
        WM_PAINT => state.theme.paint(hwnd, &[]),
        WM_ERASEBKGND => LRESULT(1),
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX => {
            let id = unsafe { GetDlgCtrlID(HWND(lparam.0 as *mut _)) } as usize;
            let (panel, tone) = match id {
                TITLE => (false, Tone::Text),
                SUBTITLE | LANGUAGE_LABEL => (false, Tone::Muted),
                LANGUAGE => (false, Tone::Text),
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
                PREVIEW_HEADING | PREVIEW_HINT => (true, Tone::Muted),
                APPLY | CLOSE => (false, Tone::Text),
                _ => (true, Tone::Text),
            };
            state
                .theme
                .control_color(HDC(wparam.0 as *mut _), panel, tone)
        }
        WM_NOTIFY => theme::custom_draw(lparam, APPLY)
            .unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }),
        WM_DRAWITEM if wparam.0 == APPEARANCE_PREVIEW && lparam.0 != 0 => {
            let draw = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if let Some(preview) = state.preview.borrow().as_ref()
                && let Err(error) = preview.paint(draw.hDC, draw.rcItem)
            {
                crate::app::diagnostics::event(format_args!(
                    "settings.preview_paint_failed {error}"
                ));
            }
            LRESULT(1)
        }
        WM_CLOSE => {
            state.queue(|pending| pending.close = true);
            LRESULT(0)
        }
        WM_COMMAND if (wparam.0 >> 16) as u32 == BN_CLICKED => {
            match wparam.0 & 0xffff {
                CLOSE => state.queue(|pending| pending.close = true),
                APPLY => state.queue(|pending| pending.apply = true),
                KEY => state.begin_capture(),
                QUICK_PICK => state.queue(|pending| pending.copy_behavior = true),
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
        WM_VSCROLL if hwnd == state.viewport.get() && lparam.0 == 0 => {
            let mut info = SCROLLINFO {
                cbSize: size_of::<SCROLLINFO>() as u32,
                fMask: SIF_ALL,
                ..Default::default()
            };
            if unsafe { GetScrollInfo(hwnd, SB_VERT, &mut info) }.is_ok() {
                let line = dip(28, unsafe { GetDpiForWindow(hwnd) }.max(96));
                let current = state
                    .pending
                    .get()
                    .scroll
                    .unwrap_or(state.scroll_offset.get());
                let next = match SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32) {
                    SB_LINEUP => current - line,
                    SB_LINEDOWN => current + line,
                    SB_PAGEUP => current - info.nPage as i32,
                    SB_PAGEDOWN => current + info.nPage as i32,
                    SB_THUMBPOSITION | SB_THUMBTRACK => info.nTrackPos,
                    SB_TOP => 0,
                    SB_BOTTOM => info.nMax,
                    _ => current,
                };
                state.queue(|pending| pending.scroll = Some(next));
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as u16 as i16) as i32;
            let total = state.wheel_remainder.get() + delta;
            state.wheel_remainder.set(total % 120);
            let steps = total / 120;
            if steps != 0 {
                let current = state
                    .pending
                    .get()
                    .scroll
                    .unwrap_or(state.scroll_offset.get());
                let next = current - steps * dip(72, unsafe { GetDpiForWindow(hwnd) }.max(96));
                state.queue(|pending| pending.scroll = Some(next));
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
        WM_SIZE if hwnd != state.viewport.get() && wparam.0 != SIZE_MINIMIZED as usize => {
            state.queue(|pending| pending.layout = true);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE if hwnd != state.viewport.get() => {
            state.queue(|pending| pending.fit_work_area = true);
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

#[cfg(test)]
mod responsive_layout_tests {
    use super::*;

    #[test]
    fn settings_content_reflows_without_horizontal_clipping_or_hidden_focus() {
        for width in [260, 300, 364, 430, 459, 460, 480] {
            let geometry = settings_content_layout(width);
            assert_eq!(geometry.controls.len(), 28);
            for (id, r) in geometry.controls {
                assert!(
                    r.left >= 0 && r.right <= width && r.right > r.left,
                    "{id} at {width}: {r:?}"
                );
                assert!(r.top >= 0 && r.bottom <= geometry.height);
                if matches!(
                    id,
                    CTRL | ALT
                        | SHIFT
                        | KEY
                        | FORMAT
                        | LANGUAGE
                        | QUICK_PICK
                        | AUTO_COPY
                        | BORDER_WIDTH
                        | BACKGROUND_TRANSPARENCY
                ) {
                    let offset = reveal_offset(0, r.top, r.bottom, 80, geometry.height, 8);
                    assert!(
                        r.top - offset >= 0 && r.bottom - offset <= 80,
                        "{id} at {width}"
                    );
                }
            }
            for r in geometry.panels {
                assert!(r.left >= 0 && r.right <= width && r.bottom <= geometry.height);
            }
        }
    }
}
