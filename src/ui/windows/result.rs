//! Native result controls. Window callbacks only queue intentions; the owner
//! calls `process_pending` after dispatch and owns the window until it is dropped.

use super::messages;
use super::window_lifetime::{WindowInit, WindowLifetime, WindowRole};
use crate::platform::windows::gdi::{OwnedPen, PaintSession, SavedDc};

use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
};

use windows::{
    Win32::{
        Foundation::{
            COLORREF, E_FAIL, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT,
            POINT, RECT, WPARAM,
        },
        Graphics::{
            Dwm::{
                DWM_WINDOW_CORNER_PREFERENCE, DWMNCRENDERINGPOLICY, DWMNCRP_DISABLED,
                DWMWA_BORDER_COLOR, DWMWA_NCRENDERING_POLICY, DWMWA_TRANSITIONS_FORCEDISABLED,
                DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute,
            },
            Gdi::*,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            SystemServices::{SS_NOPREFIX, SS_OWNERDRAW},
        },
        UI::{
            Controls::{
                CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED,
                CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, NMCUSTOMDRAW, SetScrollInfo,
            },
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{GetFocus, SetFocus},
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, Error, PCWSTR, Result, w},
};

use super::{
    drawing::{border_thickness, dip, draw_bottom_right_border},
    layout::{self, SheetLayout, fit_to_work_area, reveal_offset},
    theme::{self, Font, Theme, Tone},
};
use crate::{
    app::config::AppearanceConfig,
    app::i18n::tr,
    core::{
        format::{ColorFormat, format_color},
        state::{PickedColor, SampleKind},
    },
    platform::windows::clipboard::{Clipboard, ClipboardError},
};

pub const WM_RESULT_WAKE: u32 = WM_APP + 11;
const CLASS: PCWSTR = w!("ColorPicker.Result.v1");
const CONTENT_CLASS: PCWSTR = w!("ColorPicker.ResultContent.v1");
const CONTENT: usize = 300;
const COPY_DEFAULT: usize = 1;
const CLOSE: usize = 2;
const PICK_AGAIN: usize = 200;
const COPY_ROW: usize = 100;
const CAPTION_MINIMIZE: usize = 40;
const CAPTION_CLOSE: usize = 41;
const RETRY_MS: u32 = 50;
const COPIED_FEEDBACK_MS: u32 = 1600;
const MAX_RETRIES: u8 = 3;
const CLIENT_WIDTH: i32 = 420;
const CLIENT_HEIGHT: i32 = 364;
const CLIENT_HEIGHT_WITH_STATUS: i32 = 390;
const SWATCH_HEIGHT: i32 = 112;
const STYLE: WINDOW_STYLE =
    WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);
const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_APPWINDOW.0 | WS_EX_CONTROLPARENT.0);
static NEXT_COPY_TOKEN: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultAction {
    Close,
    PickAgain,
}

#[derive(Clone, Copy, Default)]
struct Pending {
    action: Option<ResultAction>,
    copy: Option<CopyTarget>,
    timer: Option<usize>,
    feedback_timer: Option<usize>,
    dpi_rect: Option<RECT>,
    layout: bool,
    fit_work_area: bool,
    scroll: Option<i32>,
    reveal: Option<HWND>,
    default_style: bool,
    minimize: bool,
}

