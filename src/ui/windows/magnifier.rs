//! Fixed-window, event-driven magnification of one immutable screen snapshot.

use std::{
    cell::RefCell,
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

use windows::{
    Win32::{
        Foundation::{
            COLORREF, E_FAIL, E_INVALIDARG, E_OUTOFMEMORY, ERROR_CLASS_ALREADY_EXISTS,
            ERROR_SUCCESS, GetLastError, HWND, LPARAM, LRESULT, RECT, SetLastError, WPARAM,
        },
        Graphics::{
            Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute},
            Gdi::*,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    },
    core::{BOOL, Error, Result, w},
};

use super::drawing::{PaintSession, dip};
use crate::{
    app::diagnostics,
    core::{
        format::{ColorFormat, format_color},
        geometry::{ScreenPointPx, ScreenRectPx},
        state::{PickedColor, SampleKind},
        zoom::{CachePoint, FrozenImage, SourcePixel, ZoomScale, ZoomView},
    },
};

const CLASS_NAME: windows::core::PCWSTR = w!("ColorPicker.Magnifier.v1");

#[derive(Default)]
struct State {
    view: Option<ZoomView>,
    surface: Option<Surface>,
    bounds: Option<ScreenRectPx>,
    visible: bool,
    hover: Option<SourcePixel>,
    lines: [Vec<u16>; 2],
    paint_error: Option<Error>,
    layout_invalidated: bool,
}

impl State {
    fn refresh_text(&mut self) {
        let factor = self.view.as_ref().map_or(4, |view| view.scale().factor());
        let detail = self.hover.map_or_else(
            || "移动到像素格内选择".to_owned(),
            |pixel| {
                format!(
                    "{}  X: {}  Y: {}",
                    format_color(pixel.rgb, ColorFormat::Hex),
                    pixel.source.x,
                    pixel.source.y
                )
            },
        );
        self.lines = [
            detail.encode_utf16().collect(),
            format!("{factor}×  滚轮缩放 · Esc / 右键取消")
                .encode_utf16()
                .collect(),
        ];
    }

    fn check(&mut self) -> Result<()> {
        if let Some(error) = self.paint_error.take() {
            return Err(error);
        }
        if self.layout_invalidated {
            return Err(failure("冻结期间显示缩放发生变化，请重新取色"));
        }
        Ok(())
    }
}

/// All handles and the borrowed callback allocation belong to the UI thread.
/// No sampling or timers are performed by this window.
pub struct MagnifierWindow {
    hwnd: HWND,
    state: Box<RefCell<State>>,
    _thread: PhantomData<Rc<()>>,
}

impl MagnifierWindow {
    pub fn new(
        mut image: FrozenImage,
        focus: ScreenPointPx,
        work_area: ScreenRectPx,
    ) -> Result<Self> {
        image
            .validate()
            .map_err(|_| Error::new(E_INVALIDARG, "Invalid frozen image"))?;
        if image.width > 65 || image.height > 65 || work_area.is_empty() {
            return Err(Error::new(
                E_INVALIDARG,
                "Frozen image/work area is outside the local capture limits",
            ));
        }
        let cache = CachePoint {
            x: u32::try_from(i64::from(focus.x) - i64::from(image.origin.x))
                .map_err(|_| Error::new(E_INVALIDARG, "Freeze focus is outside the snapshot"))?,
            y: u32::try_from(i64::from(focus.y) - i64::from(image.origin.y))
                .map_err(|_| Error::new(E_INVALIDARG, "Freeze focus is outside the snapshot"))?,
        };
        if cache.x >= image.width || cache.y >= image.height {
            return Err(Error::new(
                E_INVALIDARG,
                "Freeze focus is outside the snapshot",
            ));
        }
        // A 32-bit BI_RGB DIB uses width*4 bytes per row. Preserve RGB values
        // while removing optional caller padding once, before owning the view.
        compact_rows(&mut image)?;
        let instance = unsafe { GetModuleHandleW(None)? }.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0
            && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
        {
            return Err(Error::from_thread());
        }
        let state = Box::new(RefCell::new(State::default()));
        let pointer = state.as_ref() as *const RefCell<State>;
        // Creating the hidden one-pixel window on the target monitor establishes
        // its actual DPI before any DIP-sized layout is tested against work area.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                CLASS_NAME,
                w!("color-picker magnifier"),
                WS_POPUP,
                focus.x.clamp(work_area.left, work_area.right - 1),
                focus.y.clamp(work_area.top, work_area.bottom - 1),
                1,
                1,
                None,
                None,
                Some(instance),
                Some(pointer.cast()),
            )?
        };
        let window = Self {
            hwnd,
            state,
            _thread: PhantomData,
        };
        unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)? };
        let disabled = BOOL::from(true);
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&disabled as *const BOOL).cast(),
                std::mem::size_of::<BOOL>() as u32,
            )?;
        }
        if let Err(error) = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
            diagnostics::event(format_args!("magnifier.capture_exclusion_failed {error}"));
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let (bounds, viewport) = window_layout(focus, work_area, dpi)?;
        let view = ZoomView::new(image, viewport, ZoomScale::X4, cache)
            .map_err(|_| Error::new(E_INVALIDARG, "Could not map the frozen viewport"))?;
        let surface = Surface::new(bounds.width() as i32, bounds.height() as i32, dpi)?;
        {
            let mut state = window.state.borrow_mut();
            state.hover = view.hit_test(focus);
            state.view = Some(view);
            state.surface = Some(surface);
            state.bounds = Some(bounds);
            state.refresh_text();
        }
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                bounds.left,
                bounds.top,
                bounds.width() as i32,
                bounds.height() as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
        }
        window.state.borrow_mut().visible = true;
        window.invalidate()?;
        // Frozen has no periodic timer to discover a failed initial paint. Draw
        // this first frame synchronously, with no state borrow across reentry.
        if !unsafe { UpdateWindow(hwnd) }.as_bool() {
            return Err(failure("Could not present the initial frozen frame"));
        }
        window.state.borrow_mut().check()?;
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn rect(&self) -> Option<ScreenRectPx> {
        let state = self.state.borrow();
        if state.visible { state.bounds } else { None }
    }

    pub fn hide(&self) {
        let visible = {
            let mut state = self.state.borrow_mut();
            std::mem::replace(&mut state.visible, false)
        };
        if visible {
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    pub fn scale_factor(&self) -> u32 {
        self.state
            .borrow()
            .view
            .as_ref()
            .map_or(4, |view| view.scale().factor())
    }

    pub fn hit_test(&self, point: ScreenPointPx) -> Option<PickedColor> {
        let state = self.state.borrow();
        if !state.visible || state.layout_invalidated || state.paint_error.is_some() {
            return None;
        }
        let pixel = state.view.as_ref()?.hit_test(point)?;
        Some(PickedColor {
            rgb: pixel.rgb,
            source: pixel.source,
            kind: SampleKind::Frozen,
        })
    }

    pub fn update_hover(&self, point: ScreenPointPx) -> Result<bool> {
        let changed = {
            let mut state = self.state.borrow_mut();
            state.check()?;
            let hover = state.view.as_mut().and_then(|view| view.select_at(point));
            if state.hover == hover {
                false
            } else {
                state.hover = hover;
                state.refresh_text();
                true
            }
        };
        if changed {
            self.invalidate()?;
        }
        Ok(changed)
    }

    /// false means down from 4×: the controller must discard this window and
    /// snapshot and resume Live with fresh sampling resources.
    pub fn change_scale(&self, up: bool, point: ScreenPointPx) -> Result<bool> {
        let changed = {
            let mut state = self.state.borrow_mut();
            state.check()?;
            let view = state
                .view
                .as_mut()
                .ok_or_else(|| failure("Missing frozen view"))?;
            let previous = view.scale();
            let next = if up {
                previous.increase()
            } else {
                let Some(next) = previous.decrease() else {
                    return Ok(false);
                };
                next
            };
            view.change_scale(next, point);
            let hover = view.hit_test(point);
            let changed = previous != next || state.hover != hover;
            state.hover = hover;
            if changed {
                state.refresh_text();
            }
            changed
        };
        if changed {
            self.invalidate()?;
        }
        Ok(true)
    }

    fn invalidate(&self) -> Result<()> {
        if unsafe { InvalidateRect(Some(self.hwnd), None, false) }.as_bool() {
            Ok(())
        } else {
            Err(failure("Could not invalidate the frozen magnifier"))
        }
    }
}

impl Drop for MagnifierWindow {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyWindow(self.hwnd) } {
            unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
            diagnostics::event(format_args!("magnifier.destroy_failed {error}"));
        }
    }
}

