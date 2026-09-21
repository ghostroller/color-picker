//! Native result controls. Window callbacks only queue intentions; the owner
//! calls `process_pending` after dispatch and owns the window until it is dropped.

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
            COLORREF, E_FAIL, ERROR_CLASS_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HWND,
            LPARAM, LRESULT, POINT, RECT, SetLastError, WPARAM,
        },
        Graphics::{
            Dwm::{DWMWA_BORDER_COLOR, DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute},
            Gdi::*,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            SystemServices::{SS_NOPREFIX, SS_OWNERDRAW},
        },
        UI::{
            Controls::{
                CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED,
                CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR,
            },
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::SetFocus,
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, Error, PCWSTR, Result, w},
};

use super::{
    drawing::{border_thickness, dip, draw_bottom_right_border},
    theme::{self, Font, Theme, Tone},
};
use crate::{
    app::config::AppearanceConfig,
    core::{
        format::{ColorFormat, format_color},
        state::{PickedColor, SampleKind},
    },
    platform::windows::clipboard::{Clipboard, ClipboardError},
};

pub const WM_RESULT_WAKE: u32 = WM_APP + 11;
const CLASS: PCWSTR = w!("ColorPicker.Result.v1");
const COPY_DEFAULT: usize = 1;
const CLOSE: usize = 2;
const PICK_AGAIN: usize = 200;
const COPY_ROW: usize = 100;
const CAPTION_MINIMIZE: usize = 40;
const CAPTION_CLOSE: usize = 41;
const RETRY_MS: u32 = 50;
const MAX_RETRIES: u8 = 3;
const CLIENT_WIDTH: i32 = 420;
const CLIENT_HEIGHT: i32 = 364;
const CLIENT_HEIGHT_WITH_STATUS: i32 = 390;
const SWATCH_HEIGHT: i32 = 112;
const ROW_TOP: i32 = 156;
const ROW_HEIGHT: i32 = 36;
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
    copy: Option<ColorFormat>,
    timer: Option<usize>,
    dpi_rect: Option<RECT>,
    layout: bool,
    default_style: bool,
    minimize: bool,
}

