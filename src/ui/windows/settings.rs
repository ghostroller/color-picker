//! Native settings editor. Only Apply returns a validated draft; the host owns
//! persistence and the hotkey transaction. Callbacks never perform either task.

use super::messages;
use super::window_lifetime::{WindowInit, WindowLifetime, WindowRole};
use crate::platform::windows::gdi::{PaintSession, SavedDc, WindowDc};

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
            COLORREF, E_FAIL, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT,
            POINT, RECT, WPARAM,
        },
        Graphics::{
            Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute},
            Gdi::*,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            SystemServices::{SS_CENTERIMAGE, SS_NOPREFIX, SS_OWNERDRAW, SS_RIGHT},
        },
        UI::{
            Controls::{
                EM_GETLINECOUNT, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
                SetScrollInfo, ShowScrollBar, TBM_SETPAGESIZE, TBM_SETPOS, TBM_SETRANGEMAX,
                TBM_SETRANGEMIN, TBS_NOTICKS, TRACKBAR_CLASSW,
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
        i18n::{Language, language, tr},
    },
    core::format::ColorFormat,
};

#[path = "settings_preview.rs"]
mod settings_preview;
use settings_preview::AppearancePreview;

pub const WM_SETTINGS_WAKE: u32 = WM_APP + 12;
const CLASS: PCWSTR = w!("ColorPicker.Settings.v1");
const VIEWPORT_CLASS: PCWSTR = w!("ColorPicker.SettingsViewport.v1");
const CONTENT_CLASS: PCWSTR = w!("ColorPicker.SettingsContent.v1");
const CONTENT: usize = 200;
const FORM: usize = 201;
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
const STATUS_DETAILS: usize = 29;
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
    lifetime: WindowLifetime,
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
    root: Cell<HWND>,
    viewport: Cell<HWND>,
    content: Cell<HWND>,
    scroll_offset: Cell<i32>,
    viewport_height: Cell<i32>,
    content_height: Cell<i32>,
    content_panels: RefCell<Vec<RECT>>,
    wheel_remainder: Cell<i32>,
    preview: RefCell<Option<AppearancePreview>>,
}

impl CallbackState {
    /// Pure scrolling never remeasures or moves individual controls. Run synchronously
    /// so native scrollbar tracking also works inside USER32's modal tracking loop.
    fn scroll_to(&self, offset: i32) -> Result<()> {
        let offset = offset.clamp(
            0,
            (self.content_height.get() - self.viewport_height.get()).max(0),
        );
        if self.scroll_offset.get() == offset {
            return Ok(());
        }
        // SAFETY: The root retains this content HWND and its callback on this UI thread.
        // Its children retain stable content-local coordinates; no Rust borrow spans dispatch.
        unsafe {
            SetWindowPos(
                self.content.get(),
                None,
                0,
                -offset,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS,
            )?;
        }
        self.scroll_offset.set(offset);
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_POS,
            nPos: offset,
            ..Default::default()
        };
        // SAFETY: This owned viewport's range was established by layout. USER32 copies
        // the scalar scroll state and paints the clipped, composited content subtree.
        unsafe {
            SetScrollInfo(self.viewport.get(), SB_VERT, &info, true);
            let _ = RedrawWindow(
                Some(self.viewport.get()),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
            );
        }
        Ok(())
    }

    fn defer(&self, update: impl FnOnce(&mut Pending)) {
        let mut pending = self.pending.get();
        update(&mut pending);
        self.pending.set(pending);
    }

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
        self.defer(update);
        if !self.wake_posted.replace(true)
            // SAFETY: The host outlives this window's callback tree; this private wake message
            // carries scalar values only and retains no Rust reference.
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
    status_details: HWND,
}

impl Controls {
    fn handles(&self) -> [HWND; 32] {
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
            self.status_details,
        ]
    }
}

#[derive(Default)]
struct FontState {
    dpi: u32,
    small_line_height: i32,
    _fonts: Vec<Font>,
}

