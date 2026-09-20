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
            Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute},
            Gdi::*,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            SystemServices::{SS_NOPREFIX, SS_OWNERDRAW},
        },
        UI::{
            Controls::DRAWITEMSTRUCT,
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
            Input::KeyboardAndMouse::SetFocus,
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
const RETRY_MS: u32 = 50;
const MAX_RETRIES: u8 = 3;
const CLIENT_WIDTH: i32 = 500;
const CLIENT_HEIGHT: i32 = 416;
const STYLE: WINDOW_STYLE =
    WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);
const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_APPWINDOW.0 | WS_EX_CONTROLPARENT.0);
const PANELS: [RECT; 5] = [
    RECT {
        left: 24,
        top: 24,
        right: 476,
        bottom: 120,
    },
    RECT {
        left: 24,
        top: 136,
        right: 476,
        bottom: 174,
    },
    RECT {
        left: 24,
        top: 180,
        right: 476,
        bottom: 218,
    },
    RECT {
        left: 24,
        top: 224,
        right: 476,
        bottom: 262,
    },
    RECT {
        left: 24,
        top: 268,
        right: 476,
        bottom: 306,
    },
];
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
}

struct CallbackState {
    notify_hwnd: HWND,
    default_format: ColorFormat,
    theme: Theme,
    swatch_color: COLORREF,
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
    hero_caption: HWND,
    hero_hex: HWND,
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
            self.hero_caption,
            self.hero_hex,
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
    _hero_font: Option<Font>,
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
            _thread_affinity: PhantomData,
        };
        theme::configure_window(hwnd);
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
            WINDOW_STYLE(SS_OWNERDRAW.0),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.hero_caption = self.control(
            w!("STATIC"),
            "已选颜色",
            13,
            WINDOW_STYLE(SS_NOPREFIX.0),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.hero_hex = self.control(
            w!("STATIC"),
            &format_color(self.picked.rgb, ColorFormat::Hex),
            14,
            WINDOW_STYLE(SS_NOPREFIX.0),
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
            "点击复制，或选中文本按 Ctrl+C。",
            12,
            WINDOW_STYLE(SS_NOPREFIX.0),
            WINDOW_EX_STYLE::default(),
        )?;
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
        // Captioned windows have a system-enforced minimum size. At a monitor
        // edge the initial rectangle can belong to the neighbor. Move inside
        // the source monitor while hidden before querying the actual window DPI.
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
        let mut outer = RECT {
            right: dip(CLIENT_WIDTH, dpi),
            bottom: dip(CLIENT_HEIGHT, dpi),
            ..Default::default()
        };
        unsafe { AdjustWindowRectExForDpi(&mut outer, STYLE, false, EX_STYLE, dpi)? };
        let width = outer.right - outer.left;
        let height = outer.bottom - outer.top;
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
                SWP_NOZORDER | SWP_NOACTIVATE,
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
        place(self.controls.swatch, 40, 40, 64, 64)?;
        place(self.controls.hero_caption, 128, 38, 328, 17)?;
        place(self.controls.hero_hex, 126, 54, 330, 35)?;
        place(self.controls.source, 128, 91, 328, 18)?;
        for (index, row) in self.controls.rows.iter().enumerate() {
            let y = 136 + index as i32 * 44;
            place(row.label, 40, y + 11, 68, 18)?;
            place(row.edit, 118, y + 9, 264, 22)?;
            place(row.copy, 394, y + 6, 68, 26)?;
        }
        place(self.controls.default_copy, 24, 320, 192, 38)?;
        place(self.controls.pick_again, 228, 320, 146, 38)?;
        place(self.controls.close, 386, 320, 90, 38)?;
        place(self.controls.status, 24, 370, 452, 30)?;
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        Ok(())
    }

    fn update_resources(&self, dpi: u32) -> Result<()> {
        let font = Font::new(14, dpi, 400, false)?;
        let small_font = Font::new(12, dpi, 400, false)?;
        let value_font = Font::new(14, dpi, 400, true)?;
        let hero_font = Font::new(28, dpi, 600, true)?;
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
        for hwnd in [
            self.controls.hero_caption,
            self.controls.source,
            self.controls.status,
        ] {
            set_font(hwnd, &small_font);
        }
        for row in self.controls.rows {
            set_font(row.label, &small_font);
            set_font(row.edit, &value_font);
        }
        set_font(self.controls.hero_hex, &hero_font);
        // All controls now borrow the new handles, so previous DPI fonts can
        // be released without retaining a RefCell borrow across window messages.
        let old = self.resources.replace(Resources {
            dpi,
            _font: Some(font),
            _small_font: Some(small_font),
            _value_font: Some(value_font),
            _hero_font: Some(hero_font),
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
        unsafe { SetWindowTextW(self.controls.status, PCWSTR(text.as_ptr())) }
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

fn draw_swatch(lparam: LPARAM, color: COLORREF) -> LRESULT {
    let Some(draw) = (unsafe { (lparam.0 as *const DRAWITEMSTRUCT).as_ref() }) else {
        return LRESULT(0);
    };
    let hdc = draw.hDC;
    let saved = unsafe { SaveDC(hdc) };
    if saved == 0 {
        return LRESULT(0);
    }
    let dpi = unsafe { GetDpiForWindow(draw.hwndItem) }.max(96);
    unsafe {
        FillRect(hdc, &draw.rcItem, HBRUSH(GetStockObject(WHITE_BRUSH).0));
        SelectObject(hdc, GetStockObject(DC_BRUSH));
        SelectObject(hdc, GetStockObject(DC_PEN));
        SetDCBrushColor(hdc, color);
        SetDCPenColor(hdc, COLORREF(0x00f0e8e2));
        let _ = RoundRect(
            hdc,
            draw.rcItem.left,
            draw.rcItem.top,
            draw.rcItem.right,
            draw.rcItem.bottom,
            dip(12, dpi),
            dip(12, dpi),
        );
        let _ = RestoreDC(hdc, saved);
    }
    LRESULT(1)
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
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => state.theme.paint(hwnd, &PANELS),
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            let id = unsafe { GetDlgCtrlID(HWND(lparam.0 as *mut _)) } as usize;
            let panel = matches!(id, 10 | 11 | 13 | 14)
                || (20..24).contains(&id)
                || (30..34).contains(&id)
                || (COPY_ROW..COPY_ROW + 4).contains(&id);
            let tone = if id == 12 {
                state.status_tone.get()
            } else if matches!(id, 11 | 13) || (20..24).contains(&id) {
                Tone::Muted
            } else {
                Tone::Text
            };
            state
                .theme
                .control_color(HDC(wparam.0 as *mut _), panel, tone)
        }
        WM_NOTIFY => theme::custom_draw(lparam, COPY_DEFAULT)
            .unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }),
        WM_DRAWITEM if wparam.0 == 10 => draw_swatch(lparam, state.swatch_color),
        WM_CLOSE => {
            state.queue(|pending| pending.action = Some(ResultAction::Close));
            LRESULT(0)
        }
        WM_COMMAND if (wparam.0 >> 16) as u32 == BN_CLICKED => {
            match wparam.0 & 0xffff {
                CLOSE => state.queue(|pending| pending.action = Some(ResultAction::Close)),
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
            state.queue(|pending| pending.layout = true);
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