struct CallbackState {
    lifetime: WindowLifetime,
    notify_hwnd: HWND,
    default_format: ColorFormat,
    theme: Theme,
    swatch_color: COLORREF,
    border_width_dip: u8,
    status_tone: Cell<Tone>,
    pending: Cell<Pending>,
    wake_posted: Cell<bool>,
    wake_failed: Cell<bool>,
    closing: Cell<bool>,
    active_timer: Cell<usize>,
    feedback_timer: Cell<usize>,
    default_id: Cell<usize>,
    viewport: Cell<HWND>,
    scroll_offset: Cell<i32>,
    content_height: Cell<i32>,
    viewport_height: Cell<i32>,
    header_height: Cell<i32>,
    wheel_remainder: Cell<i32>,
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
            // SAFETY: The host outlives this window's callback tree; this private wake message
            // carries scalar values only and retains no Rust reference.
            && unsafe { PostMessageW(Some(self.notify_hwnd), WM_RESULT_WAKE, WPARAM(0), LPARAM(0)) }
                .is_err()
        {
            self.wake_posted.set(false);
            self.wake_failed.set(true);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Row {
    label: HWND,
    edit: HWND,
    copy: HWND,
}

#[derive(Default)]
struct Controls {
    swatch: HWND,
    minimize: HWND,
    caption_close: HWND,
    source: HWND,
    rows: [Row; 4],
    default_copy: HWND,
    pick_again: HWND,
    close: HWND,
    status: HWND,
}

impl Controls {
    fn handles(&self) -> impl Iterator<Item = HWND> + '_ {
        [
            self.swatch,
            self.minimize,
            self.caption_close,
            self.source,
            self.default_copy,
            self.pick_again,
            self.close,
            self.status,
        ]
        .into_iter()
        .chain(
            self.rows
                .iter()
                .flat_map(|row| [row.label, row.edit, row.copy]),
        )
    }
}

#[derive(Default)]
struct Resources {
    dpi: u32,
    _font: Option<Font>,
    _small_font: Option<Font>,
    _value_font: Option<Font>,
}

impl Resources {
    fn retain_fonts(&self) {
        for font in [&self._font, &self._small_font, &self._value_font]
            .into_iter()
            .flatten()
        {
            font.retain();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CopyTarget {
    format: ColorFormat,
    button_id: usize,
}

#[derive(Clone, Copy)]
struct CopyRequest {
    token: usize,
    target: CopyTarget,
    retries_left: u8,
}

#[derive(Default)]
struct CopyState(Option<CopyRequest>);

impl CopyState {
    fn replace(&mut self, token: usize, target: CopyTarget) {
        self.0 = Some(CopyRequest {
            token,
            target,
            retries_left: MAX_RETRIES,
        });
    }

    fn matching(&self, token: usize) -> Option<CopyRequest> {
        self.0.filter(|request| request.token == token)
    }

    fn reserve_retry(&mut self, token: usize) -> bool {
        let Some(request) = self.0.as_mut().filter(|request| request.token == token) else {
            return false;
        };
        if request.retries_left == 0 {
            self.0 = None;
            return false;
        }
        request.retries_left -= 1;
        true
    }
}

pub struct ResultWindow {
    hwnd: HWND,
    callback: Box<CallbackState>,
    controls: Controls,
    picked: PickedColor,
    resources: RefCell<Resources>,
    copy: RefCell<CopyState>,
    copied_target: Cell<Option<CopyTarget>>,
    status_visible: Cell<bool>,
    quick_pick_pending: Cell<bool>,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl ResultWindow {
    pub fn new(picked: PickedColor, notify_hwnd: HWND) -> Result<Self> {
        Self::new_with_options(picked, notify_hwnd, ColorFormat::Hex, false)
    }

    pub fn new_with_options(
        picked: PickedColor,
        notify_hwnd: HWND,
        default_format: ColorFormat,
        auto_copy: bool,
    ) -> Result<Self> {
        Self::new_with_appearance(
            picked,
            notify_hwnd,
            default_format,
            auto_copy,
            AppearanceConfig::default(),
        )
    }

    pub fn new_with_appearance(
        picked: PickedColor,
        notify_hwnd: HWND,
        default_format: ColorFormat,
        auto_copy: bool,
        appearance: AppearanceConfig,
    ) -> Result<Self> {
        Self::new_with_behavior(
            picked,
            notify_hwnd,
            default_format,
            auto_copy,
            appearance,
            false,
        )
    }

    /// Quick picks copy after input cleanup without showing or activating this
    /// window. A final copy failure reveals it so the color can still be used.
    pub fn new_with_behavior(
        picked: PickedColor,
        notify_hwnd: HWND,
        default_format: ColorFormat,
        auto_copy: bool,
        appearance: AppearanceConfig,
        quick_pick: bool,
    ) -> Result<Self> {
        // SAFETY: None borrows the process module containing the static callback; the module
        // lives for the process and is never freed by this window owner.
        let instance = unsafe { GetModuleHandleW(None)? }.into();
        for name in [CLASS, CONTENT_CLASS] {
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                // SAFETY: IDC_ARROW is a documented system cursor identifier. The returned shared
                // cursor is borrowed by the class and is never destroyed as an owned resource.
                hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
                lpszClassName: name,
                ..Default::default()
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
            notify_hwnd,
            default_format,
            theme: Theme::new()?,
            swatch_color: COLORREF(
                u32::from(picked.rgb.r)
                    | (u32::from(picked.rgb.g) << 8)
                    | (u32::from(picked.rgb.b) << 16),
            ),
            border_width_dip: appearance.border_width_dip,
            status_tone: Cell::new(Tone::Muted),
            pending: Cell::new(Pending::default()),
            wake_posted: Cell::new(false),
            wake_failed: Cell::new(false),
            closing: Cell::new(false),
            active_timer: Cell::new(0),
            feedback_timer: Cell::new(0),
            default_id: Cell::new(COPY_DEFAULT),
            viewport: Cell::new(HWND::default()),
            scroll_offset: Cell::new(0),
            content_height: Cell::new(0),
            viewport_height: Cell::new(0),
            header_height: Cell::new(0),
            wheel_remainder: Cell::new(0),
        });
        let pointer = callback.as_ref() as *const CallbackState;
        let init = WindowInit {
            state: pointer.cast(),
            role: WindowRole::Root,
        };
        let title = wide(tr("取色结果 — Color Picker", "Picked color — Color Picker"));
        // SAFETY: The stack WindowInit and terminated title live through synchronous creation.
        // WM_NCCREATE retains only the stable Box pointer; creation_result checks teardown.
        let hwnd = unsafe {
            super::window_lifetime::create_window(|| {
                CreateWindowExW(
                    EX_STYLE,
                    CLASS,
                    PCWSTR(title.as_ptr()),
                    STYLE,
                    picked.source.x,
                    picked.source.y,
                    1,
                    1,
                    Some(notify_hwnd),
                    None,
                    Some(instance),
                    Some((&init as *const WindowInit).cast()),
                )
            })
        };
        let hwnd = callback.lifetime.creation_result("result", hwnd)?;
        // From this point every error destroys all partially created children
        // before releasing the userdata allocation or selected GDI resources.
        let mut window = Self {
            hwnd,
            callback,
            controls: Controls::default(),
            picked,
            resources: RefCell::new(Resources::default()),
            copy: RefCell::new(CopyState::default()),
            copied_target: Cell::new(None),
            status_visible: Cell::new(false),
            quick_pick_pending: Cell::new(quick_pick),
            _thread_affinity: PhantomData,
        };
        window.callback.theme.update_window_icons(hwnd);
        // Keep the window's native title for taskbar/accessibility, while the
        // client area replaces the visible frame. Disable DWM's nonclient
        // rendering (including its shadow) on both Windows 10 and Windows 11;
        // only our bottom/right border should surround this flat sheet.
        let nonclient_policy = DWMNCRP_DISABLED;
        // SAFETY: The root owner is already established. The local attribute value has the
        // matching native type/size and remains valid through this synchronous copy.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_NCRENDERING_POLICY,
                (&nonclient_policy as *const DWMNCRENDERINGPOLICY).cast(),
                size_of::<DWMNCRENDERINGPOLICY>() as u32,
            )?;
        }
        // Windows 11 supports explicit square corners. Windows 10 already has
        // square corners and safely ignores these optional appearance hints.
        let corners = DWMWCP_DONOTROUND;
        // SAFETY: The root owner is already established. The local attribute value has the
        // matching native type/size and remains valid through this synchronous copy.
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&corners as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
                size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            )
        };
        let no_border = 0xffff_fffe_u32; // DWMWA_COLOR_NONE
        // SAFETY: The root owner is already established. The local attribute value has the
        // matching native type/size and remains valid through this synchronous copy.
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                (&no_border as *const u32).cast(),
                size_of::<u32>() as u32,
            )
        };
        // Pick Again can start sampling immediately after this owner is
        // dropped. Do not leave animated result-window pixels in composition.
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
        let init = WindowInit {
            state: pointer.cast(),
            role: WindowRole::Content,
        };
        // SAFETY: The root owner already retains the stable callback Box. This synchronous
        // child creation reads stack WindowInit only now and stores its state pointer.
        let viewport = unsafe {
            super::window_lifetime::create_window(|| {
                CreateWindowExW(
                    WS_EX_CONTROLPARENT,
                    CONTENT_CLASS,
                    w!("Color formats"),
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
        window.create_controls()?;
        window.place_initially()?;
        window.layout()?;
        if !quick_pick {
            window.show_result();
        }
        if auto_copy || quick_pick {
            // The host constructs this window only after capture/input cleanup.
            // Clipboard access stays in process_pending, just like button copies.
            window.callback.queue(|pending| {
                pending.copy = Some(CopyTarget {
                    format: default_format,
                    button_id: COPY_DEFAULT,
                })
            });
        }
        Ok(window)
    }

    /// A failed host copy opens an ordinary manual result. No copy is queued;
    /// a later successful button copy must leave this visible window open.
    pub fn new_after_copy_failure(
        picked: PickedColor,
        notify_hwnd: HWND,
        default_format: ColorFormat,
        appearance: AppearanceConfig,
        failure: &str,
    ) -> Result<Self> {
        let window =
            Self::new_with_appearance(picked, notify_hwnd, default_format, false, appearance)?;
        window.copy_failed(failure)?;
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn show_result(&self) {
        if !super::window_lifetime::show_native_windows() {
            return;
        }
        // SAFETY: The window/control belongs to the live UI-thread owner. These synchronous
        // calls occur without a held callback-state mutable borrow.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.controls.default_copy));
        }
    }

    /// Called on the creating UI thread, after the host receives WM_RESULT_WAKE.
    /// No RefCell borrow survives an API call that may synchronously reenter us.
    pub fn process_pending(&self) -> Result<Option<ResultAction>> {
        self.callback.wake_posted.set(false);
        if self.callback.wake_failed.replace(false) {
            return Err(Error::new(E_FAIL, "Could not queue result-window work"));
        }
        let pending = self.callback.pending.take();
        if let Some(action) = pending.action {
            self.callback.closing.set(true);
            self.cancel_copy();
            self.clear_copy_feedback()?;
            return Ok(Some(action));
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
            self.fit_window()?;
        }
        if let Some(offset) = pending.scroll {
            self.callback.scroll_offset.set(offset);
        }
        if pending.layout || pending.fit_work_area || pending.scroll.is_some() {
            self.layout()?;
            if pending.scroll.is_none() {
                // SAFETY: This queries a borrowed HWND from the current UI thread; reveal_control
                // checks membership in the owned viewport before using its geometry.
                self.reveal_control(unsafe { GetFocus() })?;
            }
        }
        if let Some(control) = pending.reveal {
            self.reveal_control(control)?;
        }
        if pending.default_style {
            self.update_default_style();
        }
        if pending.minimize {
            // SAFETY: The window/control belongs to the live UI-thread owner. These synchronous
            // calls occur without a held callback-state mutable borrow.
            let _ = unsafe { ShowWindow(self.hwnd, SW_MINIMIZE) };
        }
        if let Some(target) = pending.copy {
            self.cancel_copy();
            self.clear_copy_feedback()?;
            let token = next_copy_token()?;
            self.copy.borrow_mut().replace(token, target);
            self.attempt_copy(token)?;
        } else if let Some(token) = pending.timer
            && self.callback.active_timer.get() == token
        {
            self.stop_timer();
            let request = self.copy.borrow().0;
            if let Some(request) = request {
                self.attempt_copy(request.token)?;
            }
        }
        if let Some(timer) = pending.feedback_timer
            && self.callback.feedback_timer.get() == timer
        {
            self.clear_copy_feedback()?;
        }
        Ok(None)
    }

    fn create_controls(&mut self) -> Result<()> {
        self.controls.swatch = self.control(
            w!("STATIC"),
            tr("已选颜色色块", "Selected color swatch"),
            10,
            WINDOW_STYLE(SS_OWNERDRAW.0) | WS_CLIPSIBLINGS,
            WINDOW_EX_STYLE::default(),
        )?;
        let source = match self.picked.kind {
            SampleKind::Live => tr("实时屏幕", "Live screen"),
            SampleKind::Frozen => tr("冻结画面", "Frozen screen"),
        };
        self.controls.source = self.control(
            w!("STATIC"),
            &format!(
                "{source}  ·  X {}  Y {}",
                self.picked.source.x, self.picked.source.y
            ),
            11,
            WINDOW_STYLE(SS_NOPREFIX.0),
            WINDOW_EX_STYLE::default(),
        )?;
        for (index, format) in ColorFormat::ALL.into_iter().enumerate() {
            self.controls.rows[index] = Row {
                label: self.control(
                    w!("STATIC"),
                    format.label(),
                    20 + index,
                    WINDOW_STYLE(SS_NOPREFIX.0),
                    WINDOW_EX_STYLE::default(),
                )?,
                edit: self.control(
                    w!("EDIT"),
                    &format_color(self.picked.rgb, format),
                    30 + index,
                    WS_TABSTOP | WINDOW_STYLE((ES_READONLY | ES_AUTOHSCROLL | ES_NOHIDESEL) as u32),
                    WINDOW_EX_STYLE::default(),
                )?,
                copy: self.control(
                    w!("BUTTON"),
                    &crate::tr_format!("复制 {}", "Copy {}", format.label()),
                    COPY_ROW + index,
                    WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                    WINDOW_EX_STYLE::default(),
                )?,
            };
        }
        self.controls.default_copy = self.control(
            w!("BUTTON"),
            &crate::tr_format!("复制 {}", "Copy {}", self.callback.default_format.label()),
            COPY_DEFAULT,
            WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.pick_again = self.control(
            w!("BUTTON"),
            tr("重新取色", "Pick again"),
            PICK_AGAIN,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.close = self.control(
            w!("BUTTON"),
            tr("关闭", "Close"),
            CLOSE,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.status = self.control(
            w!("EDIT"),
            "",
            12,
            WS_TABSTOP
                | WS_VSCROLL
                | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        // Do not reserve an empty footer before a copy has produced feedback.
        // SAFETY: The window/control belongs to the live UI-thread owner. These synchronous
        // calls occur without a held callback-state mutable borrow.
        let _ = unsafe { ShowWindow(self.controls.status, SW_HIDE) };
        self.controls.minimize = self.control(
            w!("BUTTON"),
            tr("最小化", "Minimize"),
            CAPTION_MINIMIZE,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.caption_close = self.control(
            w!("BUTTON"),
            tr("关闭窗口", "Close window"),
            CAPTION_CLOSE,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        // The color STATIC overlaps the caption buttons. Keep it behind every
        // sibling, and clip its paint so hover/focus on the buttons stays visible.
        // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
        // passed by value and no callback-state RefMut spans synchronous layout reentry.
        unsafe {
            SetWindowPos(
                self.controls.swatch,
                Some(HWND_BOTTOM),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )?
        };
        for row in self.controls.rows {
            for control in [row.edit, row.copy] {
                // SAFETY: Each row control belongs to this root tree. The stable callback Box outlives
                // all child subclass termination; WM_NCDESTROY removes each subclass first.
                if !unsafe {
                    SetWindowSubclass(
                        control,
                        Some(scroll_control_proc),
                        1,
                        self.callback.as_ref() as *const CallbackState as usize,
                    )
                }
                .as_bool()
                {
                    return Err(Error::from_thread());
                }
            }
        }
        Ok(())
    }

    fn control(
        &self,
        class: PCWSTR,
        text: &str,
        id: usize,
        style: WINDOW_STYLE,
        ex_style: WINDOW_EX_STYLE,
    ) -> Result<HWND> {
        let text = wide(text);
        // SAFETY: The parent belongs to this owner, and class/text are terminated strings.
        // Creation copies text synchronously; the root owns cleanup of partial children.
        unsafe {
            super::window_lifetime::create_window(|| {
                CreateWindowExW(
                    ex_style,
                    class,
                    PCWSTR(text.as_ptr()),
                    WS_CHILD | WS_VISIBLE | style,
                    0,
                    0,
                    1,
                    1,
                    Some(
                        if id == 11
                            || (20..24).contains(&id)
                            || (30..34).contains(&id)
                            || (COPY_ROW..COPY_ROW + 4).contains(&id)
                        {
                            self.callback.viewport.get()
                        } else {
                            self.hwnd
                        },
                    ),
                    Some(HMENU(id as *mut _)),
                    Some(GetModuleHandleW(None)?.into()),
                    None,
                )
            })
        }
    }

    fn dpi(&self) -> Result<u32> {
        // SAFETY: The current callback or window owner retains this HWND through the scalar
        // DPI query; no native handle ownership is transferred.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 || dpi > 9600 {
            Err(Error::new(E_FAIL, "Could not determine result-window DPI"))
        } else {
            Ok(dpi)
        }
    }

    fn place_initially(&self) -> Result<()> {
        // Move inside the source monitor while hidden before querying its DPI.
        // SAFETY: The screen coordinate is passed by value and the query returns a borrowed
        // monitor identifier; it creates or retains no Rust references.
        let monitor = unsafe {
            MonitorFromPoint(
                POINT {
                    x: self.picked.source.x,
                    y: self.picked.source.y,
                },
                MONITOR_DEFAULTTONEAREST,
            )
        };
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
        // WM_NCCALCSIZE makes the whole window client space: do not add the
        // former native caption/border dimensions back into the layout.
        // Placement must stay on the source monitor whose DPI was just read.
        // The unbounded preferred rectangle can overlap a neighbouring screen.
        let rect = layout::fit_rect(
            RECT {
                left: self.picked.source.x,
                top: self.picked.source.y,
                right: self.picked.source.x.saturating_add(dip(CLIENT_WIDTH, dpi)),
                bottom: self.picked.source.y.saturating_add(dip(CLIENT_HEIGHT, dpi)),
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
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )?
        };
        // Final placement supersedes synchronous DPI suggestions produced by
        // the hidden temporary-size window; applying them later would shrink it.
        let mut pending = self.callback.pending.get();
        pending.dpi_rect = None;
        self.callback.pending.set(pending);
        Ok(())
    }

    fn minimum_size(&self) -> Result<(i32, i32)> {
        let dpi = self.dpi()?;
        Ok((
            dip(240, dpi),
            dip(if self.status_visible.get() { 262 } else { 206 }, dpi),
        ))
    }

    fn fit_window(&self) -> Result<()> {
        let dpi = self.dpi()?;
        let mut rect = RECT::default();
        // SAFETY: The live window owner or callback retains this HWND. The initialized local
        // RECT is writable for exactly this synchronous query and is not retained.
        unsafe { GetWindowRect(self.hwnd, &mut rect)? };
        let work = layout::work_area(rect)?;
        rect.right = rect.left.saturating_add(dip(CLIENT_WIDTH, dpi));
        rect.bottom = rect.top.saturating_add(dip(
            if self.status_visible.get() {
                CLIENT_HEIGHT_WITH_STATUS
            } else {
                CLIENT_HEIGHT
            },
            dpi,
        ));
        let rect =
            layout::fit_rect(rect, work, self.minimum_size()?).ok_or_else(layout::unavailable)?;
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
                SWP_NOZORDER | SWP_NOACTIVATE,
            )?
        };
        Ok(())
    }

    fn layout(&self) -> Result<()> {
        let dpi = self.dpi()?;
        self.callback.theme.update_window_icons(self.hwnd);
        if self.resources.borrow().dpi != dpi {
            self.update_resources(dpi)?;
        }
        let mut client = RECT::default();
        // SAFETY: The live window owner or callback retains this HWND. The initialized local
        // RECT is writable for exactly this synchronous query and is not retained.
        unsafe { GetClientRect(self.hwnd, &mut client)? };
        let narrow = client.right < dip(400, dpi);
        let footer = dip(
            if narrow { 94 } else { 54 } + if self.status_visible.get() { 56 } else { 0 },
            dpi,
        );
        let header = dip(SWATCH_HEIGHT, dpi).min(client.bottom - footer - dip(64, dpi));
        if header < dip(48, dpi) || client.right < dip(240, dpi) {
            return Err(layout::unavailable());
        }
        let panes = SheetLayout::new(client.right, client.bottom, header, footer, dip(64, dpi))?;
        let viewport = self.callback.viewport.get();
        let viewport_height = panes.viewport.bottom - panes.viewport.top;
        self.callback.viewport_height.set(viewport_height);
        self.callback.header_height.set(header);
        // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
        // passed by value and no callback-state RefMut spans synchronous layout reentry.
        unsafe {
            MoveWindow(
                self.controls.swatch,
                0,
                0,
                client.right
                    - border_thickness(
                        client.right,
                        client.bottom,
                        dpi,
                        self.callback.border_width_dip,
                    ),
                header,
                true,
            )?;
            MoveWindow(
                viewport,
                0,
                header,
                client.right
                    - border_thickness(
                        client.right,
                        client.bottom,
                        dpi,
                        self.callback.border_width_dip,
                    ),
                viewport_height,
                true,
            )?;
        }
        for (id, button) in [
            (CAPTION_MINIMIZE, self.controls.minimize),
            (CAPTION_CLOSE, self.controls.caption_close),
        ] {
            let r = caption_button_rect(id, client.right, dpi);
            // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
            // passed by value and no callback-state RefMut spans synchronous layout reentry.
            unsafe {
                MoveWindow(
                    button,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    true,
                )?
            };
        }
        // Reserve scrollbar width before choosing a row layout. This avoids
        // oscillating between wide and narrow layouts as the bar appears.
        let content_width = ((i64::from(client.right) * 96) / i64::from(dpi)) as i32 - 20;
        let geometry = result_content_layout(content_width);
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
        // SAFETY: The live viewport owns this scrollbar. scroll advertises its actual size;
        // SetScrollInfo copies this local descriptor and does not retain a Rust reference.
        unsafe {
            SetScrollInfo(viewport, SB_VERT, &scroll, true);
        }
        // SAFETY: All closure callers pass this owner's content controls. The validated
        // rect is copied by value, and no mutable callback-state borrow spans reentry.
        let place_content = |hwnd, r: RECT| unsafe {
            MoveWindow(
                hwnd,
                dip(r.left, dpi),
                dip(r.top, dpi) - offset,
                dip(r.right - r.left, dpi),
                dip(r.bottom - r.top, dpi),
                true,
            )
        };
        place_content(self.controls.source, geometry.source)?;
        for (row, rects) in self.controls.rows.iter().zip(geometry.rows) {
            for (hwnd, r) in [row.label, row.edit, row.copy].into_iter().zip(rects) {
                place_content(hwnd, r)?;
            }
        }
        let margin = dip(16, dpi);
        let gap = dip(12, dpi);
        let y = panes.actions.top + dip(8, dpi);
        let width = client.right - 2 * margin;
        let button_height = dip(32, dpi);
        let place =
            // SAFETY: The closure only receives this owner's action buttons and calculated
            // geometry; synchronous layout callbacks never borrow a held RefMut.
            |hwnd, x, y, width| unsafe { MoveWindow(hwnd, x, y, width, button_height, true) };
        let status_y = if narrow {
            place(self.controls.default_copy, margin, y, width)?;
            let second_y = y + dip(40, dpi);
            let half = (width - gap) / 2;
            place(self.controls.pick_again, margin, second_y, half)?;
            place(
                self.controls.close,
                margin + half + gap,
                second_y,
                width - half - gap,
            )?;
            second_y + dip(40, dpi)
        } else {
            let close_width = dip(64, dpi);
            let again_width = dip(126, dpi);
            place(
                self.controls.default_copy,
                margin,
                y,
                width - close_width - again_width - 2 * gap,
            )?;
            place(
                self.controls.pick_again,
                client.right - margin - close_width - gap - again_width,
                y,
                again_width,
            )?;
            place(
                self.controls.close,
                client.right - margin - close_width,
                y,
                close_width,
            )?;
            y + dip(40, dpi)
        };
        // SAFETY: The live owner retains these HWNDs on their creating thread. Geometry is
        // passed by value and no callback-state RefMut spans synchronous layout reentry.
        unsafe {
            MoveWindow(
                self.controls.status,
                margin,
                status_y,
                width,
                dip(44, dpi),
                true,
            )?;
        }
        // SAFETY: The owner retains this window on its UI thread; None invalidates its client
        // area without lending any RECT pointer or triggering immediate drawing here.
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        // SAFETY: The owner retains this window on its UI thread; None invalidates its client
        // area without lending any RECT pointer or triggering immediate drawing here.
        let _ = unsafe { InvalidateRect(Some(viewport), None, false) };
        Ok(())
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
        // SAFETY: The validated child and viewport remain in this owner's tree. Local RECT
        // and POINT outputs have the exact native layouts and are not retained.
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

    fn update_resources(&self, dpi: u32) -> Result<()> {
        let font = Font::new(13, dpi, 400, false)?;
        let small_font = Font::new(11, dpi, 400, false)?;
        let value_font = Font::new(14, dpi, 400, true)?;
        let set_font = |hwnd, font: &Font| {
            // SAFETY: The new font owner stays live, and all controls receive replacement fonts
            // before the old FontState is replaced; SendMessage copies the handle only.
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_SETFONT,
                    Some(WPARAM(font.raw().0 as usize)),
                    Some(LPARAM(1)),
                )
            };
        };
        for hwnd in self.controls.handles() {
            set_font(hwnd, &font);
        }
        for hwnd in [self.controls.source, self.controls.status] {
            set_font(hwnd, &small_font);
        }
        for row in self.controls.rows {
            set_font(row.label, &small_font);
            set_font(row.edit, &value_font);
            set_font(row.copy, &small_font);
        }
        // All controls now borrow the new handles, so previous DPI fonts can
        // be released without retaining a RefCell borrow across window messages.
        let old = self.resources.replace(Resources {
            dpi,
            _font: Some(font),
            _small_font: Some(small_font),
            _value_font: Some(value_font),
        });
        if self.callback.theme.fonts_must_be_retained() {
            old.retain_fonts();
        }
        drop(old);
        Ok(())
    }

    fn update_default_style(&self) {
        let default_id = self.callback.default_id.get();
        for (id, hwnd) in [
            (COPY_DEFAULT, self.controls.default_copy),
            (PICK_AGAIN, self.controls.pick_again),
            (CLOSE, self.controls.close),
            (CAPTION_MINIMIZE, self.controls.minimize),
            (CAPTION_CLOSE, self.controls.caption_close),
        ]
        .into_iter()
        .chain(
            self.controls
                .rows
                .iter()
                .enumerate()
                .map(|(i, row)| (COPY_ROW + i, row.copy)),
        ) {
            let style = if id == default_id {
                BS_DEFPUSHBUTTON
            } else {
                BS_PUSHBUTTON
            };
            // SAFETY: These are owned BUTTON controls; BM_SETSTYLE uses scalar style flags only.
            // No callback-state RefCell borrow spans the synchronous notification.
            unsafe {
                SendMessageW(
                    hwnd,
                    BM_SETSTYLE,
                    Some(WPARAM(style as usize)),
                    Some(LPARAM(1)),
                )
            };
        }
    }

    fn status(&self, text: &str, tone: Tone) -> Result<()> {
        self.callback.status_tone.set(tone);
        let text = wide(text);
        // SAFETY: The target control belongs to this owner and the terminated UTF-16 string
        // lives until SetWindowTextW finishes copying it; no text pointer is retained.
        unsafe { SetWindowTextW(self.controls.status, PCWSTR(text.as_ptr()))? };
        if !self.status_visible.replace(true) {
            self.fit_window()?;
            self.layout()?;
        }
        // SAFETY: The window/control belongs to the live UI-thread owner. These synchronous
        // calls occur without a held callback-state mutable borrow.
        let _ = unsafe { ShowWindow(self.controls.status, SW_SHOWNA) };
        Ok(())
    }

    fn copy_button(&self, target: CopyTarget) -> HWND {
        if target.button_id == COPY_DEFAULT {
            self.controls.default_copy
        } else {
            self.controls.rows[target.button_id - COPY_ROW].copy
        }
    }

    fn clear_copy_feedback(&self) -> Result<()> {
        let timer = self.callback.feedback_timer.replace(0);
        if timer != 0 {
            // SAFETY: This timer ID belongs to the still-live owned window. Cancellation carries
            // no pointer, and token checks separately reject already queued stale messages.
            let _ = unsafe { KillTimer(Some(self.hwnd), timer) };
        }
        if let Some(target) = self.copied_target.take() {
            let label = wide(&crate::tr_format!(
                "复制 {}",
                "Copy {}",
                target.format.label()
            ));
            // SAFETY: The target control belongs to this owner and the terminated UTF-16 string
            // lives until SetWindowTextW finishes copying it; no text pointer is retained.
            unsafe { SetWindowTextW(self.copy_button(target), PCWSTR(label.as_ptr()))? };
        }
        Ok(())
    }

    fn show_copy_feedback(&self, target: CopyTarget) -> Result<()> {
        self.clear_copy_feedback()?;
        // Normal success never reveals or resizes the detail area. Recovery
        // from a final failure removes its now-empty footer at the same origin.
        // SAFETY: The target control belongs to this owner and the terminated UTF-16 string
        // lives until SetWindowTextW finishes copying it; no text pointer is retained.
        unsafe { SetWindowTextW(self.controls.status, w!(""))? };
        // SAFETY: The window/control belongs to the live UI-thread owner. These synchronous
        // calls occur without a held callback-state mutable borrow.
        let _ = unsafe { ShowWindow(self.controls.status, SW_HIDE) };
        if self.status_visible.replace(false) {
            self.fit_window()?;
            self.layout()?;
        }
        let timer = next_copy_token()?;
        // Never leave a permanent "copied" label if Windows cannot arm its
        // short-lived reset timer. Retry and feedback IDs share one generator.
        // SAFETY: The live root owns this generated timer ID. No TIMERPROC pointer is used;
        // WM_TIMER is validated against the current token and canceled before teardown.
        if unsafe { SetTimer(Some(self.hwnd), timer, COPIED_FEEDBACK_MS, None) } == 0 {
            return Ok(());
        }
        self.callback.feedback_timer.set(timer);
        self.copied_target.set(Some(target));
        let label = wide(&crate::tr_format!(
            "已复制 {}",
            "Copied {}",
            target.format.label()
        ));
        // SAFETY: The target control belongs to this owner and the terminated UTF-16 string
        // lives until SetWindowTextW finishes copying it; no text pointer is retained.
        unsafe { SetWindowTextW(self.copy_button(target), PCWSTR(label.as_ptr()))? };
        Ok(())
    }

    fn stop_timer(&self) {
        let timer = self.callback.active_timer.replace(0);
        if timer != 0 {
            // SAFETY: This timer ID belongs to the still-live owned window. Cancellation carries
            // no pointer, and token checks separately reject already queued stale messages.
            let _ = unsafe { KillTimer(Some(self.hwnd), timer) };
        }
    }

    fn cancel_copy(&self) {
        self.copy.borrow_mut().0 = None;
        self.stop_timer();
    }

    fn attempt_copy(&self, token: usize) -> Result<()> {
        let Some(request) = self.copy.borrow().matching(token) else {
            return Ok(());
        };
        let text = format_color(self.picked.rgb, request.target.format);
        let outcome = Clipboard::copy_text(self.hwnd, &text);
        self.finish_copy_attempt(request, outcome)
    }

    fn copy_failed(&self, text: &str) -> Result<()> {
        self.status(text, Tone::Error)?;
        // Once a failed quick pick is visible, retry behaves like any other
        // result window. Its later successful copies do not dismiss it.
        if self.quick_pick_pending.replace(false) {
            self.show_result();
        }
        Ok(())
    }

    fn finish_copy_attempt(
        &self,
        request: CopyRequest,
        outcome: std::result::Result<(), ClipboardError>,
    ) -> Result<()> {
        match outcome {
            Ok(()) => {
                self.copy.borrow_mut().0 = None;
                if self.quick_pick_pending.replace(false) {
                    self.callback
                        .queue(|pending| pending.action = Some(ResultAction::Close));
                    Ok(())
                } else {
                    self.show_copy_feedback(request.target)
                }
            }
            Err(ClipboardError::Busy) => {
                let retry = self.copy.borrow_mut().reserve_retry(request.token);
                if retry {
                    // Every arming has a fresh ID, including retries within one
                    // copy request. An already queued WM_TIMER cannot shorten
                    // the next 50 ms delay after KillTimer and rearming.
                    let timer = next_copy_token()?;
                    // SAFETY: The live root owns this generated timer ID. No TIMERPROC pointer is used;
                    // WM_TIMER is validated against the current token and canceled before teardown.
                    if unsafe { SetTimer(Some(self.hwnd), timer, RETRY_MS, None) } == 0 {
                        self.copy.borrow_mut().0 = None;
                        return self.copy_failed(tr(
                            "剪贴板被占用，无法安排重试。请再次点击复制。",
                            "Clipboard busy; retry unavailable. Click Copy to try again.",
                        ));
                    }
                    self.callback.active_timer.set(timer);
                    Ok(())
                } else {
                    self.copy_failed(tr(
                        "复制失败：剪贴板仍被占用。请再次点击复制。",
                        "Clipboard still busy. Click Copy to try again.",
                    ))
                }
            }
            Err(ClipboardError::Other(error)) => {
                self.copy.borrow_mut().0 = None;
                self.copy_failed(&crate::tr_format!(
                    "复制失败：{error}",
                    "Copy failed: {error}"
                ))
            }
        }
    }
}

impl Drop for ResultWindow {
    fn drop(&mut self) {
        self.callback.closing.set(true);
        if self.callback.lifetime.is_alive() {
            self.cancel_copy();
            let _ = self.clear_copy_feedback();
        }
        self.callback.lifetime.destroy("result");
        // Native teardown may itself draw. Check the sticky failure only after
        // its last callback, before releasing any possibly selected font owner.
        if self.callback.theme.fonts_must_be_retained() {
            self.resources.get_mut().retain_fonts();
        }
    }
}

struct ResultContentLayout {
    source: RECT,
    rows: [[RECT; 3]; 4],
    height: i32,
}

fn result_content_layout(width: i32) -> ResultContentLayout {
    let rect = |x, y, w, h| RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    let narrow = width < 360;
    let row_height = if narrow { 58 } else { 36 };
    let row_top = if narrow { 52 } else { 44 };
    ResultContentLayout {
        source: rect(16, 12, width - 32, if narrow { 36 } else { 22 }),
        rows: std::array::from_fn(|index| {
            let y = row_top + index as i32 * row_height;
            if narrow {
                [
                    rect(16, y + 4, 68, 18),
                    rect(16, y + 28, width - 32, 24),
                    rect(width - 64, y, 48, 26),
                ]
            } else {
                [
                    rect(16, y + 9, 60, 18),
                    rect(84, y + 7, width - 160, 22),
                    rect(width - 64, y + 4, 48, 28),
                ]
            }
        }),
        height: row_top + 4 * row_height + 8,
    }
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
        // SAFETY: The original subclass message and any native payload remain valid until
        // this callback returns; all temporary mutable state borrows have ended.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }))
    .unwrap_or_else(|_| std::process::abort())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn next_copy_token() -> Result<usize> {
    NEXT_COPY_TOKEN
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| Error::new(E_FAIL, "Clipboard retry token exhausted"))
}

fn caption_button_rect(id: usize, width: i32, dpi: u32) -> RECT {
    let offset = if id == CAPTION_MINIMIZE { 38 } else { 0 };
    let right = width - dip(6 + offset, dpi);
    RECT {
        left: right - dip(36, dpi),
        top: dip(6, dpi),
        right,
        bottom: dip(36, dpi),
    }
}

fn caption_hit_test(hwnd: HWND, lparam: LPARAM, header_height: i32) -> LRESULT {
    // Screen coordinates can be negative on monitors left/above the primary.
    let mut point = POINT {
        x: (lparam.0 as u16 as i16) as i32,
        y: ((lparam.0 >> 16) as u16 as i16) as i32,
    };
    let mut client = RECT::default();
    // SAFETY: The callback retains hwnd; initialized local POINT/RECT outputs are valid
    // for these geometry calls, with no pointer retained after return.
    if !unsafe { ScreenToClient(hwnd, &mut point) }.as_bool()
        // SAFETY: The live window owner or callback retains this HWND. The initialized local
        // RECT is writable for exactly this synchronous query and is not retained.
        || unsafe { GetClientRect(hwnd, &mut client) }.is_err()
    {
        return LRESULT(HTCLIENT as isize);
    }
    // SAFETY: The current callback or window owner retains this HWND through the scalar
    // DPI query; no native handle ownership is transferred.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let inside = |rect: RECT| {
        point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
    };
    if [CAPTION_MINIMIZE, CAPTION_CLOSE]
        .into_iter()
        .any(|id| inside(caption_button_rect(id, client.right, dpi)))
    {
        return LRESULT(HTCLIENT as isize);
    }
    if inside(client) && point.y < header_height {
        LRESULT(HTCAPTION as isize)
    } else {
        LRESULT(HTCLIENT as isize)
    }
}

fn caption_ink(color: COLORREF) -> COLORREF {
    let linear = |shift: u32| {
        let channel = f64::from((color.0 >> shift) & 0xff_u32) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(0) + 0.7152 * linear(8) + 0.0722 * linear(16);
    if luminance > 0.179 {
        COLORREF(0)
    } else {
        COLORREF(0x00ffffff)
    }
}

fn caption_tint(base: COLORREF, ink: COLORREF, percent: u32) -> COLORREF {
    let channel = |shift: u32| {
        ((((base.0 >> shift) & 0xff_u32) * (100 - percent)
            + ((ink.0 >> shift) & 0xff_u32) * percent)
            / 100)
            << shift
    };
    COLORREF(channel(0) | channel(8) | channel(16))
}

fn draw_caption_button(draw: &NMCUSTOMDRAW, background: COLORREF) -> Option<LRESULT> {
    let header = &draw.hdr;
    if ![CAPTION_MINIMIZE, CAPTION_CLOSE].contains(&header.idFrom) {
        return None;
    }
    if draw.dwDrawStage != CDDS_PREPAINT {
        return None;
    }
    let hdc = draw.hdc;
    // SAFETY: The current callback or window owner retains this HWND through the scalar
    // DPI query; no native handle ownership is transferred.
    let dpi = unsafe { GetDpiForWindow(header.hwndFrom) }.max(96);
    let hot = draw.uItemState.contains(CDIS_HOT);
    let pressed = draw.uItemState.contains(CDIS_SELECTED);
    let disabled = draw.uItemState.contains(CDIS_DISABLED);
    let mut ink = caption_ink(background);
    let fill = if disabled {
        ink = caption_tint(background, ink, 40);
        background
    } else if header.idFrom == CAPTION_CLOSE && (hot || pressed) {
        ink = COLORREF(0x00ffffff);
        if pressed {
            COLORREF(0x001f0fc5)
        } else {
            COLORREF(0x002311e8)
        }
    } else if pressed || hot {
        caption_tint(background, ink, if pressed { 20 } else { 10 })
    } else {
        background
    };
    let pen = OwnedPen::new(PS_SOLID, dip(1, dpi).max(1), ink).ok()?;
    // SAFETY: Native custom draw lends this DC for the callback duration. The
    // pen is declared first so unwinding restores the DC before deleting it.
    let saved = unsafe { SavedDc::new(hdc) }.ok()?;
    // SAFETY: The typed record lends this live DC/RECT. Stock objects are borrowed;
    // the owned pen remains live until successful restore, or is retained on error.
    unsafe {
        SetDCBrushColor(hdc, background);
        FillRect(hdc, &draw.rc, HBRUSH(GetStockObject(DC_BRUSH).0));
        SelectObject(hdc, GetStockObject(DC_BRUSH));
        SelectObject(hdc, GetStockObject(DC_PEN));
        SetDCBrushColor(hdc, fill);
        SetDCPenColor(hdc, fill);
        let _ = RoundRect(
            hdc,
            draw.rc.left,
            draw.rc.top,
            draw.rc.right,
            draw.rc.bottom,
            dip(8, dpi),
            dip(8, dpi),
        );
        SelectObject(hdc, pen.raw().into());
        let x = (draw.rc.left + draw.rc.right) / 2;
        let y = (draw.rc.top + draw.rc.bottom) / 2;
        let radius = dip(4, dpi);
        if header.idFrom == CAPTION_MINIMIZE {
            let _ = MoveToEx(hdc, x - radius, y, None);
            let _ = LineTo(hdc, x + radius + 1, y);
        } else {
            let _ = MoveToEx(hdc, x - radius, y - radius, None);
            let _ = LineTo(hdc, x + radius + 1, y + radius + 1);
            let _ = MoveToEx(hdc, x + radius, y - radius, None);
            let _ = LineTo(hdc, x - radius - 1, y + radius + 1);
        }
        if draw.uItemState.contains(CDIS_FOCUS) && !disabled {
            SelectObject(hdc, GetStockObject(NULL_BRUSH));
            let inset = dip(2, dpi);
            let _ = RoundRect(
                hdc,
                draw.rc.left + inset,
                draw.rc.top + inset,
                draw.rc.right - inset,
                draw.rc.bottom - inset,
                dip(6, dpi),
                dip(6, dpi),
            );
        }
    }
    if saved.restore().is_err() {
        // A borrowed control DC cannot be deleted. Retain the possibly selected
        // pen if restoring the prior selection failed.
        pen.retain();
        return None;
    }
    Some(LRESULT(CDRF_SKIPDEFAULT as isize))
}

fn draw_swatch(draw: &DRAWITEMSTRUCT, color: COLORREF) -> LRESULT {
    let hdc = draw.hDC;
    // SAFETY: the owner-draw callback keeps hdc alive; only stock brushes are used.
    let Ok(saved) = (unsafe { SavedDc::new(hdc) }) else {
        return LRESULT(0);
    };
    // SAFETY: The owner-draw record lends a live DC and RECT for this callback. The stock
    // DC brush is borrowed, and SavedDc restores the changed drawing attributes.
    unsafe {
        SetDCBrushColor(hdc, color);
        FillRect(hdc, &draw.rcItem, HBRUSH(GetStockObject(DC_BRUSH).0));
    }
    if saved.restore().is_err() {
        return LRESULT(0);
    }
    LRESULT(1)
}

fn paint_result(hwnd: HWND, border_width_dip: u8) -> LRESULT {
    // SAFETY: native WM_PAINT supplies this live window; no mutable state borrow exists.
    if let Ok(paint) = unsafe { PaintSession::begin(hwnd) } {
        let hdc = paint.dc;
        let mut client = RECT::default();
        // SAFETY: The live window owner or callback retains this HWND. The initialized local
        // RECT is writable for exactly this synchronous query and is not retained.
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        // SAFETY: The paint session retains the DC and the local rectangle is initialized;
        // the system WHITE_BRUSH is borrowed and must not be deleted.
        unsafe { FillRect(hdc, &client, HBRUSH(GetStockObject(WHITE_BRUSH).0)) };
        // SAFETY: the paint session outlives this stock-object drawing scope.
        if let Ok(saved) = unsafe { SavedDc::new(hdc) } {
            // SAFETY: The current callback or window owner retains this HWND through the scalar
            // DPI query; no native handle ownership is transferred.
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            let _ =
                draw_bottom_right_border(hdc, client.right, client.bottom, dpi, border_width_dip);
            if let Err(error) = saved.restore() {
                crate::app::diagnostics::event(format_args!("result.paint_restore_failed {error}"));
            }
        }
    }
    LRESULT(0)
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
        WM_NCCALCSIZE if hwnd != state.viewport.get() => LRESULT(0),
        // With DWM nonclient rendering disabled, DefWindowProc would paint a
        // classic caption/frame over our client area, including on activation.
        WM_NCPAINT if hwnd != state.viewport.get() => LRESULT(0),
        WM_NCACTIVATE if hwnd != state.viewport.get() => LRESULT(1),
        WM_NCHITTEST if hwnd != state.viewport.get() => {
            caption_hit_test(hwnd, lparam, state.header_height.get())
        }
        // A fixed-size results sheet has no maximize/resize mode, even when
        // invoked through the system menu or a double click on its color field.
        WM_NCLBUTTONDBLCLK if wparam.0 == HTCAPTION as usize => LRESULT(0),
        WM_SYSCOMMAND if matches!((wparam.0 & 0xfff0) as u32, SC_MAXIMIZE | SC_SIZE) => LRESULT(0),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => paint_result(
            hwnd,
            if hwnd == state.viewport.get() {
                0
            } else {
                state.border_width_dip
            },
        ),
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            // SAFETY: This message supplies a child-control HWND (not a payload pointer); USER32
            // queries its numeric ID without taking ownership or retaining Rust references.
            let id = unsafe { GetDlgCtrlID(HWND(lparam.0 as *mut _)) } as usize;
            let tone = if id == 12 {
                state.status_tone.get()
            } else if id == 11 || (20..24).contains(&id) {
                Tone::Muted
            } else {
                Tone::Text
            };
            state
                .theme
                .control_color(HDC(wparam.0 as *mut _), true, tone)
        }
        WM_NOTIFY => {
            // SAFETY: this is the native notification callback; decoder validates
            // the sender and code before borrowing a typed custom-draw payload.
            unsafe {
                messages::with_button_custom_draw(hwnd, lparam, |draw| {
                    draw_caption_button(draw, state.swatch_color)
                        .or_else(|| theme::custom_draw_minimal(&state.theme, draw, COPY_DEFAULT))
                })
            }
            .flatten()
            .unwrap_or_else(|| {
                // SAFETY: The original native notification remains valid for
                // the default procedure throughout this callback.
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            })
        }
        WM_DRAWITEM if wparam.0 == 10 => {
            // SAFETY: native WM_DRAWITEM is decoded once and validated as our static control.
            unsafe {
                messages::with_static_draw(hwnd, lparam, 10, |draw| {
                    draw_swatch(draw, state.swatch_color)
                })
            }
            // SAFETY: Forward the unchanged native message while its payload remains valid for
            // this callback; the default procedure does not gain Rust-state ownership.
            .unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, message, wparam, lparam) })
        }
        WM_CLOSE => {
            state.queue(|pending| pending.action = Some(ResultAction::Close));
            LRESULT(0)
        }
        WM_COMMAND if (wparam.0 >> 16) as u32 == BN_CLICKED => {
            match wparam.0 & 0xffff {
                CLOSE | CAPTION_CLOSE => {
                    state.queue(|pending| pending.action = Some(ResultAction::Close))
                }
                CAPTION_MINIMIZE => state.queue(|pending| pending.minimize = true),
                PICK_AGAIN => state.queue(|pending| pending.action = Some(ResultAction::PickAgain)),
                COPY_DEFAULT => state.queue(|pending| {
                    pending.copy = Some(CopyTarget {
                        format: state.default_format,
                        button_id: COPY_DEFAULT,
                    })
                }),
                id if (COPY_ROW..COPY_ROW + 4).contains(&id) => state.queue(|pending| {
                    pending.copy = Some(CopyTarget {
                        format: ColorFormat::ALL[id - COPY_ROW],
                        button_id: id,
                    })
                }),
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 != 0 && state.active_timer.get() == wparam.0 => {
            state.queue(|pending| pending.timer = Some(wparam.0));
            LRESULT(0)
        }
        WM_TIMER if wparam.0 != 0 && state.feedback_timer.get() == wparam.0 => {
            state.queue(|pending| pending.feedback_timer = Some(wparam.0));
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // SAFETY: dispatch is handling native WM_DPICHANGED.
            if let Some(rect) = unsafe { messages::dpi_rect(lparam) } {
                state.queue(|pending| {
                    pending.dpi_rect = Some(rect);
                    pending.layout = true;
                });
            }
            LRESULT(0)
        }
        WM_SIZE if hwnd != state.viewport.get() => {
            if wparam.0 != SIZE_MINIMIZED as usize {
                state.queue(|pending| pending.layout = true);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE if hwnd != state.viewport.get() => {
            state.queue(|pending| pending.fit_work_area = true);
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
            let total = state.wheel_remainder.get() + ((wparam.0 >> 16) as u16 as i16) as i32;
            state.wheel_remainder.set(total % 120);
            let current = state
                .pending
                .get()
                .scroll
                .unwrap_or(state.scroll_offset.get());
            state.queue(|pending| {
                pending.scroll =
                    // SAFETY: The current callback or window owner retains this HWND through the scalar
                    // DPI query; no native handle ownership is transferred.
                    Some(current - total / 120 * dip(72, unsafe { GetDpiForWindow(hwnd) }.max(96)))
            });
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
mod tests {
    use super::*;

    #[test]
    fn custom_draw_restore_failure_retains_replaced_and_final_result_fonts() {
        use super::super::theme::restore_tests;
        for fail_restore in [false, true] {
            let result = quick_result();
            result.callback.pending.take();
            let handles = || {
                let resources = result.resources.borrow();
                [
                    &resources._font,
                    &resources._small_font,
                    &resources._value_font,
                ]
                .into_iter()
                .flatten()
                .map(Font::raw)
                .collect::<Vec<_>>()
            };
            let previous = handles();
            restore_tests::begin_font_delete_tracking();
            restore_tests::draw_button(
                result.hwnd,
                result.controls.default_copy,
                COPY_DEFAULT,
                &result.callback.theme,
                fail_restore,
            );
            result.update_resources(result.dpi().unwrap() + 48).unwrap();
            restore_tests::assert_font_delete_attempts(&previous, !fail_restore);
            let current = handles();
            restore_tests::begin_font_delete_tracking();
            drop(result);
            restore_tests::assert_font_delete_attempts(&current, !fail_restore);
            if fail_restore {
                restore_tests::cleanup_retained_fonts(&previous);
                restore_tests::cleanup_retained_fonts(&current);
            }
        }
    }

    #[test]
    fn native_result_partial_creation_failures_end_every_root() {
        for nth in [1, 2, 3, 7, 12] {
            super::super::window_lifetime::creation_failure_test(nth, || {
                let result = ResultWindow::new_with_behavior(
                    PickedColor {
                        rgb: crate::core::color::Rgb8::new(1, 2, 3),
                        source: crate::core::geometry::ScreenPointPx { x: 0, y: 0 },
                        kind: SampleKind::Live,
                    },
                    HWND::default(),
                    ColorFormat::Hex,
                    false,
                    AppearanceConfig::default(),
                    true,
                );
                assert!(result.is_err(), "stage {nth} must fail");
            });
        }
    }

    #[test]
    fn native_result_content_and_external_root_destruction_are_distinct() {
        let result = quick_result();
        result.callback.pending.take();
        // SAFETY: the hidden child is owned by this fixture, on this UI thread;
        // its callback allocation remains in result through native teardown.
        unsafe { DestroyWindow(result.callback.viewport.get()) }.unwrap();
        assert!(result.callback.lifetime.is_alive());
        // SAFETY: the fixture root is still live and all callback fields outlive this call.
        unsafe { DestroyWindow(result.hwnd) }.unwrap();
        assert!(!result.callback.lifetime.is_alive());
        // Drop must skip timers, controls and the now-stale root handle.
        drop(result);
    }

    #[test]
    fn narrow_result_rows_fit_and_every_edit_and_copy_can_scroll_into_view() {
        for width in [220, 260, 300, 350, 360, 400] {
            let content = result_content_layout(width);
            for r in std::iter::once(content.source).chain(content.rows.into_iter().flatten()) {
                assert!(r.left >= 0 && r.right <= width && r.right > r.left);
                assert!(r.top >= 0 && r.bottom <= content.height);
                let offset = reveal_offset(0, r.top, r.bottom, 64, content.height, 8);
                assert!(r.top - offset >= 0 && r.bottom - offset <= 64);
            }
        }
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; resizes a hidden fixture without copying or input"]
    fn small_result_viewport_keeps_actions_visible_and_reveals_focused_rows() {
        let result = quick_result();
        // Discard the constructor's quick-copy intention: this test uses no
        // clipboard and delivers only messages to this hidden test fixture.
        result.callback.pending.take();
        result
            .status(
                "Long copy failure detail remains scrollable in a small work area.",
                Tone::Error,
            )
            .unwrap();
        let dpi = result.dpi().unwrap();
        for width in [420, 320, 260] {
            // SAFETY: The fixture owns this hidden result tree on the current thread. Scalar
            // geometry updates are synchronous and no callback-state borrow is held.
            unsafe {
                SetWindowPos(
                    result.hwnd,
                    None,
                    0,
                    0,
                    dip(width, dpi),
                    dip(300, dpi),
                    SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER,
                )
                .unwrap();
            }
            result.process_pending().unwrap();
            let bounds = |hwnd| {
                let mut rect = RECT::default();
                // SAFETY: The fixture retains each queried HWND; rect is a local writable RECT.
                unsafe {
                    GetWindowRect(hwnd, &mut rect).unwrap();
                }
                rect
            };
            let window = bounds(result.hwnd);
            let viewport = bounds(result.callback.viewport.get());
            let actions = [
                result.controls.default_copy,
                result.controls.pick_again,
                result.controls.close,
                result.controls.status,
            ];
            let before: Vec<_> = actions.map(bounds).into();
            for rect in &before {
                assert!(rect.left >= window.left && rect.right <= window.right);
                assert!(rect.top >= viewport.bottom && rect.bottom <= window.bottom);
            }
            for row in result.controls.rows {
                for control in [row.edit, row.copy] {
                    // SAFETY: The fixture owns this control; the synthetic WM_SETFOCUS has no pointer
                    // payload and is dispatched synchronously without generating system input.
                    unsafe {
                        SendMessageW(control, WM_SETFOCUS, None, None);
                    }
                    result.process_pending().unwrap();
                    let rect = bounds(control);
                    assert!(rect.left >= viewport.left && rect.right <= viewport.right);
                    assert!(rect.top >= viewport.top && rect.bottom <= viewport.bottom);
                    assert_eq!(actions.map(bounds).as_slice(), before);
                }
            }
            // SAFETY: The test retains the owning result/control tree while querying visibility;
            // this scalar query neither activates nor changes that window.
            assert!(!unsafe { IsWindowVisible(result.hwnd) }.as_bool());
        }
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; displays a result window without copying"]
    fn feedback_reveals_footer_and_keeps_it_inside_the_work_area() {
        let result = ResultWindow::new_with_appearance(
            PickedColor {
                rgb: crate::core::color::Rgb8::new(244, 242, 242),
                source: crate::core::geometry::ScreenPointPx { x: 0, y: 0 },
                kind: SampleKind::Live,
            },
            HWND::default(),
            ColorFormat::Hex,
            false,
            AppearanceConfig {
                border_width_dip: crate::app::config::MAX_BORDER_WIDTH_DIP,
                ..AppearanceConfig::default()
            },
        )
        .unwrap();
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mut before = RECT::default();
        // SAFETY: The fixture owns the result HWND. The initialized RECT/MONITORINFO outputs
        // have their advertised sizes and remain live for these synchronous queries.
        unsafe {
            GetWindowRect(result.hwnd, &mut before).unwrap();
            let monitor = MonitorFromWindow(result.hwnd, MONITOR_DEFAULTTONEAREST);
            assert!(GetMonitorInfoW(monitor, &mut info).as_bool());
            SetWindowPos(
                result.hwnd,
                None,
                before.left,
                info.rcWork.bottom - (before.bottom - before.top),
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
            )
            .unwrap();
        }
        // Use the error path without touching the real clipboard.
        result
            .status("剪贴板被占用，无法安排重试。请再次点击复制。", Tone::Error)
            .unwrap();
        let mut after = RECT::default();
        let mut status = RECT::default();
        // SAFETY: The result owner retains its root/status HWNDs; both local RECT output
        // slots remain valid through the bounded native writes.
        unsafe {
            GetWindowRect(result.hwnd, &mut after).unwrap();
            GetWindowRect(result.controls.status, &mut status).unwrap();
            assert!(IsWindowVisible(result.controls.status).as_bool());
        }
        assert!(after.bottom - after.top > before.bottom - before.top);
        assert!(after.bottom <= info.rcWork.bottom);
        assert!(
            status.bottom
                <= after.bottom
                    - dip(
                        i32::from(crate::app::config::MAX_BORDER_WIDTH_DIP),
                        result.dpi().unwrap()
                    ),
            "copy feedback must not clip the widest supported bottom border"
        );
        assert!(status.top >= after.top);
        result
            .show_copy_feedback(CopyTarget {
                format: ColorFormat::Hex,
                button_id: COPY_DEFAULT,
            })
            .unwrap();
        let mut repeated = RECT::default();
        // SAFETY: The live window owner or callback retains this HWND. The initialized local
        // RECT is writable for exactly this synchronous query and is not retained.
        unsafe { GetWindowRect(result.hwnd, &mut repeated) }.unwrap();
        assert_eq!(
            repeated.bottom - repeated.top,
            before.bottom - before.top,
            "successful recovery removes the stale error footer"
        );
        assert_eq!(repeated.left, after.left);
        assert_eq!(repeated.top, after.top);
        assert!(!result.status_visible.get());
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(!unsafe { IsWindowVisible(result.controls.status) }.as_bool());
    }

    #[test]
    fn clipboard_contention_has_three_retries_and_then_goes_idle() {
        let mut copy = CopyState::default();
        copy.replace(1, target(ColorFormat::Hex, COPY_DEFAULT));
        for _ in 0..3 {
            assert!(copy.reserve_retry(1));
        }
        assert!(!copy.reserve_retry(1));
        assert!(copy.0.is_none());
    }

    #[test]
    fn replacing_or_canceling_copy_rejects_queued_old_timer() {
        let mut copy = CopyState::default();
        copy.replace(1, target(ColorFormat::Hex, COPY_DEFAULT));
        assert!(copy.reserve_retry(1));
        copy.replace(2, target(ColorFormat::Hsl, COPY_ROW + 3));
        assert!(copy.matching(1).is_none());
        assert!(!copy.reserve_retry(1));
        assert_eq!(copy.matching(2).unwrap().retries_left, 3);
        assert_eq!(
            copy.matching(2).unwrap().target,
            target(ColorFormat::Hsl, COPY_ROW + 3)
        );
        copy.0 = None;
        assert!(copy.matching(2).is_none());
        assert!(!copy.reserve_retry(2));
    }

    fn target(format: ColorFormat, button_id: usize) -> CopyTarget {
        CopyTarget { format, button_id }
    }

    fn quick_result() -> ResultWindow {
        ResultWindow::new_with_behavior(
            PickedColor {
                rgb: crate::core::color::Rgb8::new(244, 242, 242),
                source: crate::core::geometry::ScreenPointPx { x: 0, y: 0 },
                kind: SampleKind::Frozen,
            },
            HWND::default(),
            ColorFormat::CssRgb,
            false,
            AppearanceConfig::default(),
            true,
        )
        .unwrap()
    }

    fn take_initial_copy(result: &ResultWindow) -> CopyRequest {
        // Drive only the outcome path: these desktop checks deliberately do
        // not read or write the user's real clipboard.
        let target = result.callback.pending.take().copy.unwrap();
        assert_eq!(target.format, ColorFormat::CssRgb);
        let token = next_copy_token().unwrap();
        result.copy.borrow_mut().replace(token, target);
        result.copy.borrow().matching(token).unwrap()
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; displays a failed host copy without copying"]
    fn failed_host_copy_opens_manual_result_and_later_success_keeps_it_open() {
        let result = ResultWindow::new_after_copy_failure(
            PickedColor {
                rgb: crate::core::color::Rgb8::new(12, 34, 56),
                source: crate::core::geometry::ScreenPointPx { x: 0, y: 0 },
                kind: SampleKind::Frozen,
            },
            HWND::default(),
            ColorFormat::CssRgb,
            AppearanceConfig::default(),
            "Simulated host copy failure",
        )
        .unwrap();
        assert!(!result.quick_pick_pending.get());
        assert!(result.callback.pending.get().copy.is_none());
        assert!(result.copy.borrow().0.is_none());
        assert!(result.status_visible.get());
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(unsafe { IsWindowVisible(result.hwnd) }.as_bool());
        let token = next_copy_token().unwrap();
        result
            .copy
            .borrow_mut()
            .replace(token, target(ColorFormat::CssRgb, COPY_DEFAULT));
        let request = result.copy.borrow().matching(token).unwrap();
        result.finish_copy_attempt(request, Ok(())).unwrap();
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(unsafe { IsWindowVisible(result.hwnd) }.as_bool());
        assert_eq!(result.process_pending().unwrap(), None);
        assert!(!result.status_visible.get());
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; checks a hidden result without copying"]
    fn quick_copy_success_keeps_focus_and_queues_cleanup_without_showing() {
        // SAFETY: This only queries the current foreground HWND for comparison; the borrowed
        // identifier is not dereferenced, retained as owner, or destroyed.
        let foreground = unsafe { GetForegroundWindow() };
        let result = quick_result();
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(!unsafe { IsWindowVisible(result.hwnd) }.as_bool());
        // SAFETY: This only queries the current foreground HWND for comparison; the borrowed
        // identifier is not dereferenced, retained as owner, or destroyed.
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        assert!(result.quick_pick_pending.get());
        let request = take_initial_copy(&result);
        result.finish_copy_attempt(request, Ok(())).unwrap();
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(!unsafe { IsWindowVisible(result.hwnd) }.as_bool());
        // SAFETY: This only queries the current foreground HWND for comparison; the borrowed
        // identifier is not dereferenced, retained as owner, or destroyed.
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        assert_eq!(result.callback.feedback_timer.get(), 0);
        assert_eq!(result.process_pending().unwrap(), Some(ResultAction::Close));
        assert!(result.copy.borrow().0.is_none());
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; displays copy failure without copying"]
    fn quick_copy_retries_hidden_then_reveals_failure_for_manual_recovery() {
        let result = quick_result();
        let request = take_initial_copy(&result);
        let mut previous_timer = 0;
        for _ in 0..MAX_RETRIES {
            result.stop_timer();
            result
                .finish_copy_attempt(request, Err(ClipboardError::Busy))
                .unwrap();
            let timer = result.callback.active_timer.get();
            assert_ne!(timer, 0);
            assert_ne!(timer, previous_timer);
            previous_timer = timer;
            // SAFETY: The test retains the owning result/control tree while querying visibility;
            // this scalar query neither activates nor changes that window.
            assert!(!unsafe { IsWindowVisible(result.hwnd) }.as_bool());
            assert!(!result.status_visible.get());
        }
        result.stop_timer();
        result
            .finish_copy_attempt(request, Err(ClipboardError::Busy))
            .unwrap();
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(unsafe { IsWindowVisible(result.hwnd) }.as_bool());
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(unsafe { IsWindowVisible(result.controls.status) }.as_bool());
        assert!(result.status_visible.get());
        assert!(!result.quick_pick_pending.get());
        assert!(result.copy.borrow().0.is_none());
        assert_eq!(result.callback.active_timer.get(), 0);

        // A manual successful retry keeps the recovered result available.
        // SAFETY: The test owns the result HWND and supplies a scalar button WM_COMMAND;
        // this synchronous message does not retain a pointer or generate global input.
        unsafe {
            SendMessageW(
                result.hwnd,
                WM_COMMAND,
                Some(WPARAM(COPY_DEFAULT | ((BN_CLICKED as usize) << 16))),
                Some(LPARAM(result.controls.default_copy.0 as isize)),
            )
        };
        let manual_request = take_initial_copy(&result);
        assert_ne!(manual_request.token, request.token);
        result.finish_copy_attempt(manual_request, Ok(())).unwrap();
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(unsafe { IsWindowVisible(result.hwnd) }.as_bool());
        assert!(!result.status_visible.get());
        assert_eq!(result.process_pending().unwrap(), None);
        assert_ne!(result.callback.feedback_timer.get(), 0);
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; displays copy failure without copying"]
    fn quick_copy_non_busy_failure_reveals_result_immediately() {
        let result = quick_result();
        let request = take_initial_copy(&result);
        result
            .finish_copy_attempt(
                request,
                Err(ClipboardError::Other(Error::new(E_FAIL, "test failure"))),
            )
            .unwrap();
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(unsafe { IsWindowVisible(result.hwnd) }.as_bool());
        assert!(result.status_visible.get());
        assert!(!result.quick_pick_pending.get());
        assert_eq!(result.callback.active_timer.get(), 0);
        assert_eq!(result.process_pending().unwrap(), None);
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; displays a result window without copying"]
    fn copy_feedback_preserves_layout_and_rejects_old_reset_timers() {
        use crate::app::i18n::{Language, language, set_language};
        let previous_language = language();
        set_language(Language::SimplifiedChinese);
        let result = ResultWindow::new(
            PickedColor {
                rgb: crate::core::color::Rgb8::new(244, 242, 242),
                source: crate::core::geometry::ScreenPointPx { x: 0, y: 0 },
                kind: SampleKind::Live,
            },
            HWND::default(),
        )
        .unwrap();
        let rect = || {
            let mut rect = RECT::default();
            // SAFETY: The live window owner or callback retains this HWND. The initialized local
            // RECT is writable for exactly this synchronous query and is not retained.
            unsafe { GetWindowRect(result.hwnd, &mut rect) }.unwrap();
            rect
        };
        let text = |hwnd| {
            let mut text = [0_u16; 128];
            // SAFETY: The fixture retains its control; the local UTF-16 buffer is writable and
            // the binding passes its actual length, so the text query cannot overrun it.
            let len = unsafe { GetWindowTextW(hwnd, &mut text) } as usize;
            String::from_utf16_lossy(&text[..len])
        };
        let before = rect();
        let css = target(ColorFormat::CssRgb, COPY_ROW + 2);
        result.show_copy_feedback(css).unwrap();
        let old_timer = result.callback.feedback_timer.get();
        assert_ne!(old_timer, 0);
        assert_eq!(text(result.copy_button(css)), "已复制 CSS RGB");
        assert_eq!(text(result.controls.default_copy), "复制 HEX");
        assert_eq!(rect(), before);
        assert!(!result.status_visible.get());
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(!unsafe { IsWindowVisible(result.controls.status) }.as_bool());

        let hex = target(ColorFormat::Hex, COPY_DEFAULT);
        result.show_copy_feedback(hex).unwrap();
        let current_timer = result.callback.feedback_timer.get();
        assert_ne!(current_timer, old_timer);
        assert_eq!(text(result.copy_button(css)), "复制 CSS RGB");
        assert_eq!(text(result.controls.default_copy), "已复制 HEX");
        // A timer already queued before the second copy must not clear it.
        result
            .callback
            .queue(|pending| pending.feedback_timer = Some(old_timer));
        result.process_pending().unwrap();
        assert_eq!(text(result.controls.default_copy), "已复制 HEX");
        // SAFETY: The helper uses an owned control and a documented scalar-only message;
        // synchronous processing retains no Rust pointer or mutable state borrow.
        unsafe { SendMessageW(result.hwnd, WM_TIMER, Some(WPARAM(current_timer)), None) };
        result.process_pending().unwrap();
        assert_eq!(text(result.controls.default_copy), "复制 HEX");
        assert_eq!(result.callback.feedback_timer.get(), 0);
        assert_eq!(rect(), before);
        drop(result);
        set_language(previous_language);
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; checks English controls without copying"]
    fn english_result_labels_and_copy_feedback_fit_without_resizing() {
        use crate::app::i18n::{Language, language, set_language};
        let previous_language = language();
        set_language(Language::English);
        let result = quick_result();
        let text = |hwnd| {
            let mut text = [0_u16; 128];
            // SAFETY: The fixture retains its control; the local UTF-16 buffer is writable and
            // the binding passes its actual length, so the text query cannot overrun it.
            let length = unsafe { GetWindowTextW(hwnd, &mut text) } as usize;
            String::from_utf16_lossy(&text[..length])
        };
        assert_eq!(text(result.hwnd), "Picked color — Color Picker");
        assert_eq!(text(result.controls.swatch), "Selected color swatch");
        assert_eq!(text(result.controls.source), "Frozen screen  ·  X 0  Y 0");
        assert_eq!(text(result.controls.pick_again), "Pick again");
        assert_eq!(text(result.controls.close), "Close");
        assert_eq!(text(result.controls.minimize), "Minimize");
        assert_eq!(text(result.controls.caption_close), "Close window");
        assert_eq!(text(result.controls.default_copy), "Copy CSS RGB");
        let mut before = RECT::default();
        // SAFETY: The live window owner or callback retains this HWND. The initialized local
        // RECT is writable for exactly this synchronous query and is not retained.
        unsafe { GetWindowRect(result.hwnd, &mut before) }.unwrap();
        let css = target(ColorFormat::CssRgb, COPY_ROW + 2);
        assert_eq!(text(result.copy_button(css)), "Copy CSS RGB");
        result.show_copy_feedback(css).unwrap();
        assert_eq!(text(result.copy_button(css)), "Copied CSS RGB");
        // The owner-drawn row shows "Copied"; its native accessible name keeps
        // the format. Measure the real selected font in the existing footprint.
        let button = result.copy_button(css);
        // SAFETY: The fixture owns the button for this DC lease and later pairs it with
        // ReleaseDC using the same HWND; this is not a DeleteDC-owned memory DC.
        let dc = unsafe { GetDC(Some(button)) };
        assert!(!dc.is_invalid());
        // SAFETY: The helper uses an owned control and a documented scalar-only message;
        // synchronous processing retains no Rust pointer or mutable state borrow.
        let font = unsafe { SendMessageW(button, WM_GETFONT, None, None) };
        // SAFETY: WM_GETFONT returned a font borrowed from this live button; the fixture
        // retains its owner while the test temporarily selects and then restores it.
        let old = unsafe { SelectObject(dc, HGDIOBJ(font.0 as *mut _)) };
        let mut size = windows::Win32::Foundation::SIZE::default();
        let mut bounds = RECT::default();
        // SAFETY: The test keeps its window DC, borrowed font, and UTF-16 text live; SIZE and
        // RECT are local outputs. It restores the prior font before ReleaseDC.
        unsafe {
            GetClientRect(button, &mut bounds).unwrap();
            assert!(
                GetTextExtentPoint32W(dc, &"Copied".encode_utf16().collect::<Vec<_>>(), &mut size,)
                    .as_bool()
            );
            SelectObject(dc, old);
            ReleaseDC(Some(button), dc);
        }
        assert!(size.cx <= bounds.right - dip(8, result.dpi().unwrap()));
        assert!(size.cy <= bounds.bottom - dip(8, result.dpi().unwrap()));
        let mut after = RECT::default();
        // SAFETY: The live window owner or callback retains this HWND. The initialized local
        // RECT is writable for exactly this synchronous query and is not retained.
        unsafe { GetWindowRect(result.hwnd, &mut after) }.unwrap();
        assert_eq!(before, after);
        assert!(!result.status_visible.get());
        // SAFETY: The test retains the owning result/control tree while querying visibility;
        // this scalar query neither activates nor changes that window.
        assert!(!unsafe { IsWindowVisible(result.hwnd) }.as_bool());
        result.clear_copy_feedback().unwrap();
        assert_eq!(text(button), "Copy CSS RGB");
        drop(result);
        set_language(previous_language);
    }
}