impl FontState {
    fn retain_fonts(&self) {
        for font in &self._fonts {
            font.retain();
        }
    }
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
    rendered_language: Cell<Option<Language>>,
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
        // SAFETY: None borrows the process module containing the static callback; the module
        // lives for the process and is never freed by this window owner.
        let instance = unsafe { GetModuleHandleW(None)? }.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            // SAFETY: IDC_ARROW is a documented system cursor identifier. The returned shared
            // cursor is borrowed by the class and is never destroyed as an owned resource.
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
            lpszClassName: CLASS,
            ..Default::default()
        };
        for name in [CLASS, VIEWPORT_CLASS, CONTENT_CLASS] {
            let class = WNDCLASSW {
                lpszClassName: name,
                ..class
            };
            // SAFETY: The initialized descriptor refers to static class names and callbacks;
            // registration copies it synchronously and these references outlive all windows.
            if unsafe { RegisterClassW(&class) } == 0
                // SAFETY: Read this thread's last-error value immediately after class registration;
                // no pointer or borrowed resource is involved.
                && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
            {
                return Err(Error::from_thread());
            }
        }
        let callback = Box::new(CallbackState {
            lifetime: WindowLifetime::default(),
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
            root: Cell::new(HWND::default()),
            viewport: Cell::new(HWND::default()),
            content: Cell::new(HWND::default()),
            scroll_offset: Cell::new(0),
            viewport_height: Cell::new(0),
            content_height: Cell::new(0),
            content_panels: RefCell::new(Vec::new()),
            wheel_remainder: Cell::new(0),
            preview: RefCell::new(None),
        });
        let pointer = callback.as_ref() as *const CallbackState;
        let init = WindowInit {
            state: pointer.cast(),
            role: WindowRole::Root,
        };
        let mut cursor = POINT::default();
        // SAFETY: cursor is initialized, writable POINT storage retained through this query.
        unsafe { GetCursorPos(&mut cursor)? };
        // SAFETY: The callback Box stays stable while synchronous creation reads stack
        // WindowInit and text; only the Box pointer is retained, checked by creation_result.
        let hwnd = unsafe {
            super::window_lifetime::create_window(|| {
                CreateWindowExW(
                    EX_STYLE,
                    CLASS,
                    PCWSTR(wide(tr("设置 — Color Picker", "Settings — Color Picker")).as_ptr()),
                    super::window_lifetime::initial_window_style(STYLE),
                    cursor.x,
                    cursor.y,
                    1,
                    1,
                    Some(notify),
                    None,
                    Some(instance),
                    Some((&init as *const WindowInit).cast()),
                )
            })
        };
        let hwnd = callback.lifetime.creation_result("settings", hwnd)?;
        callback.root.set(hwnd);
        let mut window = Self {
            hwnd,
            callback,
            controls: Controls::default(),
            font: RefCell::new(FontState::default()),
            language_font: RefCell::new(None),
            rendered_language: Cell::new(None),
            schema_version: config.schema_version,
            save_allowed,
            _thread_affinity: PhantomData,
        };
        let disable_transitions = BOOL::from(true);
        // SAFETY: The root owner is already established. The local attribute value has the
        // matching native type/size and remains valid through this synchronous copy.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&disable_transitions as *const BOOL).cast(),
                size_of::<BOOL>() as u32,
            )?;
        }
        theme::configure_window(hwnd, &window.callback.theme);
        let init = WindowInit {
            state: pointer.cast(),
            role: WindowRole::Content,
        };
        // SAFETY: The root owner retains callback state and owns partial-child cleanup.
        // The stack content WindowInit/text live through this synchronous creation only.
        let viewport = unsafe {
            super::window_lifetime::create_window(|| {
                CreateWindowExW(
                    WS_EX_CONTROLPARENT | WS_EX_COMPOSITED,
                    VIEWPORT_CLASS,
                    PCWSTR(wide(tr("设置内容", "Settings content")).as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_VSCROLL,
                    0,
                    0,
                    1,
                    1,
                    Some(hwnd),
                    Some(HMENU(CONTENT as *mut _)),
                    Some(instance),
                    Some((&init as *const WindowInit).cast()),
                )
            })?
        };
        window.callback.viewport.set(viewport);
        // SAFETY: The root owns both container levels. The same stable callback allocation
        // is retained through descendant destruction; WindowInit is copied synchronously.
        let content = unsafe {
            super::window_lifetime::create_window(|| {
                CreateWindowExW(
                    WS_EX_CONTROLPARENT,
                    CONTENT_CLASS,
                    PCWSTR(wide(tr("设置表单", "Settings form")).as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                    0,
                    0,
                    1,
                    1,
                    Some(viewport),
                    Some(HMENU(FORM as *mut _)),
                    Some(instance),
                    Some((&init as *const WindowInit).cast()),
                )
            })?
        };
        window.callback.content.set(content);
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
        // SAFETY: The target belongs to this live control tree; the scalar enabled state
        // retains no pointer and no callback-state RefMut spans synchronous dispatch.
        unsafe {
            let _ = EnableWindow(window.controls.apply, save_allowed);
        }
        if super::window_lifetime::show_native_windows() {
            // SAFETY: The window/control belongs to the live UI-thread owner. These synchronous
            // calls occur without a held callback-state mutable borrow.
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(window.controls.ctrl));
            }
        }
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Relabel the existing draft only after the host successfully applies it.
    /// Controls, selections and scroll position stay intact during the switch.
    /// An unchanged language leaves fonts, preview and layout untouched.
    pub fn refresh_language(&self) -> Result<()> {
        let current_language = language();
        if self.rendered_language.get() == Some(current_language) {
            return Ok(());
        }
        // A failed refresh must remain retryable, including after switching back.
        self.rendered_language.set(None);
        for (hwnd, text) in [
            (
                self.hwnd,
                tr("设置 — Color Picker", "Settings — Color Picker"),
            ),
            (
                self.callback.viewport.get(),
                tr("设置内容", "Settings content"),
            ),
            (self.callback.content.get(), tr("设置表单", "Settings form")),
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
            // SAFETY: The target control belongs to this owner and the terminated UTF-16 string
            // lives until SetWindowTextW finishes copying it; no text pointer is retained.
            unsafe { SetWindowTextW(hwnd, PCWSTR(wide(text).as_ptr()))? };
        }
        self.update_capture_text()?;
        self.font.borrow_mut().dpi = 0;
        self.callback.preview.replace(None);
        self.layout()?;
        self.update_default_style();
        self.rendered_language.set(Some(current_language));
        Ok(())
    }

    /// Run before IsDialogMessage: a key consumed during recording stays owned
    /// until release, including after Tab/click moves focus to another control.
    /// This is window-local message filtering, not a global keyboard hook.
    pub fn filter_key_message(&self, message: &MSG) -> bool {
        // SAFETY: Both identifiers are borrowed; USER32 validates their descendant relation
        // without dereferencing any Rust storage or taking HWND ownership.
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
        // SAFETY: Both status controls belong to this owner; the terminated UTF-16 text
        // lives through synchronous copying and no callback-state RefMut is held.
        unsafe {
            SetWindowTextW(self.controls.status, PCWSTR(text.as_ptr()))?;
            SetWindowTextW(self.controls.status_details, PCWSTR(text.as_ptr()))?;
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
        let mut pending = self.callback.pending.take();
        if pending.close {
            self.callback.recording.set(false);
            self.callback.closing.set(true);
            return Ok(Some(SettingsAction::Close));
        }
        // A minimized window has an icon rectangle and a zero client area.
        // Keep geometry work for restoration without delaying Apply or Close.
        // DPI suggestions made for that icon are replaced by a fit using the
        // restored window's actual rectangle and DPI.
        // SAFETY: this UI-thread owner retains the root throughout this query.
        if unsafe { IsIconic(self.hwnd) }.as_bool() {
            let dpi_changed = pending.dpi_rect.take().is_some();
            self.callback.defer(|deferred| {
                deferred.fit_work_area |= std::mem::take(&mut pending.fit_work_area) || dpi_changed;
                deferred.layout |= std::mem::take(&mut pending.layout);
                deferred.reveal = pending.reveal.take().or(deferred.reveal);
            });
        }
        if let Some(rect) = pending.dpi_rect {
            let rect = fit_to_work_area(rect, self.minimum_size()?)?;
            // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
            // passed by value and no callback-state RefMut spans synchronous layout reentry.
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
            // SAFETY: The live window owner or callback retains this HWND. The initialized local
            // RECT is writable for exactly this synchronous query and is not retained.
            unsafe { GetWindowRect(self.hwnd, &mut rect)? };
            let rect = fit_to_work_area(rect, self.minimum_size()?)?;
            // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
            // passed by value and no callback-state RefMut spans synchronous layout reentry.
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
        if pending.layout || pending.fit_work_area {
            self.layout()?;
            // SAFETY: This queries a borrowed HWND from the current UI thread; reveal_control
            // checks membership in the owned viewport before using its geometry.
            self.reveal_control(unsafe { GetFocus() })?;
        }
        if let Some(hwnd) = pending.reveal {
            self.reveal_control(hwnd)?;
        }
        if pending.default_style {
            self.update_default_style();
        }
        if pending.capture {
            self.update_capture_text()?;
            self.layout()?;
        }
        if pending.focus_key && self.callback.recording.get() {
            // SAFETY: The window/control belongs to the live UI-thread owner. These synchronous
            // calls occur without a held callback-state mutable borrow.
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
        // SAFETY: INITCOMMONCONTROLSEX advertises its actual size and lives for this call;
        // the requested trackbar classes retain no pointers to the descriptor.
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
        self.controls.language_label = self.control(
            w!("STATIC"),
            "",
            LANGUAGE_LABEL,
            label | WINDOW_STYLE(SS_CENTERIMAGE.0),
        )?;
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
        // SAFETY: The key button is a child of this root. Its subclass borrows the stable
        // callback Box until child WM_NCDESTROY, which precedes root-state release.
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
        self.controls.border_value = self.control(
            w!("STATIC"),
            "",
            BORDER_VALUE,
            label | WINDOW_STYLE(SS_RIGHT.0),
        )?;
        self.controls.transparency_label =
            self.control(w!("STATIC"), "", TRANSPARENCY_LABEL, label)?;
        self.controls.background_transparency =
            self.control(TRACKBAR_CLASSW, "", BACKGROUND_TRANSPARENCY, slider)?;
        self.controls.transparency_value = self.control(
            w!("STATIC"),
            "",
            TRANSPARENCY_VALUE,
            label | WINDOW_STYLE(SS_RIGHT.0),
        )?;
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
        self.controls.status = self.control(w!("STATIC"), "", STATUS, label)?;
        // Ordinary hints are labels, without selection or a Tab stop. Retain a
        // scrollable detail view only when a long diagnostic does not fit.
        self.controls.status_details = self.control(
            w!("EDIT"),
            "",
            STATUS_DETAILS,
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
        let footer = matches!(id, APPLY | CLOSE | STATUS | STATUS_DETAILS);
        // SAFETY: class/text are terminated strings read only during synchronous creation.
        // The root owner owns this child tree, including cleanup after partial failure.
        let hwnd = unsafe {
            super::window_lifetime::create_window(|| {
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
                        self.callback.content.get()
                    }),
                    Some(HMENU(id as *mut _)),
                    Some(GetModuleHandleW(None)?.into()),
                    None,
                )
            })?
        };
        if !footer
            // SAFETY: This content control belongs to the root tree. The stable Box referenced
            // by its subclass outlives child teardown and the subclass removes itself.
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
            // SAFETY: The target control belongs to this owner and the terminated UTF-16 string
            // lives until SetWindowTextW finishes copying it; no text pointer is retained.
            unsafe { SetWindowTextW(hwnd, PCWSTR(wide(&text).as_ptr()))? };
        }
        self.update_appearance_preview()
    }

    fn update_copy_behavior(&self) {
        // Keep the ordinary-mode preference intact, even when quick picking
        // makes copying mandatory. Unchecking quick mode restores this choice.
        let _ =
            // SAFETY: The target belongs to this live control tree; the scalar enabled state
            // retains no pointer and no callback-state RefMut spans synchronous dispatch.
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
            // SAFETY: The preview control belongs to this owner; the description is a live
            // terminated temporary copied synchronously, and invalidation retains no pointer.
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
        // SAFETY: The button/hint belong to this owner. Each temporary UTF-16 string lives
        // through its synchronous SetWindowTextW call; no borrowed text is retained.
        unsafe {
            SetWindowTextW(self.controls.key, PCWSTR(wide(&button).as_ptr()))?;
            SetWindowTextW(self.controls.hint, PCWSTR(wide(hint).as_ptr()))?;
        }
        Ok(())
    }

    fn dpi(&self) -> Result<u32> {
        // SAFETY: The current callback or window owner retains this HWND through the scalar
        // DPI query; no native handle ownership is transferred.
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
        // SAFETY: The screen coordinate is passed by value and the query returns a borrowed
        // monitor identifier; it creates or retains no Rust references.
        let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: The monitor came from the native lookup; initialized MONITORINFO advertises
        // its exact size and stays writable for the duration of this query.
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return Err(Error::from_thread());
        }
        let work = info.rcWork;
        let center_x = ((i64::from(work.left) + i64::from(work.right)) / 2) as i32;
        let center_y = ((i64::from(work.top) + i64::from(work.bottom)) / 2) as i32;
        // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
        // passed by value and no callback-state RefMut spans synchronous layout reentry.
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
        // SAFETY: outer is initialized local RECT storage; styles and validated DPI are
        // values, and the geometry calculation retains no pointers.
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
        // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
        // passed by value and no callback-state RefMut spans synchronous layout reentry.
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
        // SAFETY: outer is initialized local RECT storage; styles and validated DPI are
        // values, and the geometry calculation retains no pointers.
        unsafe { AdjustWindowRectExForDpi(&mut outer, STYLE, false, EX_STYLE, dpi)? };
        Ok((outer.right - outer.left, outer.bottom - outer.top))
    }

    fn layout(&self) -> Result<()> {
        let result = self.layout_content();
        // Native placement can fail after part of a SWP_NOREDRAW batch has been
        // applied. Always invalidate the complete tree, including error paths,
        // so suppressed painting never leaves an incomplete frame on screen.
        // SAFETY: This owner retains the complete window tree on its UI thread;
        // invalidation borrows no Rust storage and requests painting after return.
        let _ = unsafe {
            RedrawWindow(
                Some(self.hwnd),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME,
            )
        };
        result
    }

    fn layout_content(&self) -> Result<()> {
        // refresh_language can request layout outside process_pending, including
        // when an Apply was queued just before minimizing the settings window.
        // SAFETY: this UI-thread owner retains the root throughout this query.
        if unsafe { IsIconic(self.hwnd) }.as_bool() {
            self.callback.defer(|pending| pending.layout = true);
            return Ok(());
        }
        self.callback.theme.update_window_icons(self.hwnd);
        let dpi = self.dpi()?;
        if self
            .language_font
            .borrow()
            .as_ref()
            .is_none_or(|(font_dpi, _)| *font_dpi != dpi)
        {
            let font = Font::for_language(14, dpi, 400, false, Language::SimplifiedChinese)?;
            // SAFETY: The new language font remains owned locally until stored in language_font;
            // WM_SETFONT replaces the control reference before the previous font is dropped.
            unsafe {
                SendMessageW(
                    self.controls.language,
                    WM_SETFONT,
                    Some(WPARAM(font.raw().0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            let old = self.language_font.replace(Some((dpi, font)));
            if self.callback.theme.fonts_must_be_retained()
                && let Some((_, font)) = &old
            {
                font.retain();
            }
            drop(old);
        }
        if self.font.borrow().dpi != dpi {
            let body = Font::new(14, dpi, 400, false)?;
            let title = Font::new(20, dpi, 600, false)?;
            let heading = Font::new(14, dpi, 600, false)?;
            let small = Font::new(12, dpi, 400, false)?;
            let small_line_height = font_line_height(self.controls.status, &small, dpi);
            for hwnd in self.controls.handles() {
                if hwnd == self.controls.language {
                    continue;
                }
                // SAFETY: All new fonts were successfully created before notifications begin; these
                // owned controls switch fonts before replacing and dropping the old FontState.
                unsafe {
                    SendMessageW(
                        hwnd,
                        WM_SETFONT,
                        Some(WPARAM(body.raw().0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
            for (hwnd, font) in [
                (self.controls.title, title.raw()),
                (self.controls.hotkey_label, heading.raw()),
                (self.controls.copy_heading, heading.raw()),
                (self.controls.appearance_heading, heading.raw()),
                (self.controls.usage_heading, small.raw()),
                (self.controls.subtitle, small.raw()),
                (self.controls.hint, small.raw()),
                (self.controls.usage_hint, small.raw()),
                (self.controls.status, small.raw()),
                (self.controls.status_details, small.raw()),
                (self.controls.preview_heading, small.raw()),
                (self.controls.preview_hint, small.raw()),
            ] {
                // SAFETY: The new body/heading font owners remain live through these synchronous
                // WM_SETFONT calls and are retained in FontState after all controls switch.
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
            if self.callback.theme.fonts_must_be_retained() {
                old.retain_fonts();
            }
            drop(old);
        }
        let mut client = RECT::default();
        // SAFETY: The live window owner or callback retains this HWND. The initialized local
        // RECT is writable for exactly this synchronous query and is not retained.
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
        // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
        // passed by value and no callback-state RefMut spans synchronous layout reentry.
        unsafe { MoveWindow(viewport, 0, 0, client.right, viewport_height, true)? };
        self.callback.viewport_height.set(viewport_height);
        for combo in [self.controls.language, self.controls.format] {
            // SAFETY: These owned COMBOBOX controls synchronously copy scalar item heights.
            // Set the selection height before measuring and arranging their form rows.
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
        // Start at the full width so a previously visible scrollbar cannot keep
        // itself alive through the narrower responsive layout. Reserve space
        // only when content actually needs scrolling, using the native client.
        let full_width = ((i64::from(client.right) * 96) / i64::from(dpi)) as i32;
        let fields = self.field_metrics(dpi)?;
        let mut measure = |id, width, wrap| self.measure_text(id, width, wrap, dpi);
        let full_geometry = settings_content_layout(full_width, fields, &mut measure)?;
        let needs_scroll = dip(full_geometry.height, dpi) > viewport_height;
        let mut viewport_client = RECT::default();
        // SAFETY: The owner retains the viewport during scrollbar reentry. viewport_client
        // is local writable RECT storage; no callback-state mutable borrow spans calls.
        unsafe {
            ShowScrollBar(viewport, SB_VERT, needs_scroll)?;
            GetClientRect(viewport, &mut viewport_client)?;
        }
        let content_width = ((i64::from(viewport_client.right) * 96) / i64::from(dpi)) as i32;
        let geometry = if content_width == full_width {
            full_geometry
        } else {
            // The native scrollbar removes actual client pixels. Remeasure wrapping
            // against that width instead of subtracting an assumed scrollbar metric.
            settings_content_layout(content_width, fields, &mut measure)?
        };
        let content_height = dip(geometry.height, dpi);
        self.callback.content_height.set(content_height);
        self.callback.content_panels.replace(geometry.panels);
        let offset = self
            .callback
            .scroll_offset
            .get()
            .clamp(0, (content_height - viewport_height).max(0));
        self.callback.scroll_offset.set(offset);
        // SAFETY: The form is the viewport's only child; its stable local coordinates
        // include every control and card. Suppress intermediate painting during layout.
        unsafe {
            SetWindowPos(
                self.callback.content.get(),
                None,
                0,
                -offset,
                viewport_client.right,
                content_height.max(viewport_height),
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOREDRAW | SWP_NOCOPYBITS,
            )?;
        }
        let scroll = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: content_height - 1,
            nPage: viewport_height as u32,
            nPos: offset,
            ..Default::default()
        };
        // SAFETY: scroll is an initialized, correctly sized local SCROLLINFO for the owned
        // viewport; USER32 copies it synchronously and retains no Rust reference.
        unsafe {
            SetScrollInfo(viewport, SB_VERT, &scroll, true);
        }
        let mut positions = Vec::with_capacity(geometry.controls.len());
        for (id, rect) in geometry.controls {
            // SAFETY: The live owner retains the parent and child tree. This query borrows an
            // existing child HWND and transfers no ownership.
            let control = unsafe { GetDlgItem(Some(self.callback.content.get()), id as i32)? };
            // COMBOBOX uses the requested height for its opened dropdown.
            let height = if matches!(id, LANGUAGE | FORMAT) {
                160
            } else {
                rect.bottom - rect.top
            };
            positions.push((
                control,
                dip(rect.left, dpi),
                dip(rect.top, dpi),
                dip(rect.right - rect.left, dpi),
                dip(height, dpi),
            ));
        }
        // SAFETY: Every HWND is an owned sibling in the content window. USER32 owns
        // the batch, discarding it on DeferWindowPos failure; no Rust borrow spans calls.
        unsafe {
            let mut batch = BeginDeferWindowPos(positions.len() as i32)?;
            for (hwnd, x, y, width, height) in positions {
                batch = DeferWindowPos(
                    batch,
                    hwnd,
                    None,
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOREDRAW | SWP_NOCOPYBITS,
                )?;
            }
            EndDeferWindowPos(batch)?;
        }
        let footer_top = panes.actions.top + dip(8, dpi);
        let button_width = dip(92, dpi);
        let gap = dip(12, dpi);
        let margin = dip(if content_width >= 460 { 24 } else { 16 }, dpi);
        let apply_x = client.right - margin - button_width;
        let close_x = apply_x - gap - button_width;
        let button_y = footer_top + if narrow { dip(44, dpi) } else { 0 };
        for status in [self.controls.status, self.controls.status_details] {
            // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
            // passed by value and no callback-state RefMut spans synchronous layout reentry.
            unsafe {
                MoveWindow(
                    status,
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
            }
        }
        // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
        // passed by value and no callback-state RefMut spans synchronous layout reentry.
        unsafe {
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
        self.update_status_scrollbar()?;
        self.update_appearance_preview()?;
        Ok(())
    }

    fn field_metrics(&self, dpi: u32) -> Result<FieldMetrics> {
        let height = |control: HWND| -> Result<i32> {
            let mut bounds = RECT::default();
            // A dropdown-list combo recomputes its collapsed height when USER32
            // applies the requested dropdown size. CB_SETITEMHEIGHT alone can
            // therefore report an intermediate height that arranging replaces.
            // Prepare the same dropdown size used by the final placement before
            // asking for the native collapsed height; suppress intermediate paint.
            // SAFETY: This owner retains the native COMBOBOX on its UI thread.
            // Only value geometry is passed and no Rust borrow spans dispatch.
            unsafe {
                SetWindowPos(
                    control,
                    None,
                    0,
                    0,
                    dip(180, dpi),
                    dip(160, dpi),
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW | SWP_NOCOPYBITS,
                )?;
                GetWindowRect(control, &mut bounds)?;
            }
            let pixels = (bounds.bottom - bounds.top).max(1);
            Ok(((i64::from(pixels) * 96 + i64::from(dpi) - 1) / i64::from(dpi)) as i32)
        };
        Ok(FieldMetrics {
            language_height: height(self.controls.language)?,
            format_height: height(self.controls.format)?,
        })
    }

    fn measure_text(&self, id: usize, width: i32, wrap: bool, dpi: u32) -> Result<TextExtent> {
        // SAFETY: The root owns this content tree and its controls on this UI thread.
        // The borrowed HWND and its currently assigned font remain live throughout measurement.
        let control = unsafe { GetDlgItem(Some(self.callback.content.get()), id as i32)? };
        // SAFETY: This live owned HWND supplies its scalar text length and font handle.
        let length = unsafe { GetWindowTextLengthW(control) }.max(0) as usize;
        let mut text = vec![0_u16; length + 1];
        // SAFETY: The writable UTF-16 slice advertises its full capacity; USER32 retains no pointer.
        let copied = unsafe { GetWindowTextW(control, &mut text) }.max(0) as usize;
        text.truncate(copied);
        // SAFETY: The control is retained by self until after this scoped DC lease ends.
        let dc = unsafe { WindowDc::new(control)? };
        // SAFETY: The DC lease and control's assigned font outlive the saved-state guard.
        let saved = unsafe { SavedDc::new(dc.raw())? };
        let measured = (|| {
            // SAFETY: The scalar WM_GETFONT response borrows the live control's assigned font;
            // SaveDC restores its selection before the DC or any font owner can be released.
            let font = unsafe { SendMessageW(control, WM_GETFONT, None, None) };
            if font.0 != 0 {
                // SAFETY: The control owns the font reference and this method keeps self alive.
                let previous = unsafe { SelectObject(dc.raw(), HGDIOBJ(font.0 as *mut _)) };
                if crate::platform::windows::gdi::invalid_selection(previous) {
                    return Err(Error::new(
                        E_FAIL,
                        "Could not select the settings text font",
                    ));
                }
            }
            let mut metrics = TEXTMETRICW::default();
            // SAFETY: metrics is initialized writable output for the scoped live window DC.
            if !unsafe { GetTextMetricsW(dc.raw(), &mut metrics) }.as_bool() {
                return Err(Error::new(
                    E_FAIL,
                    "Could not measure settings text metrics",
                ));
            }
            let mut bounds = RECT {
                right: dip(width.max(1), dpi),
                ..Default::default()
            };
            if !text.is_empty() {
                let flags =
                    DT_CALCRECT | DT_NOPREFIX | if wrap { DT_WORDBREAK } else { DT_SINGLELINE };
                // SAFETY: DrawText only reads this mutable UTF-16 slice without DT_MODIFYSTRING;
                // bounds is writable local storage and the selected font remains retained.
                if unsafe { DrawTextW(dc.raw(), &mut text, &mut bounds, flags) } == 0 {
                    return Err(Error::new(E_FAIL, "Could not measure settings text"));
                }
            }
            let to_dip = |pixels: i32| {
                ((i64::from(pixels.max(0)) * 96 + i64::from(dpi) - 1) / i64::from(dpi)) as i32
            };
            Ok(TextExtent {
                width: to_dip(bounds.right - bounds.left),
                height: to_dip((bounds.bottom - bounds.top).max(metrics.tmHeight)) + 2,
            })
        })();
        if let Err(error) = saved.restore() {
            // A failed RestoreDC leaves the borrowed font's selection uncertain. Keep every
            // possible owner alive, including after a later language or DPI replacement.
            self.font.borrow().retain_fonts();
            if let Some((_, font)) = self.language_font.borrow().as_ref() {
                font.retain();
            }
            return Err(error);
        }
        measured
    }

    fn reveal_control(&self, hwnd: HWND) -> Result<()> {
        let viewport = self.callback.viewport.get();
        // SAFETY: Both identifiers are borrowed; USER32 validates their descendant relation
        // without dereferencing any Rust storage or taking HWND ownership.
        if !unsafe { IsChild(viewport, hwnd) }.as_bool() {
            return Ok(());
        }
        let mut rect = RECT::default();
        let mut origin = POINT::default();
        // SAFETY: The verified descendant and viewport remain owned here. RECT/POINT are
        // valid local output slots used only by these synchronous geometry queries.
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
        self.callback.scroll_to(offset)?;
        Ok(())
    }

    fn update_status_scrollbar(&self) -> Result<()> {
        let line_height = self.font.borrow().small_line_height.max(1);
        // Measure wrapping in the hidden edit. Short statuses use a static label;
        // long diagnostics keep the existing readable, scrollable detail view.
        let details = self.controls.status_details;
        // SAFETY: The status EDIT belongs to this window and lives through this scalar
        // scrollbar change; no RefCell borrow spans native reentrant processing.
        unsafe {
            ShowScrollBar(details, SB_VERT, false)?;
        }
        let mut client = RECT::default();
        // SAFETY: The live window owner or callback retains this HWND. The initialized local
        // RECT is writable for exactly this synchronous query and is not retained.
        unsafe {
            GetClientRect(details, &mut client)?;
        }
        // SAFETY: The helper uses an owned control and a documented scalar-only message;
        // synchronous processing retains no Rust pointer or mutable state borrow.
        let lines = unsafe { SendMessageW(details, EM_GETLINECOUNT, None, None) }.0;
        let visible_lines = ((client.bottom - client.top) / line_height).max(1);
        let overflow = lines > visible_lines as isize;
        // SAFETY: The live status/action controls belong to this owner. Focus/style updates
        // use values only and occur outside callback-state mutable borrows.
        unsafe {
            if !overflow && GetFocus() == details {
                let _ = SetFocus(Some(self.controls.close));
            }
            ShowScrollBar(details, SB_VERT, overflow)?;
            let _ = ShowWindow(details, if overflow { SW_SHOWNA } else { SW_HIDE });
            let _ = ShowWindow(
                self.controls.status,
                if overflow { SW_HIDE } else { SW_SHOWNA },
            );
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
            // SAFETY: The owned BUTTON controls receive only scalar BM_SETSTYLE flags; this
            // synchronous dispatch occurs without a callback-state RefMut.
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
        self.callback.lifetime.destroy("settings");
        // Include failures from teardown reentry before any font owner drops.
        if self.callback.theme.fonts_must_be_retained() {
            self.font.get_mut().retain_fonts();
            if let Some((_, font)) = self.language_font.get_mut() {
                font.retain();
            }
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[derive(Default)]
struct SettingsContentLayout {
    controls: Vec<(usize, RECT)>,
    panels: Vec<RECT>,
    height: i32,
}

#[derive(Clone, Copy)]
struct TextExtent {
    width: i32,
    height: i32,
}

#[derive(Clone, Copy)]
struct FieldMetrics {
    language_height: i32,
    format_height: i32,
}

fn content_rect(x: i32, y: i32, width: i32, height: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

/// All rectangles use content-local DIPs. Measuring is injected so tests use real
/// fonts at each DPI while painting consumes only the completed cached geometry.
fn settings_content_layout(
    width: i32,
    fields: FieldMetrics,
    measure: &mut impl FnMut(usize, i32, bool) -> Result<TextExtent>,
) -> Result<SettingsContentLayout> {
    let margin = if width >= 460 { 24 } else { 16 };
    let card_width = width - 2 * margin;
    let left = margin + 16;
    let inner = card_width - 32;
    if inner <= 0 {
        return Err(layout::unavailable());
    }
    let editor_width = 180.min(inner);
    let label_width = [LANGUAGE_LABEL, 11, 13]
        .into_iter()
        .map(|id| measure(id, inner, false).map(|text| text.width))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
        .unwrap_or(0);
    let inline_fields = inner >= label_width + 12 + editor_width;
    let mut form = SettingsContentLayout::default();
    let mut y = 22;
    y += place_text(&mut form, measure, TITLE, margin, y, card_width, 30)?;
    y += 8;
    y += place_text(&mut form, measure, SUBTITLE, margin, y, card_width, 18)?;
    y += 20;

    let top = y;
    y += 16;
    y += place_field(
        &mut form,
        measure,
        LANGUAGE_LABEL,
        LANGUAGE,
        left,
        y,
        inner,
        editor_width,
        fields.language_height,
        inline_fields,
    )?;
    y += 16;
    form.panels
        .push(content_rect(margin, top, card_width, y - top));
    y += 12;

    let top = y;
    y += 16;
    y += place_text(&mut form, measure, 10, left, y, inner, 22)?;
    y += 12;
    let mut check_x = left;
    let mut check_y = y;
    let mut check_height = 0;
    for id in [CTRL, ALT, SHIFT] {
        let natural = measure(id, inner, false)?;
        let check_width = (natural.width + 28).min(inner);
        if check_x > left && check_x + check_width > left + inner {
            check_y += check_height + 8;
            check_x = left;
            check_height = 0;
        }
        let height = place_checkbox(&mut form, measure, id, check_x, check_y, check_width)?;
        check_height = check_height.max(height);
        check_x += check_width + 20;
    }
    y = check_y + check_height + 16;
    y += place_field(
        &mut form,
        measure,
        11,
        KEY,
        left,
        y,
        inner,
        editor_width,
        32,
        inline_fields,
    )?;
    y += 12;
    y += place_text(&mut form, measure, 12, left, y, inner, 32)?;
    y += 16;
    form.panels
        .push(content_rect(margin, top, card_width, y - top));
    y += 12;

    let top = y;
    y += 16;
    y += place_text(&mut form, measure, COPY_HEADING, left, y, inner, 22)?;
    y += 12;
    y += place_field(
        &mut form,
        measure,
        13,
        FORMAT,
        left,
        y,
        inner,
        editor_width,
        fields.format_height,
        inline_fields,
    )?;
    y += 16;
    y += place_checkbox(&mut form, measure, QUICK_PICK, left, y, inner)?;
    y += 12;
    y += place_checkbox(&mut form, measure, AUTO_COPY, left, y, inner)?;
    y += 16;
    form.panels
        .push(content_rect(margin, top, card_width, y - top));
    y += 12;

    let top = y;
    y += 16;
    y += place_text(&mut form, measure, APPEARANCE_HEADING, left, y, inner, 22)?;
    y += 16;
    // Keep enough space for every value in the slider ranges, so changing a
    // slider never needs to remeasure or rearrange the form.
    let value_width = 64.min(inner / 2);
    for (label, value, slider) in [
        (BORDER_LABEL, BORDER_VALUE, BORDER_WIDTH),
        (
            TRANSPARENCY_LABEL,
            TRANSPARENCY_VALUE,
            BACKGROUND_TRANSPARENCY,
        ),
    ] {
        let label_width = inner - value_width - 12;
        let label_height = measure(label, label_width, true)?.height.max(24);
        let value_height = measure(value, value_width, false)?.height.max(24);
        let row_height = label_height.max(value_height);
        form.controls.push((
            label,
            content_rect(
                left,
                y + (row_height - label_height) / 2,
                label_width,
                label_height,
            ),
        ));
        form.controls.push((
            value,
            content_rect(
                left + inner - value_width,
                y + (row_height - value_height) / 2,
                value_width,
                value_height,
            ),
        ));
        y += row_height + 4;
        form.controls
            .push((slider, content_rect(left, y, inner, 30)));
        y += 42;
    }
    y += 4;
    y += place_text(&mut form, measure, PREVIEW_HEADING, left, y, inner, 18)?;
    y += 8;
    form.controls.push((
        APPEARANCE_PREVIEW,
        content_rect(left, y, inner, settings_preview::HEIGHT),
    ));
    y += settings_preview::HEIGHT + 8;
    y += place_text(&mut form, measure, PREVIEW_HINT, left, y, inner, 18)?;
    y += 16;
    form.panels
        .push(content_rect(margin, top, card_width, y - top));
    y += 20;

    y += place_text(&mut form, measure, USAGE_HEADING, margin, y, card_width, 18)?;
    y += 8;
    y += place_text(&mut form, measure, USAGE_HINT, margin, y, card_width, 32)?;
    form.height = y + 24;
    Ok(form)
}

fn place_text(
    form: &mut SettingsContentLayout,
    measure: &mut impl FnMut(usize, i32, bool) -> Result<TextExtent>,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
    minimum_height: i32,
) -> Result<i32> {
    let height = measure(id, width, true)?.height.max(minimum_height);
    form.controls.push((id, content_rect(x, y, width, height)));
    Ok(height)
}

#[allow(clippy::too_many_arguments)]
fn place_field(
    form: &mut SettingsContentLayout,
    measure: &mut impl FnMut(usize, i32, bool) -> Result<TextExtent>,
    label: usize,
    editor: usize,
    x: i32,
    y: i32,
    width: i32,
    editor_width: i32,
    editor_height: i32,
    inline: bool,
) -> Result<i32> {
    // COMBOBOX dropdowns receive a separate requested window height in layout;
    // only their collapsed selection field participates in the row.
    if inline {
        let label_width = width - editor_width - 12;
        let label_height = measure(label, label_width, true)?.height.max(20);
        let row_height = label_height.max(editor_height).max(32);
        form.controls.push((
            label,
            content_rect(
                x,
                y + (row_height - label_height) / 2,
                label_width,
                label_height,
            ),
        ));
        form.controls.push((
            editor,
            content_rect(
                x + width - editor_width,
                y + (row_height - editor_height) / 2,
                editor_width,
                editor_height,
            ),
        ));
        Ok(row_height)
    } else {
        let label_height = place_text(form, measure, label, x, y, width, 20)?;
        form.controls.push((
            editor,
            content_rect(x, y + label_height + 8, editor_width, editor_height),
        ));
        Ok(label_height + 8 + editor_height)
    }
}

fn place_checkbox(
    form: &mut SettingsContentLayout,
    measure: &mut impl FnMut(usize, i32, bool) -> Result<TextExtent>,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
) -> Result<i32> {
    let height = (measure(id, (width - 24).max(1), true)?.height + 4).max(28);
    form.controls.push((id, content_rect(x, y, width, height)));
    Ok(height)
}

fn paint_content(hwnd: HWND, panels: &[RECT]) -> LRESULT {
    // SAFETY: native WM_PAINT; establish its DC before any mutable state borrow.
    if let Ok(paint) = unsafe { PaintSession::begin(hwnd) } {
        let dc = paint.dc;
        // SAFETY: the paint session outlives this stock-object drawing scope.
        if let Ok(saved) = unsafe { SavedDc::new(dc) } {
            // SAFETY: The current callback or window owner retains this HWND through the scalar
            // DPI query; no native handle ownership is transferred.
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            let mut client = RECT::default();
            // SAFETY: PaintSession keeps this DC/window alive; local RECT output is initialized.
            // Only stock brush/pen objects are selected, with SavedDc restoring attributes.
            unsafe {
                let _ = GetClientRect(hwnd, &mut client);
                SelectObject(dc, GetStockObject(DC_BRUSH));
                SelectObject(dc, GetStockObject(DC_PEN));
                SetDCBrushColor(dc, COLORREF(0xfaf7f5));
                FillRect(dc, &client, HBRUSH(GetStockObject(DC_BRUSH).0));
                SetDCBrushColor(dc, COLORREF(0xffffff));
                SetDCPenColor(dc, COLORREF(0xf0e8e2));
                for panel in panels {
                    let _ = RoundRect(
                        dc,
                        dip(panel.left, dpi),
                        dip(panel.top, dpi),
                        dip(panel.right, dpi),
                        dip(panel.bottom, dpi),
                        dip(16, dpi),
                        dip(16, dpi),
                    );
                }
            }
            if let Err(error) = saved.restore() {
                crate::app::diagnostics::event(format_args!(
                    "settings.paint_restore_failed {error}"
                ));
            }
        }
    }
    LRESULT(0)
}

/// # Safety
/// Invoked by the installed child subclass on its creating UI thread. reference
/// must be this root's stable CallbackState pointer, retained through child
/// WM_NCDESTROY; message parameters obey the native subclass contract.
unsafe extern "system" fn scroll_control_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The installed subclass reference points to this root's stable callback
        // Box. Root teardown keeps it alive until every child/subclass callback returns.
        let state = unsafe { &*(reference as *const CallbackState) };
        match message {
            WM_SETFOCUS => state.queue(|pending| pending.reveal = Some(hwnd)),
            WM_NCDESTROY => {
                // SAFETY: This removes the exact subclass identity installed on this child. The
                // root lifetime keeps callback state alive until all child teardown finishes.
                let _ = unsafe { RemoveWindowSubclass(hwnd, Some(scroll_control_proc), subclass) };
            }
            _ => {}
        }
        // Native sliders and combos retain their wheel/arrow semantics. Other
        // controls let DefWindowProc propagate an unhandled wheel to the pane.
        // SAFETY: The original subclass message and any native payload remain valid until
        // this callback returns; all temporary mutable state borrows have ended.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }))
    .unwrap_or_else(|_| std::process::abort())
}

fn font_line_height(hwnd: HWND, font: &Font, dpi: u32) -> i32 {
    let fallback = dip(16, dpi).max(1);
    // SAFETY: the owned control remains live for this synchronous measurement.
    let Ok(dc) = (unsafe { WindowDc::new(hwnd) }) else {
        return fallback;
    };
    // SAFETY: dc and font remain live until saved state is restored below.
    let Ok(saved) = (unsafe { SavedDc::new(dc.raw()) }) else {
        return fallback;
    };
    // SAFETY: this is the live font owner; only the selection is borrowed by GDI.
    let old = unsafe { SelectObject(dc.raw(), font.raw().into()) };
    if crate::platform::windows::gdi::invalid_selection(old) {
        return fallback;
    }
    let mut metric = TEXTMETRICW::default();
    // SAFETY: metric is writable output storage for the live window DC.
    let measured = unsafe { GetTextMetricsW(dc.raw(), &mut metric) }.as_bool();
    if saved.restore().is_err() {
        font.retain();
        return fallback;
    }
    if measured {
        metric.tmHeight.max(1)
    } else {
        fallback
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
    // SAFETY: This is an owned string COMBOBOX (not owner-drawn without strings). It
    // copies the terminated UTF-16 allocation synchronously before this call returns.
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
    // SAFETY: The helper uses an owned control and a documented scalar-only message;
    // synchronous processing retains no Rust pointer or mutable state borrow.
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
    // SAFETY: The helper uses an owned control and a documented scalar-only message;
    // synchronous processing retains no Rust pointer or mutable state borrow.
    let result = unsafe { SendMessageW(hwnd, CB_GETCURSEL, None, None) }.0;
    usize::try_from(result)
        .map_err(|_| Error::new(E_FAIL, tr("请选择有效的选项", "Choose a valid option")))
}

fn set_checked(hwnd: HWND, value: bool) {
    // SAFETY: This private helper is called with owned checkbox BUTTON controls; the
    // message contains only a scalar check state and retains no Rust references.
    unsafe {
        SendMessageW(hwnd, BM_SETCHECK, Some(WPARAM(usize::from(value))), None);
    }
}

fn checked(hwnd: HWND) -> bool {
    // SAFETY: The helper uses an owned control and a documented scalar-only message;
    // synchronous processing retains no Rust pointer or mutable state borrow.
    unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None) }.0 == 1
}

fn set_slider(hwnd: HWND, maximum: u8, value: u8, page_size: u8) {
    // SAFETY: This private helper receives owned TRACKBAR controls; all four messages
    // carry scalar range/position values and retain no Rust pointers.
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
    // SAFETY: The helper uses an owned control and a documented scalar-only message;
    // synchronous processing retains no Rust pointer or mutable state borrow.
    let value = unsafe { SendMessageW(hwnd, TBM_GETPOS, None, None) }.0;
    u8::try_from(value).map_err(|_| {
        Error::new(
            E_FAIL,
            tr("请选择有效的取色外观值", "Choose a valid appearance value"),
        )
    })
}

/// # Safety
/// Invoked by the installed child subclass on its creating UI thread. reference
/// must be this root's stable CallbackState pointer, retained through child
/// WM_NCDESTROY; message parameters obey the native subclass contract.
unsafe extern "system" fn key_button_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    // SAFETY: This registered native callback supplies the dispatcher's valid HWND and
    // message payload; the panic boundary prevents unwind across the system ABI.
    catch_unwind(AssertUnwindSafe(|| unsafe {
        key_button_dispatch(hwnd, message, wparam, lparam, subclass, reference)
    }))
    .unwrap_or_else(|_| std::process::abort())
}

/// # Safety
/// Invoked by the installed child subclass on its creating UI thread. reference
/// must be this root's stable CallbackState pointer, retained through child
/// WM_NCDESTROY; message parameters obey the native subclass contract.
unsafe fn key_button_dispatch(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    // The owner destroys all child windows before dropping this stable Box.
    // SAFETY: The installed subclass reference points to this root's stable callback
    // Box. Root teardown keeps it alive until every child/subclass callback returns.
    let state = unsafe { &*(reference as *const CallbackState) };
    let key = wparam.0 as u32;
    match message {
        WM_NCDESTROY => {
            // SAFETY: This removes the exact subclass identity installed on this child. The
            // root lifetime keeps callback state alive until all child teardown finishes.
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(key_button_proc), subclass) };
        }
        WM_GETDLGCODE => {
            // SAFETY: The original subclass message and any native payload remain valid until
            // this callback returns; all temporary mutable state borrows have ended.
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
    // SAFETY: The original subclass message and any native payload remain valid until
    // this callback returns; all temporary mutable state borrows have ended.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

/// # Safety
/// Invoked for this registered class with native message payloads valid for the
/// callback duration. WM_NCCREATE must carry our synchronous WindowInit pointing
/// to stable CallbackState; later userdata remains owned until root termination.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: This registered native callback supplies the dispatcher's valid HWND and
    // message payload; the panic boundary prevents unwind across the system ABI.
    catch_unwind(AssertUnwindSafe(|| unsafe {
        dispatch(hwnd, message, wparam, lparam)
    }))
    .unwrap_or_else(|_| std::process::abort())
}

/// # Safety
/// Invoked for this registered class with native message payloads valid for the
/// callback duration. WM_NCCREATE must carry our synchronous WindowInit pointing
/// to stable CallbackState; later userdata remains owned until root termination.
unsafe fn dispatch(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: native WM_NCCREATE carries our synchronous WindowInit;
        // only the stable callback pointer is installed, never the stack init.
        return unsafe {
            messages::with_window_init(lparam, |init| {
                let state = &*(init.state as *const CallbackState);
                if state.lifetime.attach(hwnd, init) {
                    // Preserve native nonclient initialization, including the
                    // accessible window title copied from CREATESTRUCTW.
                    DefWindowProcW(hwnd, message, wparam, lparam)
                } else {
                    LRESULT(0)
                }
            })
        }
        .unwrap_or(LRESULT(0));
    }
    // SAFETY: Read only userdata installed by this class's WM_NCCREATE on the UI thread;
    // root lifetime tracking retains its stable callback allocation until teardown.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const CallbackState;
    // SAFETY: A nonnull value is this class's installed stable Box pointer, not an
    // arbitrary address; root termination completes before its owner can free it.
    let Some(state) = (unsafe { pointer.as_ref() }) else {
        // SAFETY: Forward the unchanged native message while its payload remains valid for
        // this callback; the default procedure does not gain Rust-state ownership.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_PAINT if hwnd == state.content.get() => {
            // Snapshot the completed layout before GDI calls; paint never measures
            // text or changes geometry while the window tree is being scrolled.
            let panels = state.content_panels.borrow().clone();
            paint_content(hwnd, &panels)
        }
        WM_PAINT => state.theme.paint(hwnd, &[]),
        WM_ERASEBKGND => LRESULT(1),
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX => {
            // SAFETY: This message supplies a child-control HWND (not a payload pointer); USER32
            // queries its numeric ID without taking ownership or retaining Rust references.
            let id = unsafe { GetDlgCtrlID(HWND(lparam.0 as *mut _)) } as usize;
            let (panel, tone) = match id {
                TITLE => (false, Tone::Text),
                SUBTITLE => (false, Tone::Muted),
                LANGUAGE_LABEL | LANGUAGE => (true, Tone::Text),
                STATUS | STATUS_DETAILS => (false, state.status_tone.get()),
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
        WM_NOTIFY => {
            // SAFETY: native notification is validated before typed custom drawing.
            unsafe {
                messages::with_button_custom_draw(hwnd, lparam, |draw| {
                    theme::custom_draw(&state.theme, draw, APPLY)
                })
            }
            // SAFETY: Forward the unchanged native message while its payload remains valid for
            // this callback; the default procedure does not gain Rust-state ownership.
            .flatten()
            .unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, message, wparam, lparam) })
        }
        WM_DRAWITEM if wparam.0 == APPEARANCE_PREVIEW => {
            // SAFETY: native owner draw is decoded once; the preview borrows its DC only here.
            unsafe {
                messages::with_static_draw(hwnd, lparam, APPEARANCE_PREVIEW, |draw| {
                    if let Some(preview) = state.preview.borrow().as_ref()
                        && let Err(error) = preview.paint(draw.hDC, draw.rcItem)
                    {
                        crate::app::diagnostics::event(format_args!(
                            "settings.preview_paint_failed {error}"
                        ));
                    }
                    LRESULT(1)
                })
            }
            .unwrap_or_else(|| {
                // SAFETY: The original owner-draw payload remains valid for
                // default handling throughout this callback.
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            })
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
            // SAFETY: This message supplies a child-control HWND (not a payload pointer); USER32
            // queries its numeric ID without taking ownership or retaining Rust references.
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
            // SAFETY: The callback retains its viewport; initialized SCROLLINFO advertises the
            // actual layout size and is writable only for this synchronous query.
            if unsafe { GetScrollInfo(hwnd, SB_VERT, &mut info) }.is_ok() {
                // SAFETY: The current callback or window owner retains this HWND through the scalar
                // DPI query; no native handle ownership is transferred.
                let line = dip(28, unsafe { GetDpiForWindow(hwnd) }.max(96));
                let current = state.scroll_offset.get();
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
                if let Err(error) = state.scroll_to(next) {
                    crate::app::diagnostics::event(format_args!("settings.scroll_failed {error}"));
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as u16 as i16) as i32;
            let total = state.wheel_remainder.get() + delta;
            state.wheel_remainder.set(total % 120);
            let steps = total / 120;
            if steps != 0 {
                let current = state.scroll_offset.get();
                // SAFETY: The current callback or window owner retains this HWND through the scalar
                // DPI query; no native handle ownership is transferred.
                let next = current - steps * dip(72, unsafe { GetDpiForWindow(hwnd) }.max(96));
                if let Err(error) = state.scroll_to(next) {
                    crate::app::diagnostics::event(format_args!("settings.scroll_failed {error}"));
                }
            }
            LRESULT(0)
        }
        WM_ACTIVATE if wparam.0 as u32 & 0xffff == WA_INACTIVE => {
            state.cancel_capture();
            // SAFETY: Forward the unchanged native message while its payload remains valid for
            // this callback; the default procedure does not gain Rust-state ownership.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_DPICHANGED if hwnd == state.root.get() => {
            // SAFETY: dispatch is handling native WM_DPICHANGED.
            if let Some(rect) = unsafe { messages::dpi_rect(lparam) } {
                state.queue(|pending| {
                    pending.dpi_rect = Some(rect);
                    pending.layout = true;
                });
            }
            LRESULT(0)
        }
        WM_SIZE if hwnd == state.root.get() && wparam.0 != SIZE_MINIMIZED as usize => {
            state.queue(|pending| pending.layout = true);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE if hwnd == state.root.get() => {
            state.queue(|pending| pending.fit_work_area = true);
            LRESULT(0)
        }
        DM_GETDEFID => LRESULT(((DC_HASDEFID as usize) << 16 | state.default_id.get()) as isize),
        DM_SETDEFID => {
            state.default_id.set(wparam.0);
            state.queue(|pending| pending.default_style = true);
            LRESULT(1)
        }
        WM_NCDESTROY => {
            // SAFETY: the native tree still borrows state through this callback.
            let result = unsafe {
                let result = DefWindowProcW(hwnd, message, wparam, lparam);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                result
            };
            state.lifetime.terminated(hwnd);
            result
        }
        // SAFETY: Forward the unchanged native message while its payload remains valid for
        // this callback; the default procedure does not gain Rust-state ownership.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod responsive_layout_tests {
    use super::*;

    #[test]
    fn minimized_settings_defer_broadcast_geometry_but_keep_apply_and_restore_layout() {
        use super::super::window_lifetime::{restore_hidden_window, with_hidden_minimized_windows};
        with_hidden_minimized_windows(|| {
            let config = Config::default();
            let window = SettingsWindow::new(&config, HWND::default(), None, true).unwrap();
            let icon = RECT {
                left: -32000,
                top: -32000,
                right: -31724,
                bottom: -31955,
            };
            let mut client = RECT::default();
            // SAFETY: these scalar broadcasts and synchronous DPI payload target
            // only this retained hidden fixture. No system settings are changed.
            unsafe {
                assert!(IsIconic(window.hwnd).as_bool());
                assert!(!IsWindowVisible(window.hwnd).as_bool());
                GetClientRect(window.hwnd, &mut client).unwrap();
                SendMessageW(window.hwnd, WM_SETTINGCHANGE, None, None);
                SendMessageW(window.hwnd, WM_DISPLAYCHANGE, None, None);
                SendMessageW(
                    window.hwnd,
                    WM_DPICHANGED,
                    None,
                    Some(LPARAM(&icon as *const RECT as isize)),
                );
                SendMessageW(window.hwnd, WM_COMMAND, Some(WPARAM(APPLY)), None);
            }
            assert_eq!((client.right, client.bottom), (0, 0));
            assert_eq!(
                window.process_pending().unwrap(),
                Some(SettingsAction::Apply(config))
            );
            assert!(window.callback.pending.get().fit_work_area);
            assert!(window.callback.pending.get().dpi_rect.is_none());
            window.show_status("saved while minimized", true).unwrap();
            // Repeated host passes must neither error nor consume deferred fitting.
            assert_eq!(window.process_pending().unwrap(), None);
            assert!(window.callback.pending.get().fit_work_area);
            // Localization refresh also reaches layout outside process_pending.
            window.layout().unwrap();
            assert!(window.callback.pending.get().layout);

            let dpi = window.dpi().unwrap();
            let request = RECT {
                left: 0,
                top: 0,
                right: dip(CLIENT_WIDTH, dpi),
                bottom: dip(CLIENT_HEIGHT, dpi),
            };
            let work = layout::work_area(request).unwrap();
            // Begin partly beyond the work area so deferred fitting is observable.
            restore_hidden_window(
                window.hwnd,
                RECT {
                    left: work.right - dip(120, dpi),
                    top: work.top,
                    right: work.right + dip(CLIENT_WIDTH - 120, dpi),
                    bottom: work.top + dip(CLIENT_HEIGHT, dpi),
                },
            );
            window.process_pending().unwrap();
            window.process_pending().unwrap();
            let mut restored = RECT::default();
            // SAFETY: the fixture still owns this hidden root and output storage.
            unsafe {
                GetWindowRect(window.hwnd, &mut restored).unwrap();
                GetClientRect(window.hwnd, &mut client).unwrap();
                assert!(!IsWindowVisible(window.hwnd).as_bool());
            }
            assert!(client.right > 0 && client.bottom > 0);
            let work = layout::work_area(restored).unwrap();
            assert!(restored.left >= work.left && restored.right <= work.right);
            assert!(restored.top >= work.top && restored.bottom <= work.bottom);
            assert_eq!(window.font.borrow().dpi, window.dpi().unwrap());
            assert!(!window.callback.pending.get().fit_work_area);
            assert!(!window.callback.pending.get().layout);
        });
    }

    #[test]
    fn custom_draw_restore_failure_retains_replaced_and_final_settings_fonts() {
        use super::super::{theme::restore_tests, window_lifetime::with_hidden_windows};
        with_hidden_windows(|| {
            for fail_restore in [false, true] {
                let window =
                    SettingsWindow::new(&Config::default(), HWND::default(), None, true).unwrap();
                let handles = || {
                    let mut fonts = window
                        .font
                        .borrow()
                        ._fonts
                        .iter()
                        .map(Font::raw)
                        .collect::<Vec<_>>();
                    fonts.push(window.language_font.borrow().as_ref().unwrap().1.raw());
                    fonts
                };
                let previous = handles();
                restore_tests::begin_font_delete_tracking();
                restore_tests::draw_button(
                    window.hwnd,
                    window.controls.apply,
                    APPLY,
                    &window.callback.theme,
                    fail_restore,
                );
                window.font.borrow_mut().dpi = 0;
                window.language_font.borrow_mut().as_mut().unwrap().0 = 0;
                window.layout().unwrap();
                restore_tests::assert_font_delete_attempts(&previous, !fail_restore);
                let current = handles();
                restore_tests::begin_font_delete_tracking();
                drop(window);
                restore_tests::assert_font_delete_attempts(&current, !fail_restore);
                if fail_restore {
                    restore_tests::cleanup_retained_fonts(&previous);
                    restore_tests::cleanup_retained_fonts(&current);
                }
            }
        });
    }

    #[test]
    fn native_settings_partial_creation_failures_end_every_root() {
        for nth in [1, 2, 3, 8, 15] {
            super::super::window_lifetime::creation_failure_test(nth, || {
                assert!(
                    SettingsWindow::new(&Config::default(), HWND::default(), None, true).is_err()
                );
            });
        }
    }

    #[test]
    fn settings_content_reflows_without_horizontal_clipping_or_hidden_focus() {
        // This is a deterministic geometry check; the following test separately
        // measures actual native fonts for both languages and every supported DPI.
        for width in [260, 280, 300, 364, 430, 459, 460, 480, 500] {
            let mut measure = |_: usize, width: i32, _: bool| {
                Ok(TextExtent {
                    width: width.min(84),
                    height: 20,
                })
            };
            let geometry = settings_content_layout(
                width,
                FieldMetrics {
                    language_height: 28,
                    format_height: 28,
                },
                &mut measure,
            )
            .unwrap();
            assert_geometry_fits(&geometry, width);
        }
    }

    #[test]
    fn settings_text_fits_native_fonts_at_each_supported_dpi_and_language() {
        use super::super::window_lifetime::with_hidden_windows;
        use crate::app::i18n::set_language;
        let previous_language = language();
        with_hidden_windows(|| {
            for language in [Language::SimplifiedChinese, Language::English] {
                set_language(language);
                for dpi in [96, 120, 144, 168, 192, 240, 288] {
                    let window =
                        SettingsWindow::new(&Config::default(), HWND::default(), None, true)
                            .unwrap();
                    let body = Font::for_language(14, dpi, 400, false, language).unwrap();
                    let title = Font::for_language(20, dpi, 600, false, language).unwrap();
                    let heading = Font::for_language(14, dpi, 600, false, language).unwrap();
                    let small = Font::for_language(12, dpi, 400, false, language).unwrap();
                    let selector =
                        Font::for_language(14, dpi, 400, false, Language::SimplifiedChinese)
                            .unwrap();
                    for hwnd in window.controls.handles() {
                        // SAFETY: The owned control is queried synchronously for its scalar ID.
                        let id = unsafe { GetDlgCtrlID(hwnd) } as usize;
                        let font = match id {
                            TITLE => &title,
                            10 | COPY_HEADING | APPEARANCE_HEADING => &heading,
                            SUBTITLE | 12 | USAGE_HEADING | USAGE_HINT | STATUS
                            | STATUS_DETAILS | PREVIEW_HEADING | PREVIEW_HINT => &small,
                            LANGUAGE => &selector,
                            _ => &body,
                        };
                        // SAFETY: Each local font owner lives until the complete hidden
                        // window tree is destroyed at the end of this DPI case.
                        unsafe {
                            SendMessageW(
                                hwnd,
                                WM_SETFONT,
                                Some(WPARAM(font.raw().0 as usize)),
                                Some(LPARAM(0)),
                            );
                        }
                    }
                    // This case measures actual rasterized fonts at the requested
                    // DPI; it does not change the hidden window's monitor DPI.
                    // Supply font-sized field slots instead of treating native
                    // chrome from that monitor as chrome at the requested DPI.
                    // The interactive integration test checks real collapsed
                    // COMBOBOX heights and alignment at the monitor's actual DPI.
                    let fields = FieldMetrics {
                        language_height: window
                            .measure_text(LANGUAGE, 180, false, dpi)
                            .unwrap()
                            .height
                            + 6,
                        format_height: window.measure_text(FORMAT, 180, false, dpi).unwrap().height
                            + 6,
                    };
                    for hint in [
                        CaptureHint::Ready,
                        CaptureHint::Listening,
                        CaptureHint::Invalid,
                        CaptureHint::Accepted,
                        CaptureHint::Cancelled,
                    ] {
                        window.callback.capture_hint.set(hint);
                        window.update_capture_text().unwrap();
                        for width in [260, 280, 300, 364, 430, 459, 460, 480, 500] {
                            let mut measure =
                                |id, width, wrap| window.measure_text(id, width, wrap, dpi);
                            let geometry =
                                settings_content_layout(width, fields, &mut measure).unwrap();
                            assert_geometry_fits(&geometry, width);
                            for &(id, r) in &geometry.controls {
                                if matches!(
                                    id,
                                    APPEARANCE_PREVIEW | BORDER_WIDTH | BACKGROUND_TRANSPARENCY
                                ) {
                                    continue;
                                }
                                let check =
                                    matches!(id, CTRL | ALT | SHIFT | QUICK_PICK | AUTO_COPY);
                                let available = r.right - r.left - if check { 24 } else { 0 };
                                let wrap = !matches!(
                                    id,
                                    KEY | LANGUAGE | FORMAT | BORDER_VALUE | TRANSPARENCY_VALUE
                                );
                                let text = window.measure_text(id, available, wrap, dpi).unwrap();
                                assert!(
                                    text.height <= r.bottom - r.top,
                                    "{language:?} at {dpi} DPI, width {width}, control {id}: {} > {}",
                                    text.height,
                                    r.bottom - r.top,
                                );
                                if !wrap {
                                    assert!(
                                        text.width <= available,
                                        "{language:?} at {dpi} DPI, width {width}, control {id}: {} > {available}",
                                        text.width,
                                    );
                                }
                            }
                        }
                    }
                    // The locally assigned fonts must outlive the control tree, even
                    // though this offscreen test intentionally differs from monitor DPI.
                    drop(window);
                }
            }
        });
        set_language(previous_language);
    }

    fn assert_geometry_fits(geometry: &SettingsContentLayout, width: i32) {
        assert_eq!(geometry.controls.len(), 28);
        assert_eq!(geometry.panels.len(), 4);
        for &(id, r) in &geometry.controls {
            assert!(
                r.left >= 0 && r.right <= width && r.right > r.left,
                "{id} at {width}: {r:?}",
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
                    "{id} at {width}: {r:?}",
                );
            }
        }
        for (index, &(_, first)) in geometry.controls.iter().enumerate() {
            for &(other_id, second) in &geometry.controls[index + 1..] {
                let intersect = first.left < second.right
                    && second.left < first.right
                    && first.top < second.bottom
                    && second.top < first.bottom;
                assert!(
                    !intersect,
                    "overlapping control {other_id} at {width}: {first:?}, {second:?}"
                );
            }
        }
        for r in &geometry.panels {
            assert!(r.left >= 0 && r.right <= width && r.bottom <= geometry.height);
            assert_eq!(r.left, width - r.right, "panel margins must be symmetric");
        }
    }
}