struct CallbackState {
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

#[derive(Clone, Copy)]
struct CopyRequest {
    token: usize,
    format: ColorFormat,
    retries_left: u8,
}

#[derive(Default)]
struct CopyState(Option<CopyRequest>);

impl CopyState {
    fn replace(&mut self, token: usize, format: ColorFormat) {
        self.0 = Some(CopyRequest {
            token,
            format,
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
    status_visible: Cell<bool>,
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
            default_id: Cell::new(COPY_DEFAULT),
        });
        let pointer = callback.as_ref() as *const CallbackState;
        let hwnd = unsafe {
            CreateWindowExW(
                EX_STYLE,
                CLASS,
                w!("取色结果 — Color Picker"),
                STYLE,
                picked.source.x,
                picked.source.y,
                1,
                1,
                Some(notify_hwnd),
                None,
                Some(instance),
                Some(pointer.cast()),
            )?
        };
        // From this point every error destroys all partially created children
        // before releasing the userdata allocation or selected GDI resources.
        let mut window = Self {
            hwnd,
            callback,
            controls: Controls::default(),
            picked,
            resources: RefCell::new(Resources::default()),
            copy: RefCell::new(CopyState::default()),
            status_visible: Cell::new(false),
            _thread_affinity: PhantomData,
        };
        theme::configure_window(hwnd, &window.callback.theme);
        // Keep the window's native title for taskbar/accessibility, while the
        // client area replaces the visible frame. DWM rounding is best effort.
        let no_border = 0xffff_fffe_u32; // DWMWA_COLOR_NONE
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
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&disable_transitions as *const BOOL).cast(),
                size_of::<BOOL>() as u32,
            )?;
        }
        window.create_controls()?;
        window.place_initially()?;
        window.layout()?;
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(window.controls.default_copy));
        }
        if auto_copy {
            // The host constructs this window only after capture/input cleanup.
            // Clipboard access stays in process_pending, just like button copies.
            window
                .callback
                .queue(|pending| pending.copy = Some(default_format));
        }
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
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
            return Ok(Some(action));
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
        if pending.minimize {
            let _ = unsafe { ShowWindow(self.hwnd, SW_MINIMIZE) };
        }
        if let Some(format) = pending.copy {
            self.cancel_copy();
            let token = next_copy_token()?;
            self.copy.borrow_mut().replace(token, format);
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
        Ok(None)
    }

    fn create_controls(&mut self) -> Result<()> {
        self.controls.swatch = self.control(
            w!("STATIC"),
            "已选颜色色块",
            10,
            WINDOW_STYLE(SS_OWNERDRAW.0) | WS_CLIPSIBLINGS,
            WINDOW_EX_STYLE::default(),
        )?;
        let source = match self.picked.kind {
            SampleKind::Live => "实时屏幕",
            SampleKind::Frozen => "冻结画面",
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
                    &format!("复制 {}", format.label()),
                    COPY_ROW + index,
                    WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                    WINDOW_EX_STYLE::default(),
                )?,
            };
        }
        self.controls.default_copy = self.control(
            w!("BUTTON"),
            &format!("复制 {}", self.callback.default_format.label()),
            COPY_DEFAULT,
            WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.pick_again = self.control(
            w!("BUTTON"),
            "重新取色",
            PICK_AGAIN,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.close = self.control(
            w!("BUTTON"),
            "关闭",
            CLOSE,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.status = self.control(
            w!("STATIC"),
            "",
            12,
            WINDOW_STYLE(SS_NOPREFIX.0),
            WINDOW_EX_STYLE::default(),
        )?;
        // Do not reserve an empty footer before a copy has produced feedback.
        let _ = unsafe { ShowWindow(self.controls.status, SW_HIDE) };
        self.controls.minimize = self.control(
            w!("BUTTON"),
            "最小化",
            CAPTION_MINIMIZE,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.caption_close = self.control(
            w!("BUTTON"),
            "关闭窗口",
            CAPTION_CLOSE,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        // The color STATIC overlaps the caption buttons. Keep it behind every
        // sibling, and clip its paint so hover/focus on the buttons stays visible.
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
        unsafe {
            CreateWindowExW(
                ex_style,
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

    fn dpi(&self) -> Result<u32> {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 || dpi > 9600 {
            Err(Error::new(E_FAIL, "Could not determine result-window DPI"))
        } else {
            Ok(dpi)
        }
    }

    fn place_initially(&self) -> Result<()> {
        // Move inside the source monitor while hidden before querying its DPI.
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
        // WM_NCCALCSIZE makes the whole window client space: do not add the
        // former native caption/border dimensions back into the layout.
        let width = dip(CLIENT_WIDTH, dpi);
        let height = dip(CLIENT_HEIGHT, dpi);
        let x = i64::from(self.picked.source.x).clamp(
            i64::from(work.left),
            (i64::from(work.right) - i64::from(width)).max(i64::from(work.left)),
        ) as i32;
        let y = i64::from(self.picked.source.y).clamp(
            i64::from(work.top),
            (i64::from(work.bottom) - i64::from(height)).max(i64::from(work.top)),
        ) as i32;
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

    fn layout(&self) -> Result<()> {
        let dpi = self.dpi()?;
        self.callback.theme.update_window_icons(self.hwnd);
        if self.resources.borrow().dpi != dpi {
            self.update_resources(dpi)?;
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
        // Leave the shared right border uncovered by this child window, even
        // across fractional DPI changes. The parent uses WS_CLIPCHILDREN.
        let mut client = RECT::default();
        unsafe {
            GetClientRect(self.hwnd, &mut client)?;
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
                dip(SWATCH_HEIGHT, dpi),
                true,
            )?;
        }
        for (id, button) in [
            (CAPTION_MINIMIZE, self.controls.minimize),
            (CAPTION_CLOSE, self.controls.caption_close),
        ] {
            let rect = caption_button_rect(id, client.right, dpi);
            unsafe {
                MoveWindow(
                    button,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    true,
                )?
            };
        }
        place(self.controls.source, 20, 128, 380, 18)?;
        for (index, row) in self.controls.rows.iter().enumerate() {
            let y = ROW_TOP + index as i32 * ROW_HEIGHT;
            place(row.label, 20, y + 9, 60, 18)?;
            place(row.edit, 88, y + 7, 252, 22)?;
            place(row.copy, 352, y + 4, 48, 28)?;
        }
        place(self.controls.default_copy, 20, 316, 160, 32)?;
        place(self.controls.pick_again, 196, 316, 126, 32)?;
        place(self.controls.close, 336, 316, 64, 32)?;
        place(self.controls.status, 20, 354, 380, 28)?;
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        Ok(())
    }

    fn update_resources(&self, dpi: u32) -> Result<()> {
        let font = Font::new(13, dpi, 400, false)?;
        let small_font = Font::new(11, dpi, 400, false)?;
        let value_font = Font::new(14, dpi, 400, true)?;
        let set_font = |hwnd, font: &Font| {
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_SETFONT,
                    Some(WPARAM(font.0.0 as usize)),
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
        unsafe { SetWindowTextW(self.controls.status, PCWSTR(text.as_ptr()))? };
        if !self.status_visible.get() {
            // Expand only once, keeping the existing two-line success/error
            // area. Clamp upwards at the work-area edge so feedback stays on
            // screen when the picked pixel was close to the taskbar.
            let dpi = self.dpi()?;
            let mut bounds = RECT::default();
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            unsafe {
                GetWindowRect(self.hwnd, &mut bounds)?;
                let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                    return Err(Error::from_thread());
                }
                let height = dip(CLIENT_HEIGHT_WITH_STATUS, dpi);
                let top = bounds
                    .top
                    .min(info.rcWork.bottom - height)
                    .max(info.rcWork.top);
                SetWindowPos(
                    self.hwnd,
                    None,
                    bounds.left,
                    top,
                    bounds.right - bounds.left,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )?;
                let _ = ShowWindow(self.controls.status, SW_SHOWNA);
            }
            self.status_visible.set(true);
            self.layout()?;
        }
        Ok(())
    }

    fn stop_timer(&self) {
        let timer = self.callback.active_timer.replace(0);
        if timer != 0 {
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
        let text = format_color(self.picked.rgb, request.format);
        match Clipboard::copy_text(self.hwnd, &text) {
            Ok(()) => {
                self.copy.borrow_mut().0 = None;
                self.status(
                    &format!("已复制 {}：{text}", request.format.label()),
                    Tone::Success,
                )
            }
            Err(ClipboardError::Busy) => {
                let retry = self.copy.borrow_mut().reserve_retry(token);
                if retry {
                    // Every arming has a fresh ID, including retries within one
                    // copy request. An already queued WM_TIMER cannot shorten
                    // the next 50 ms delay after KillTimer and rearming.
                    let timer = next_copy_token()?;
                    if unsafe { SetTimer(Some(self.hwnd), timer, RETRY_MS, None) } == 0 {
                        self.copy.borrow_mut().0 = None;
                        return self
                            .status("剪贴板被占用，无法安排重试。请再次点击复制。", Tone::Error);
                    }
                    self.callback.active_timer.set(timer);
                    self.status("剪贴板被占用，正在重试…", Tone::Muted)
                } else {
                    self.status("复制失败：剪贴板仍被占用。请再次点击复制。", Tone::Error)
                }
            }
            Err(ClipboardError::Other(error)) => {
                self.copy.borrow_mut().0 = None;
                self.status(&format!("复制失败：{error}"), Tone::Error)
            }
        }
    }
}

impl Drop for ResultWindow {
    fn drop(&mut self) {
        self.callback.closing.set(true);
        self.cancel_copy();
        // Child controls release their borrowed fonts before these fields
        // and callback userdata are dropped. Only the owner destroys the HWND.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
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

fn caption_hit_test(hwnd: HWND, lparam: LPARAM) -> LRESULT {
    // Screen coordinates can be negative on monitors left/above the primary.
    let mut point = POINT {
        x: (lparam.0 as u16 as i16) as i32,
        y: ((lparam.0 >> 16) as u16 as i16) as i32,
    };
    let mut client = RECT::default();
    if !unsafe { ScreenToClient(hwnd, &mut point) }.as_bool()
        || unsafe { GetClientRect(hwnd, &mut client) }.is_err()
    {
        return LRESULT(HTCLIENT as isize);
    }
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
    if inside(client) && point.y < dip(SWATCH_HEIGHT, dpi) {
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

fn draw_caption_button(lparam: LPARAM, background: COLORREF) -> Option<LRESULT> {
    if lparam.0 == 0 {
        return None;
    }
    let header = unsafe { &*(lparam.0 as *const NMHDR) };
    if header.code != NM_CUSTOMDRAW || ![CAPTION_MINIMIZE, CAPTION_CLOSE].contains(&header.idFrom) {
        return None;
    }
    let draw = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
    if draw.dwDrawStage != CDDS_PREPAINT {
        return None;
    }
    let hdc = draw.hdc;
    let saved = unsafe { SaveDC(hdc) };
    if saved == 0 {
        return None;
    }
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
    let pen = unsafe { CreatePen(PS_SOLID, dip(1, dpi).max(1), ink) };
    if pen.is_invalid() {
        let _ = unsafe { RestoreDC(hdc, saved) };
        return None;
    }
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
        SelectObject(hdc, HGDIOBJ(pen.0));
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
        let _ = RestoreDC(hdc, saved);
        let _ = DeleteObject(HGDIOBJ(pen.0));
    }
    Some(LRESULT(CDRF_SKIPDEFAULT as isize))
}

fn draw_swatch(lparam: LPARAM, color: COLORREF) -> LRESULT {
    let Some(draw) = (unsafe { (lparam.0 as *const DRAWITEMSTRUCT).as_ref() }) else {
        return LRESULT(0);
    };
    let hdc = draw.hDC;
    let saved = unsafe { SaveDC(hdc) };
    if saved == 0 {
        return LRESULT(0);
    }
    unsafe {
        SetDCBrushColor(hdc, color);
        FillRect(hdc, &draw.rcItem, HBRUSH(GetStockObject(DC_BRUSH).0));
        let _ = RestoreDC(hdc, saved);
    }
    LRESULT(1)
}

fn paint_result(hwnd: HWND, border_width_dip: u8) -> LRESULT {
    let mut paint = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
    if !hdc.is_invalid() {
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        unsafe { FillRect(hdc, &client, HBRUSH(GetStockObject(WHITE_BRUSH).0)) };
        let saved = unsafe { SaveDC(hdc) };
        if saved != 0 {
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            unsafe { SetDCBrushColor(hdc, COLORREF(0x00f0eeeb)) };
            // One continuous value list, separated only by quiet hairlines.
            for index in 1..=4 {
                let top = dip(ROW_TOP + index * ROW_HEIGHT, dpi);
                let line = RECT {
                    left: dip(20, dpi),
                    top,
                    right: client.right - dip(20, dpi),
                    bottom: top + 1,
                };
                unsafe { FillRect(hdc, &line, HBRUSH(GetStockObject(DC_BRUSH).0)) };
            }
            let _ =
                draw_bottom_right_border(hdc, client.right, client.bottom, dpi, border_width_dip);
            let _ = unsafe { RestoreDC(hdc, saved) };
        }
    }
    let _ = unsafe { EndPaint(hwnd, &paint) };
    LRESULT(0)
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
        WM_NCCALCSIZE => LRESULT(0),
        WM_NCHITTEST => caption_hit_test(hwnd, lparam),
        // A fixed-size results sheet has no maximize/resize mode, even when
        // invoked through the system menu or a double click on its color field.
        WM_NCLBUTTONDBLCLK if wparam.0 == HTCAPTION as usize => LRESULT(0),
        WM_SYSCOMMAND if matches!((wparam.0 & 0xfff0) as u32, SC_MAXIMIZE | SC_SIZE) => LRESULT(0),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => paint_result(hwnd, state.border_width_dip),
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
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
        WM_NOTIFY => draw_caption_button(lparam, state.swatch_color)
            .or_else(|| theme::custom_draw_minimal(lparam, COPY_DEFAULT))
            .unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }),
        WM_DRAWITEM if wparam.0 == 10 => draw_swatch(lparam, state.swatch_color),
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
                COPY_DEFAULT => state.queue(|pending| pending.copy = Some(state.default_format)),
                id if (COPY_ROW..COPY_ROW + 4).contains(&id) => {
                    state.queue(|pending| pending.copy = Some(ColorFormat::ALL[id - COPY_ROW]))
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 != 0 && state.active_timer.get() == wparam.0 => {
            state.queue(|pending| pending.timer = Some(wparam.0));
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
            if wparam.0 != SIZE_MINIMIZED as usize {
                state.queue(|pending| pending.layout = true);
            }
            LRESULT(0)
        }
        DM_GETDEFID => LRESULT(((DC_HASDEFID as usize) << 16 | state.default_id.get()) as isize),
        DM_SETDEFID => {
            state.default_id.set(wparam.0);
            state.queue(|pending| pending.default_style = true);
            LRESULT(1)
        }
        WM_NCDESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        result.status("已复制 HEX：#F4F2F2", Tone::Success).unwrap();
        let mut repeated = RECT::default();
        unsafe { GetWindowRect(result.hwnd, &mut repeated) }.unwrap();
        assert_eq!(
            repeated, after,
            "later copies must not grow the window again"
        );
    }

    #[test]
    fn clipboard_contention_has_three_retries_and_then_goes_idle() {
        let mut copy = CopyState::default();
        copy.replace(1, ColorFormat::Hex);
        for _ in 0..3 {
            assert!(copy.reserve_retry(1));
        }
        assert!(!copy.reserve_retry(1));
        assert!(copy.0.is_none());
    }

    #[test]
    fn replacing_or_canceling_copy_rejects_queued_old_timer() {
        let mut copy = CopyState::default();
        copy.replace(1, ColorFormat::Hex);
        assert!(copy.reserve_retry(1));
        copy.replace(2, ColorFormat::Hsl);
        assert!(copy.matching(1).is_none());
        assert!(!copy.reserve_retry(1));
        assert_eq!(copy.matching(2).unwrap().retries_left, 3);
        assert_eq!(copy.matching(2).unwrap().format, ColorFormat::Hsl);
        copy.0 = None;
        assert!(copy.matching(2).is_none());
        assert!(!copy.reserve_retry(2));
    }
}