fn window_layout(
    focus: ScreenPointPx,
    work: ScreenRectPx,
    dpi: u32,
) -> Result<(ScreenRectPx, ScreenRectPx)> {
    if dpi == 0 || dpi > 9600 || work.is_empty() {
        return Err(failure("Invalid magnifier DPI/work area"));
    }
    let padding = dip(8, dpi);
    let footer = dip(54, dpi);
    let available_width = i64::from(work.width()) - i64::from(2 * padding);
    let available_height = i64::from(work.height()) - i64::from(2 * padding + footer);
    let side = i64::from(dip(320, dpi))
        .min(available_width)
        .min(available_height);
    if side < 32 {
        return Err(failure("工作区空间不足，无法显示完整像素格"));
    }
    let width = side + i64::from(2 * padding);
    let height = side + i64::from(2 * padding + footer);
    let left = (i64::from(focus.x) - side / 2 - i64::from(padding))
        .clamp(i64::from(work.left), i64::from(work.right) - width);
    let top = (i64::from(focus.y) - side / 2 - i64::from(padding))
        .clamp(i64::from(work.top), i64::from(work.bottom) - height);
    let bounds = ScreenRectPx {
        left: left as i32,
        top: top as i32,
        right: (left + width) as i32,
        bottom: (top + height) as i32,
    };
    let viewport = ScreenRectPx {
        left: (left + i64::from(padding)) as i32,
        top: (top + i64::from(padding)) as i32,
        right: (left + i64::from(padding) + side) as i32,
        bottom: (top + i64::from(padding) + side) as i32,
    };
    Ok((bounds, viewport))
}

