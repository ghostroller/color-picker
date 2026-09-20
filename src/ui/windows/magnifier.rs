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

use super::drawing::{OwnedFont, PaintSession, dip, draw_text, palette};
use crate::{
    app::diagnostics,
    core::{
        color::Rgb8,
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
    footer: Footer,
    paint_error: Option<Error>,
    layout_invalidated: bool,
}

#[derive(Default)]
struct Footer {
    rgb: Option<Rgb8>,
    hex: Vec<u16>,
    coordinates: Vec<u16>,
    help: Vec<u16>,
    scale: Vec<u16>,
}

impl State {
    fn refresh_text(&mut self) {
        let factor = self.view.as_ref().map_or(4, |view| view.scale().factor());
        let (hex, coordinates) = self.hover.map_or_else(
            || ("选择像素".to_owned(), "移动到像素格内".to_owned()),
            |pixel| {
                (
                    format_color(pixel.rgb, ColorFormat::Hex),
                    format!("X {}   Y {}", pixel.source.x, pixel.source.y),
                )
            },
        );
        self.footer = Footer {
            rgb: self.hover.map(|pixel| pixel.rgb),
            hex: hex.encode_utf16().collect(),
            coordinates: coordinates.encode_utf16().collect(),
            help: "左键取色 · 滚轮缩放 · Esc 取消".encode_utf16().collect(),
            scale: format!("{factor}×").encode_utf16().collect(),
        };
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
                    && let Err(error) = surface.draw(paint.dc, view, bounds, &state.footer)
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
struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        let _ = unsafe { ReleaseDC(None, self.0) };
    }
}

struct Surface {
    dc: MemoryDc,
    _bitmap: OwnedBitmap,
    heading_font: OwnedFont,
    body_font: OwnedFont,
    help_font: OwnedFont,
    old_bitmap: HGDIOBJ,
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
        let heading_font = OwnedFont::new(18, 600, dpi)?;
        let body_font = OwnedFont::new(12, 400, dpi)?;
        let help_font = OwnedFont::new(11, 400, dpi)?;
        let old_bitmap = unsafe { SelectObject(dc.0, bitmap.0.into()) };
        if old_bitmap.is_invalid() {
            return Err(failure("Could not select magnifier bitmap"));
        }
        Ok(Self {
            dc,
            _bitmap: bitmap,
            heading_font,
            body_font,
            help_font,
            old_bitmap,
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
        footer: &Footer,
    ) -> Result<()> {
        self.fill(
            RECT {
                left: 0,
                top: 0,
                right: self.width,
                bottom: self.height,
            },
            palette::BACKGROUND,
        )?;
        let image = view.image();
        let source = view.source_view();
        let drawn = local_rect(view.drawn_rect(), bounds);
        let viewport = local_rect(view.viewport(), bounds);
        // Borders stay outside the viewport. Snapshot scaling, the integer grid
        // and the contrasting selection frame below retain their exact geometry.
        self.frame(
            RECT {
                left: 0,
                top: 0,
                right: self.width,
                bottom: self.height,
            },
            palette::BORDER,
        )?;
        self.frame(
            RECT {
                left: viewport.left - 1,
                top: viewport.top - 1,
                right: viewport.right + 1,
                bottom: viewport.bottom + 1,
            },
            palette::BORDER,
        )?;
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
        self.draw_footer(viewport.bottom, footer)?;
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

    fn draw_footer(&self, top: i32, footer: &Footer) -> Result<()> {
        let pad = dip(12, self.dpi);
        self.fill(
            RECT {
                left: 1,
                top: top + dip(7, self.dpi),
                right: self.width - 1,
                bottom: self.height - 1,
            },
            palette::PANEL,
        )?;
        self.fill(
            RECT {
                left: pad,
                top: top + dip(7, self.dpi),
                right: self.width - pad,
                bottom: top + dip(8, self.dpi),
            },
            palette::BORDER,
        )?;
        let swatch = RECT {
            left: pad,
            top: top + dip(15, self.dpi),
            right: dip(48, self.dpi).min(self.width - pad),
            bottom: top + dip(51, self.dpi),
        };
        if swatch.right > swatch.left {
            self.fill(swatch, palette::SWATCH_BORDER)?;
            let inset = dip(2, self.dpi).max(1);
            let interior = RECT {
                left: swatch.left + inset,
                top: swatch.top + inset,
                right: swatch.right - inset,
                bottom: swatch.bottom - inset,
            };
            if interior.right > interior.left {
                self.fill(
                    interior,
                    footer.rgb.map_or(palette::EMPTY, |rgb| {
                        COLORREF(
                            u32::from(rgb.r) | (u32::from(rgb.g) << 8) | (u32::from(rgb.b) << 16),
                        )
                    }),
                )?;
            }
        }
        let right = self.width - pad;
        draw_text(
            self.dc.0,
            &self.heading_font,
            RECT {
                left: dip(60, self.dpi),
                top: top + dip(12, self.dpi),
                right: dip(152, self.dpi).min(right),
                bottom: top + dip(36, self.dpi),
            },
            palette::TEXT,
            &footer.hex,
        )?;
        draw_text(
            self.dc.0,
            &self.body_font,
            RECT {
                left: dip(160, self.dpi),
                top: top + dip(17, self.dpi),
                right,
                bottom: top + dip(35, self.dpi),
            },
            palette::SECONDARY,
            &footer.coordinates,
        )?;
        let badge_width = dip(38, self.dpi);
        let badge_left = (right - badge_width).max(dip(60, self.dpi));
        draw_text(
            self.dc.0,
            &self.help_font,
            RECT {
                left: dip(60, self.dpi),
                top: top + dip(39, self.dpi),
                right: badge_left - dip(6, self.dpi),
                bottom: top + dip(55, self.dpi),
            },
            palette::MUTED,
            &footer.help,
        )?;
        draw_text(
            self.dc.0,
            &self.body_font,
            RECT {
                left: badge_left,
                top: top + dip(38, self.dpi),
                right,
                bottom: top + dip(56, self.dpi),
            },
            palette::ACCENT,
            &footer.scale,
        )?;
        Ok(())
    }

    fn brush(&self, color: COLORREF) -> Result<HBRUSH> {
        let brush = HBRUSH(unsafe { GetStockObject(DC_BRUSH) }.0);
        if brush.is_invalid() || unsafe { SetDCBrushColor(self.dc.0, color) }.0 == CLR_INVALID {
            return Err(failure("Could not configure magnifier drawing brush"));
        }
        Ok(brush)
    }
    fn fill(&self, rect: RECT, color: COLORREF) -> Result<()> {
        // Very small work areas keep the existing minimum pixel viewport; its
        // footer may have no room for decoration or text.
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return Ok(());
        }
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
