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
            E_FAIL, ERROR_CLASS_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HWND, LPARAM, LRESULT,
            POINT, RECT, SetLastError, WPARAM,
        },
        Graphics::{
            Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute},
            Gdi::*,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            SystemServices::{SS_BITMAP, SS_CENTERIMAGE, SS_NOPREFIX},
        },
        UI::{
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
            Input::KeyboardAndMouse::SetFocus,
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, Error, PCWSTR, Result, w},
};

use super::drawing::dip;
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
const CLIENT_WIDTH: i32 = 624;
const CLIENT_HEIGHT: i32 = 366;
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0);
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
}

struct CallbackState {
    notify_hwnd: HWND,
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
    _bitmap: Option<Bitmap>,
}

struct Font(HFONT);
impl Drop for Font {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
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
            notify_hwnd,
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
            "",
            10,
            WINDOW_STYLE(SS_BITMAP.0 | SS_CENTERIMAGE.0),
            WINDOW_EX_STYLE::default(),
        )?;
        let source = match self.picked.kind {
            SampleKind::Live => "实时屏幕",
            SampleKind::Frozen => "冻结画面",
        };
        self.controls.source = self.control(
            w!("STATIC"),
            &format!(
                "来源：{source}\r\n原始坐标：X {}，Y {}",
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
                    WS_EX_CLIENTEDGE,
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
            "复制默认格式（HEX）",
            COPY_DEFAULT,
            WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.pick_again = self.control(
            w!("BUTTON"),
            "重新取色(&P)",
            PICK_AGAIN,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.close = self.control(
            w!("BUTTON"),
            "关闭(&C)",
            CLOSE,
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            WINDOW_EX_STYLE::default(),
        )?;
        self.controls.status = self.control(
            w!("STATIC"),
            "点击复制按钮，或选中文本后按 Ctrl+C。",
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
        place(self.controls.swatch, 20, 20, 56, 56)?;
        place(self.controls.source, 92, 24, 510, 48)?;
        for (index, row) in self.controls.rows.iter().enumerate() {
            let y = 98 + index as i32 * 42;
            place(row.label, 20, y + 6, 76, 26)?;
            place(row.edit, 100, y, 374, 30)?;
            place(row.copy, 488, y, 116, 30)?;
        }
        place(self.controls.default_copy, 20, 278, 184, 34)?;
        place(self.controls.pick_again, 218, 278, 150, 34)?;
        place(self.controls.close, 382, 278, 104, 34)?;
        place(self.controls.status, 20, 326, 584, 32)?;
        Ok(())
    }

    fn update_resources(&self, dpi: u32) -> Result<()> {
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
            return Err(Error::new(E_FAIL, "Could not create result font"));
        }
        let size = dip(56, dpi);
        let rgb = self.picked.rgb;
        // Zero alpha avoids the STATIC control making an alpha-converted copy.
        let pixel = u32::from(rgb.b) | (u32::from(rgb.g) << 8) | (u32::from(rgb.r) << 16);
        let pixels = vec![pixel; size as usize * size as usize];
        let bitmap =
            Bitmap(unsafe { CreateBitmap(size, size, 1, 32, Some(pixels.as_ptr().cast())) });
        if bitmap.0.0.is_null() {
            return Err(Error::new(E_FAIL, "Could not create result swatch"));
        }
        for hwnd in self.controls.handles() {
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_SETFONT,
                    Some(WPARAM(font.0.0 as usize)),
                    Some(LPARAM(1)),
                )
            };
        }
        unsafe {
            SendMessageW(
                self.controls.swatch,
                STM_SETIMAGE,
                Some(WPARAM(IMAGE_BITMAP.0 as usize)),
                Some(LPARAM(bitmap.0.0 as isize)),
            );
        }
        let old = self.resources.replace(Resources {
            dpi,
            _font: Some(font),
            _bitmap: Some(bitmap),
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

    fn status(&self, text: &str) -> Result<()> {
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
                self.status(&format!("已复制 {}：{text}", request.format.label()))
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
                        return self.status("剪贴板被占用，无法安排重试。请再次点击复制。");
                    }
                    self.callback.active_timer.set(timer);
                    self.status("剪贴板被占用，正在重试…")
                } else {
                    self.status("复制失败：剪贴板仍被占用。请再次点击复制。")
                }
            }
            Err(ClipboardError::Other(error)) => {
                self.copy.borrow_mut().0 = None;
                self.status(&format!("复制失败：{error}"))
            }
        }
    }
}

impl Drop for ResultWindow {
    fn drop(&mut self) {
        self.callback.closing.set(true);
        self.cancel_copy();
        // Child controls release their borrowed font/bitmap before these fields
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
            state.queue(|pending| pending.action = Some(ResultAction::Close));
            LRESULT(0)
        }
        WM_COMMAND if (wparam.0 >> 16) as u32 == BN_CLICKED => {
            match wparam.0 & 0xffff {
                CLOSE => state.queue(|pending| pending.action = Some(ResultAction::Close)),
                PICK_AGAIN => state.queue(|pending| pending.action = Some(ResultAction::PickAgain)),
                COPY_DEFAULT => state.queue(|pending| pending.copy = Some(ColorFormat::Hex)),
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