fn compact_rows(image: &mut FrozenImage) -> Result<()> {
    let row = image.width as usize * 4;
    if image.stride_bytes == row {
        return Ok(());
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(row * image.height as usize)
        .map_err(|_| Error::new(E_OUTOFMEMORY, "Could not normalize frozen pixel rows"))?;
    for y in 0..image.height as usize {
        pixels.extend_from_slice(&image.bgrx[y * image.stride_bytes..y * image.stride_bytes + row]);
    }
    image.bgrx = pixels;
    image.stride_bytes = row;
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        window_message(hwnd, message, wparam, lparam)
    }))
    .unwrap_or_else(|_| std::process::abort())
}

fn window_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        let creation = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe { SetLastError(ERROR_SUCCESS) };
        let previous =
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, creation.lpCreateParams as isize) };
        return LRESULT(
            i32::from(previous != 0 || unsafe { GetLastError() } == ERROR_SUCCESS) as isize,
        );
    }
    if message == WM_NCDESTROY {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<State>;
    if !pointer.is_null() {
        let state = unsafe { &*pointer };
        match message {
            WM_PAINT => {
                let paint = PaintSession::begin(hwnd);
                if let Ok(mut state) = state.try_borrow_mut()
                    && let (Some(surface), Some(view), Some(bounds)) =
                        (&state.surface, &state.view, state.bounds)
                    && let Err(error) = surface.draw(paint.dc, view, bounds, &state.lines)
                {
                    state.paint_error = Some(error);
                }
                return LRESULT(0);
            }
            WM_DPICHANGED => {
                if let Ok(mut state) = state.try_borrow_mut() {
                    state.layout_invalidated = true;
                }
                return LRESULT(0);
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

struct MemoryDc(HDC);
impl Drop for MemoryDc {
    fn drop(&mut self) {
        let _ = unsafe { DeleteDC(self.0) };
    }
}
struct OwnedBitmap(HBITMAP);
impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(self.0.into()) };
    }
}
struct OwnedFont(HFONT);
impl Drop for OwnedFont {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(self.0.into()) };
    }
}
struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        let _ = unsafe { ReleaseDC(None, self.0) };
    }
}

struct Surface {
    dc: MemoryDc,
    _bitmap: OwnedBitmap,
    _font: OwnedFont,
    old_bitmap: HGDIOBJ,
    old_font: HGDIOBJ,
    width: i32,
    height: i32,
    dpi: u32,
}

impl Surface {
    fn new(width: i32, height: i32, dpi: u32) -> Result<Self> {
        let screen = ScreenDc(unsafe { GetDC(None) });
        if screen.0.is_invalid() {
            return Err(failure("Could not obtain drawing DC"));
        }
        let dc = MemoryDc(unsafe { CreateCompatibleDC(Some(screen.0)) });
        if dc.0.is_invalid() {
            return Err(failure("Could not create magnifier DC"));
        }
        let bitmap = OwnedBitmap(unsafe { CreateCompatibleBitmap(screen.0, width, height) });
        if bitmap.0.is_invalid() {
            return Err(failure("Could not create magnifier buffer"));
        }
        let font = OwnedFont(unsafe {
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
        if font.0.is_invalid() {
            return Err(failure("Could not create magnifier font"));
        }
        let old_bitmap = unsafe { SelectObject(dc.0, bitmap.0.into()) };
        if old_bitmap.is_invalid() {
            return Err(failure("Could not select magnifier bitmap"));
        }
        let old_font = unsafe { SelectObject(dc.0, font.0.into()) };
        if old_font.is_invalid() {
            let _ = unsafe { SelectObject(dc.0, old_bitmap) };
            return Err(failure("Could not select magnifier font"));
        }
        Ok(Self {
            dc,
            _bitmap: bitmap,
            _font: font,
            old_bitmap,
            old_font,
            width,
            height,
            dpi,
        })
    }

    fn draw(
        &self,
        target: HDC,
        view: &ZoomView,
        bounds: ScreenRectPx,
        lines: &[Vec<u16>; 2],
    ) -> Result<()> {
        self.fill(
            RECT {
                left: 0,
                top: 0,
                right: self.width,
                bottom: self.height,
            },
            COLORREF(0x00202020),
        )?;
        let image = view.image();
        let source = view.source_view();
        let drawn = local_rect(view.drawn_rect(), bounds);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: image.width as i32,
                biHeight: -(image.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (image.width * image.height * 4),
                ..Default::default()
            },
            ..Default::default()
        };
        if unsafe { SetStretchBltMode(self.dc.0, COLORONCOLOR) } == 0 {
            return Err(failure(
                "Could not select nearest-neighbor magnifier drawing",
            ));
        }
        if unsafe {
            StretchDIBits(
                self.dc.0,
                drawn.left,
                drawn.top,
                drawn.right - drawn.left,
                drawn.bottom - drawn.top,
                source.x as i32,
                source.y as i32,
                source.width as i32,
                source.height as i32,
                Some(image.bgrx.as_ptr().cast()),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        } <= 0
        {
            return Err(failure("Could not draw the frozen pixels"));
        }
        let scale = view.scale().factor() as i32;
        if scale >= 16 {
            for x in (drawn.left..drawn.right).step_by(scale as usize) {
                self.fill(
                    RECT {
                        left: x,
                        top: drawn.top,
                        right: x + 1,
                        bottom: drawn.bottom,
                    },
                    COLORREF(0x00505050),
                )?;
            }
            for y in (drawn.top..drawn.bottom).step_by(scale as usize) {
                self.fill(
                    RECT {
                        left: drawn.left,
                        top: y,
                        right: drawn.right,
                        bottom: y + 1,
                    },
                    COLORREF(0x00505050),
                )?;
            }
        }
        if let Some(center) = view.cell_center(view.selected()) {
            let left = center.x - bounds.left - scale / 2;
            let top = center.y - bounds.top - scale / 2;
            self.frame(
                RECT {
                    left: left - 1,
                    top: top - 1,
                    right: left + scale + 1,
                    bottom: top + scale + 1,
                },
                COLORREF(0),
            )?;
            self.frame(
                RECT {
                    left,
                    top,
                    right: left + scale,
                    bottom: top + scale,
                },
                COLORREF(0x00ffffff),
            )?;
        }
        if unsafe { SetBkMode(self.dc.0, TRANSPARENT) } == 0
            || unsafe { SetTextColor(self.dc.0, COLORREF(0x00ffffff)) }.0 == CLR_INVALID
        {
            return Err(failure("Could not configure magnifier text"));
        }
        let footer_top = view.viewport().bottom - bounds.top + dip(8, self.dpi);
        for (index, text) in lines.iter().enumerate() {
            if !unsafe {
                TextOutW(
                    self.dc.0,
                    dip(8, self.dpi),
                    footer_top + dip(index as i32 * 22, self.dpi),
                    text,
                )
            }
            .as_bool()
            {
                return Err(failure("Could not draw magnifier text"));
            }
        }
        unsafe {
            BitBlt(
                target,
                0,
                0,
                self.width,
                self.height,
                Some(self.dc.0),
                0,
                0,
                SRCCOPY,
            )
        }
    }

    fn brush(&self, color: COLORREF) -> Result<HBRUSH> {
        let brush = HBRUSH(unsafe { GetStockObject(DC_BRUSH) }.0);
        if brush.is_invalid() || unsafe { SetDCBrushColor(self.dc.0, color) }.0 == CLR_INVALID {
            return Err(failure("Could not configure magnifier drawing brush"));
        }
        Ok(brush)
    }
    fn fill(&self, rect: RECT, color: COLORREF) -> Result<()> {
        if unsafe { FillRect(self.dc.0, &rect, self.brush(color)?) } == 0 {
            Err(failure("Could not fill magnifier buffer"))
        } else {
            Ok(())
        }
    }
    fn frame(&self, rect: RECT, color: COLORREF) -> Result<()> {
        if unsafe { FrameRect(self.dc.0, &rect, self.brush(color)?) } == 0 {
            Err(failure("Could not draw selected pixel border"))
        } else {
            Ok(())
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc.0, self.old_font);
            SelectObject(self.dc.0, self.old_bitmap);
        }
    }
}

fn local_rect(rect: ScreenRectPx, bounds: ScreenRectPx) -> RECT {
    RECT {
        left: rect.left - bounds.left,
        top: rect.top - bounds.top,
        right: rect.right - bounds.left,
        bottom: rect.bottom - bounds.top,
    }
}

fn failure(message: &'static str) -> Error {
    Error::new(E_FAIL, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_layout_uses_target_dpi_and_stays_inside_work_area() {
        let work = ScreenRectPx {
            left: -1200,
            top: -700,
            right: -200,
            bottom: 100,
        };
        for dpi in [96, 120, 144, 192] {
            let (window, viewport) =
                window_layout(ScreenPointPx { x: -1199, y: -699 }, work, dpi).unwrap();
            assert_eq!(window.intersection(work), Some(window));
            assert_eq!(viewport.width(), viewport.height());
            assert_eq!(viewport.width(), dip(320, dpi) as u32);
            assert!(viewport.width() >= 32);
            assert!(viewport.bottom < window.bottom);
        }
        assert!(
            window_layout(
                ScreenPointPx { x: 0, y: 0 },
                ScreenRectPx {
                    left: 0,
                    top: 0,
                    right: 16,
                    bottom: 16
                },
                96
            )
            .is_err()
        );
    }
}
