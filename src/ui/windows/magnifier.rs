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
            ERROR_SUCCESS, GetLastError, HWND, LPARAM, LRESULT, RECT, SIZE, SetLastError, WPARAM,
        },
        Graphics::{
            Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute},
            Gdi::*,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    },
    core::{BOOL, Error, PCWSTR, Result, w},
};

use super::drawing::{
    OwnedFont, PaintSession, border_thickness, dip, draw_bottom_right_border, draw_text, palette,
};
use super::frost::FrostedPanel;
use crate::{
    app::{config::AppearanceConfig, diagnostics, i18n::tr},
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
    scale: Vec<u16>,
    coordinates: Vec<u16>,
    compact_coordinates: Vec<u16>,
}

impl State {
    fn hit_test(&self, point: ScreenPointPx) -> Option<SourcePixel> {
        if !self.visible
            || self.layout_invalidated
            || self.paint_error.is_some()
            || !self.accepts_pointer(point)
        {
            return None;
        }
        self.view.as_ref()?.hit_test(point)
    }

    fn draw(&self, target: HDC) -> Result<()> {
        if let (Some(surface), Some(view), Some(bounds)) = (&self.surface, &self.view, self.bounds)
        {
            surface.draw(
                target,
                view,
                bounds,
                self.hover.map(|pixel| pixel.cache),
                &self.footer,
            )?;
        }
        Ok(())
    }

    fn accepts_pointer(&self, point: ScreenPointPx) -> bool {
        self.bounds
            .zip(self.surface.as_ref())
            .is_some_and(|(bounds, surface)| {
                inside_uncovered_window(
                    point,
                    bounds,
                    surface.dpi,
                    surface.appearance.border_width_dip,
                )
            })
    }

    fn refresh_text(&mut self) {
        let factor = self.view.as_ref().map_or(4, |view| view.scale().factor());
        let hex = self.hover.map_or_else(
            || "—".to_owned(),
            |pixel| format_color(pixel.rgb, ColorFormat::Hex),
        );
        let (x, y) = self.hover.map_or_else(
            || ("X —".to_owned(), "Y —".to_owned()),
            |pixel| {
                (
                    format!("X {}", pixel.source.x),
                    format!("Y {}", pixel.source.y),
                )
            },
        );
        self.footer = Footer {
            rgb: self.hover.map(|pixel| pixel.rgb),
            hex: hex.encode_utf16().collect(),
            scale: format!("{factor}×").encode_utf16().collect(),
            coordinates: format!("{x}  {y}").encode_utf16().collect(),
            compact_coordinates: format!("{} {}", x.replace(' ', ""), y.replace(' ', ""))
                .encode_utf16()
                .collect(),
        };
    }

    fn check(&mut self) -> Result<()> {
        if let Some(error) = self.paint_error.take() {
            return Err(error);
        }
        if self.layout_invalidated {
            return Err(failure(tr(
                "冻结期间显示缩放发生变化，请重新取色",
                "Display scaling changed while frozen. Please pick again.",
            )));
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
    pub fn new(image: FrozenImage, focus: ScreenPointPx, work_area: ScreenRectPx) -> Result<Self> {
        Self::with_appearance(image, focus, work_area, AppearanceConfig::default())
    }

    pub fn with_appearance(
        mut image: FrozenImage,
        focus: ScreenPointPx,
        work_area: ScreenRectPx,
        appearance: AppearanceConfig,
    ) -> Result<Self> {
        appearance
            .validate()
            .map_err(|error| Error::new(E_INVALIDARG, error.to_string()))?;
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
        let title: Vec<u16> = tr("缩放取色 — Color Picker", "Zoom picker — Color Picker")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // Creating the hidden one-pixel window on the target monitor establishes
        // its actual DPI before any DIP-sized layout is tested against work area.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                CLASS_NAME,
                PCWSTR(title.as_ptr()),
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
        let capture_excluded =
            match unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
                Ok(()) => true,
                Err(error) => {
                    diagnostics::event(format_args!("magnifier.capture_exclusion_failed {error}"));
                    false
                }
            };
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let (bounds, viewport) = window_layout(focus, work_area, dpi, image.width, image.height)?;
        let view = ZoomView::new(image, viewport, ZoomScale::X4, cache)
            .map_err(|_| Error::new(E_INVALIDARG, "Could not map the frozen viewport"))?;
        let surface = Surface::new(
            bounds.width() as i32,
            bounds.height() as i32,
            dpi,
            capture_excluded,
            appearance,
        )?;
        {
            let mut state = window.state.borrow_mut();
            state.hover =
                if inside_uncovered_window(focus, bounds, dpi, appearance.border_width_dip) {
                    view.hit_test(focus)
                } else {
                    None
                };
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
        // Confirm from the event coordinate, even if hover painting is delayed.
        let pixel = state.hit_test(point)?;
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
            let hover = if state.accepts_pointer(point) {
                state.view.as_mut().and_then(|view| view.select_at(point))
            } else {
                None
            };
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
            let accepts_pointer = state.accepts_pointer(point);
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
            let hover = if accepts_pointer {
                view.hit_test(point)
            } else {
                None
            };
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
    image_width: u32,
    image_height: u32,
) -> Result<(ScreenRectPx, ScreenRectPx)> {
    if dpi == 0 || dpi > 9600 || work.is_empty() || image_width == 0 || image_height == 0 {
        return Err(failure("Invalid magnifier DPI/work area"));
    }
    let initial_scale = i64::from(ZoomScale::X4.factor());
    let cap = i64::from(dip(240, dpi));
    // Source pixels, unlike the footer, are physical pixels. In particular a
    // 65×65 image at 4× needs 260×260 px even on a high-DPI display. Keep only
    // the 32 px minimum needed for a complete cell at the highest zoom when a
    // caller supplies an unusually narrow image. Round down to whole 4× cells.
    let axis = |source: u32, available: i64| {
        (i64::from(source) * initial_scale)
            .max(i64::from(ZoomScale::X32.factor()))
            .min(cap)
            .min(available)
            / initial_scale
            * initial_scale
    };
    let width = axis(image_width, i64::from(work.width()));
    let footer = i64::from(footer_height(dpi));
    let viewport_height = axis(image_height, i64::from(work.height()) - footer);
    if width < 32 || viewport_height < 32 {
        return Err(failure(tr(
            "工作区空间不足，无法显示完整像素格",
            "Not enough screen space to show a complete pixel cell",
        )));
    }
    let height = viewport_height + footer;
    let left =
        (i64::from(focus.x) - width / 2).clamp(i64::from(work.left), i64::from(work.right) - width);
    let top = (i64::from(focus.y) - viewport_height / 2)
        .clamp(i64::from(work.top), i64::from(work.bottom) - height);
    let bounds = ScreenRectPx {
        left: left as i32,
        top: top as i32,
        right: (left + width) as i32,
        bottom: (top + height) as i32,
    };
    let viewport = ScreenRectPx {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right,
        bottom: (top + viewport_height) as i32,
    };
    Ok((bounds, viewport))
}

fn footer_height(dpi: u32) -> i32 {
    dip(28, dpi)
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
                    && let Err(error) = state.draw(paint.dc)
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
    compact_font: OwnedFont,
    narrow_font: OwnedFont,
    frost: Option<FrostedPanel>,
    appearance: AppearanceConfig,
    old_bitmap: HGDIOBJ,
    width: i32,
    height: i32,
    dpi: u32,
}

impl Surface {
    fn new(
        width: i32,
        height: i32,
        dpi: u32,
        capture_excluded: bool,
        appearance: AppearanceConfig,
    ) -> Result<Self> {
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
        let heading_font = OwnedFont::new(13, 600, dpi)?;
        let body_font = OwnedFont::new(10, 400, dpi)?;
        // Edge-cropped images can be only 132 physical pixels wide even at
        // high DPI. Compact fonts fit that image; the window never grows.
        let compact_font = OwnedFont::new(dip(8, dpi).min(12), 400, 96)?;
        let narrow_font = OwnedFont::new(8, 400, 96)?;
        let frost = if capture_excluded && appearance.background_transparency_percent != 0 {
            match FrostedPanel::new(
                width,
                footer_height(dpi),
                dip(6, dpi),
                appearance.background_transparency_percent,
            ) {
                Ok(panel) => Some(panel),
                Err(error) => {
                    diagnostics::event(format_args!("magnifier.frost_unavailable {error}"));
                    None
                }
            }
        } else {
            None
        };
        let old_bitmap = unsafe { SelectObject(dc.0, bitmap.0.into()) };
        if old_bitmap.is_invalid() {
            return Err(failure("Could not select magnifier bitmap"));
        }
        Ok(Self {
            dc,
            _bitmap: bitmap,
            heading_font,
            body_font,
            compact_font,
            narrow_font,
            frost,
            appearance,
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
        hover: Option<CachePoint>,
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
        // Give GDI only the selected top-down rows, with ySrc = 0. Passing a
        // nonzero top-origin ySrc against the full negative-height DIB selects
        // the vertically opposite crop on the GDI path. A row slice avoids
        // that convention without flipping pixels or changing hit mapping.
        // compact_rows already made every source row image.width * 4 bytes.
        let rows = &image.bgrx[source.y as usize * image.stride_bytes
            ..(source.y + source.height) as usize * image.stride_bytes];
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: image.width as i32,
                biHeight: -(source.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (image.width * source.height * 4),
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
                0,
                source.width as i32,
                source.height as i32,
                Some(rows.as_ptr().cast()),
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
        // The zoom anchor can differ from the pixel under the cursor after
        // edge clamping. Highlight exactly the same hover used by the footer.
        if let Some(center) = hover.and_then(|cache| view.cell_center(cache)) {
            let left = center.x - bounds.left - scale / 2;
            let top = center.y - bounds.top - scale / 2;
            self.frame(
                RECT {
                    left: left - 1,
                    top: top - 1,
                    right: left + scale + 1,
                    bottom: top + scale + 1,
                },
                drawn,
                COLORREF(0),
            )?;
            self.frame(
                RECT {
                    left,
                    top,
                    right: left + scale,
                    bottom: top + scale,
                },
                drawn,
                COLORREF(0x00ffffff),
            )?;
        }
        self.draw_footer(viewport.bottom, footer, bounds)?;
        draw_bottom_right_border(
            self.dc.0,
            self.width,
            self.height,
            self.dpi,
            self.appearance.border_width_dip,
        )?;
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

    fn draw_footer(&self, top: i32, footer: &Footer, bounds: ScreenRectPx) -> Result<()> {
        self.fill(
            RECT {
                left: 0,
                top,
                right: self.width,
                bottom: self.height,
            },
            palette::PANEL,
        )?;
        if let Some(frost) = &self.frost {
            let panel = RECT {
                left: 0,
                top,
                right: self.width,
                bottom: self.height,
            };
            if frost
                .paint(
                    self.dc.0,
                    panel,
                    ScreenPointPx {
                        x: bounds.left,
                        y: bounds.top,
                    },
                )
                .is_err()
            {
                // Backdrop failure is decorative only; retain readable colors
                // and continue sampling with the ordinary solid information bar.
                self.fill(panel, palette::PANEL)?;
            }
        }
        let border = border_thickness(
            self.width,
            self.height,
            self.dpi,
            self.appearance.border_width_dip,
        );
        let color = footer.rgb.map_or(palette::EMPTY, |rgb| {
            COLORREF(u32::from(rgb.r) | (u32::from(rgb.g) << 8) | (u32::from(rgb.b) << 16))
        });
        self.fill(
            RECT {
                left: 0,
                top,
                right: self.footer_swatch_width(footer, border)?,
                bottom: self.height - border,
            },
            color,
        )?;
        for FooterLabel {
            rect,
            font,
            color,
            text,
        } in self.footer_labels(footer, border)?
        {
            draw_text(
                self.dc.0,
                font,
                RECT {
                    top: top + rect.top,
                    bottom: (top + rect.bottom).min(self.height - border),
                    ..rect
                },
                color,
                text,
            )?;
        }
        Ok(())
    }

    fn text_size(&self, font: &OwnedFont, text: &[u16]) -> Result<SIZE> {
        font.measure(self.dc.0, text)
    }

    fn footer_swatch_width(&self, footer: &Footer, border: i32) -> Result<i32> {
        // Fill the footer height flush to the left edge. Limit width on narrow
        // snapshots without shrinking text below the existing smallest font.
        let hex: Vec<u16> = "#DDDDDD".encode_utf16().collect();
        let text_width = self.text_size(&self.narrow_font, &hex)?.cx
            + self
                .text_size(&self.narrow_font, &footer.compact_coordinates)?
                .cx;
        let remaining = self.width - border - text_width - 8;
        Ok((footer_height(self.dpi) - border)
            .min((self.width - border) / 8)
            .min(remaining)
            .max(0))
    }

    fn footer_labels<'a>(
        &'a self,
        footer: &'a Footer,
        border: i32,
    ) -> Result<[FooterLabel<'a>; 3]> {
        let left = self.footer_swatch_width(footer, border)? + 2;
        let right = (self.width - border - 2).max(left);
        let available = right - left;
        let gap = 4;
        let widest_hex: Vec<u16> = "#DDDDDD".encode_utf16().collect();
        let choices = [
            (
                &self.heading_font,
                &self.body_font,
                footer.coordinates.as_slice(),
            ),
            (
                &self.body_font,
                &self.body_font,
                footer.compact_coordinates.as_slice(),
            ),
            (
                &self.compact_font,
                &self.compact_font,
                footer.compact_coordinates.as_slice(),
            ),
            (
                &self.narrow_font,
                &self.narrow_font,
                footer.compact_coordinates.as_slice(),
            ),
        ];
        for (index, (hex_font, info_font, coordinates)) in choices.into_iter().enumerate() {
            let hex = self.text_size(hex_font, &widest_hex)?;
            let position = self.text_size(info_font, coordinates)?;
            let scale = self.text_size(info_font, &footer.scale)?;
            if hex.cx + gap + position.cx > available && index != choices.len() - 1 {
                continue;
            }
            let show_scale = hex.cx + position.cx + scale.cx + 3 * gap <= available;
            let coordinate_left = (right - position.cx)
                .max(left)
                .max(left + available.min(hex.cx + gap));
            let row_height = (footer_height(self.dpi) - border).max(0);
            let rect = |x: i32, end: i32, height: i32| RECT {
                left: x.min(right),
                top: ((row_height - height) / 2).max(0),
                right: end.min(right),
                bottom: row_height,
            };
            return Ok([
                FooterLabel {
                    rect: rect(left, (left + hex.cx).min(coordinate_left - gap), hex.cy),
                    font: hex_font,
                    color: palette::TEXT,
                    text: &footer.hex,
                },
                FooterLabel {
                    rect: rect(left + hex.cx + gap, coordinate_left - gap, scale.cy),
                    font: info_font,
                    color: palette::ACCENT,
                    text: if show_scale { &footer.scale } else { &[] },
                },
                FooterLabel {
                    rect: rect(coordinate_left, right, position.cy),
                    font: info_font,
                    color: palette::SECONDARY,
                    text: coordinates,
                },
            ]);
        }
        unreachable!("the smallest font always supplies a clipped layout")
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
    fn frame(&self, rect: RECT, clip: RECT, color: COLORREF) -> Result<()> {
        // Clipping each original edge preserves the cell geometry. Intersecting
        // the frame rectangle first would invent an edge along the clip boundary.
        for edge in clipped_frame_edges(rect, clip).into_iter().flatten() {
            self.fill(edge, color)?;
        }
        Ok(())
    }
}

struct FooterLabel<'a> {
    rect: RECT,
    font: &'a OwnedFont,
    color: COLORREF,
    text: &'a [u16],
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

fn inside_uncovered_window(
    point: ScreenPointPx,
    bounds: ScreenRectPx,
    dpi: u32,
    border_width_dip: u8,
) -> bool {
    let border = border_thickness(
        bounds.width() as i32,
        bounds.height() as i32,
        dpi,
        border_width_dip,
    );
    // The bottom edge is already in the non-pixel footer. The right edge can
    // cover image columns, which must neither hover nor produce a picked color.
    bounds.contains(point) && point.x < bounds.right - border
}

fn clipped_frame_edges(rect: RECT, clip: RECT) -> [Option<RECT>; 4] {
    [
        RECT {
            bottom: rect.top + 1,
            ..rect
        },
        RECT {
            top: rect.bottom - 1,
            ..rect
        },
        RECT {
            right: rect.left + 1,
            ..rect
        },
        RECT {
            left: rect.right - 1,
            ..rect
        },
    ]
    .map(|edge| {
        let clipped = RECT {
            left: edge.left.max(clip.left),
            top: edge.top.max(clip.top),
            right: edge.right.min(clip.right),
            bottom: edge.bottom.min(clip.bottom),
        };
        (clipped.left < clipped.right && clipped.top < clipped.bottom).then_some(clipped)
    })
}

fn failure(message: &'static str) -> Error {
    Error::new(E_FAIL, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge_state(focus: ScreenPointPx, monitor: ScreenRectPx) -> State {
        let work = ScreenRectPx {
            bottom: monitor.bottom - 40,
            ..monitor
        };
        let capture = crate::core::geometry::freeze_rect(focus, monitor).unwrap();
        let mut snapshot = image(capture.width(), capture.height());
        snapshot.origin = ScreenPointPx {
            x: capture.left,
            y: capture.top,
        };
        for y in 0..snapshot.height {
            for x in 0..snapshot.width {
                let offset = y as usize * snapshot.stride_bytes + x as usize * 4;
                snapshot.bgrx[offset..offset + 4].copy_from_slice(&[
                    17,
                    y as u8 + 1,
                    x as u8 + 1,
                    0,
                ]);
            }
        }
        let (bounds, viewport) =
            window_layout(focus, work, 96, snapshot.width, snapshot.height).unwrap();
        let view = ZoomView::new(
            snapshot,
            viewport,
            ZoomScale::X4,
            CachePoint {
                x: (focus.x - capture.left) as u32,
                y: (focus.y - capture.top) as u32,
            },
        )
        .unwrap();
        let surface = Surface::new(
            bounds.width() as i32,
            bounds.height() as i32,
            96,
            false,
            AppearanceConfig {
                border_width_dip: 2,
                background_transparency_percent: 0,
            },
        )
        .unwrap();
        let mut state = State {
            view: Some(view),
            surface: Some(surface),
            bounds: Some(bounds),
            visible: true,
            ..Default::default()
        };
        state.hover = state.hit_test(focus);
        state.refresh_text();
        state
    }

    fn assert_hover_frame(state: &State, point: ScreenPointPx) {
        let surface = state.surface.as_ref().unwrap();
        let view = state.view.as_ref().unwrap();
        let bounds = state.bounds.unwrap();
        state.draw(surface.dc.0).unwrap();
        assert_eq!(
            state.hover,
            state.hit_test(point),
            "click mapping must agree"
        );
        assert_eq!(state.footer.rgb, state.hover.map(|pixel| pixel.rgb));
        if let Some(pixel) = state.hover {
            assert_eq!(
                String::from_utf16(&state.footer.hex).unwrap(),
                format_color(pixel.rgb, ColorFormat::Hex),
            );
            assert_eq!(
                String::from_utf16(&state.footer.coordinates).unwrap(),
                format!("X {}  Y {}", pixel.source.x, pixel.source.y),
            );
            let center = view.cell_center(pixel.cache).unwrap();
            let scale = view.scale().factor() as i32;
            assert_eq!(
                unsafe {
                    GetPixel(
                        surface.dc.0,
                        center.x - bounds.left - scale / 2 + 1,
                        center.y - bounds.top - scale / 2,
                    )
                },
                COLORREF(0x00ffffff),
                "the actual GDI frame must surround hover.cache",
            );
        } else {
            assert_eq!(String::from_utf16(&state.footer.hex).unwrap(), "—");
            // No white pixels occur in the fixture or grid. Any white in the
            // image would therefore be a stale current-pixel frame.
            let drawn = local_rect(view.drawn_rect(), bounds);
            for y in drawn.top..drawn.bottom {
                for x in drawn.left..drawn.right - 2 {
                    assert_ne!(
                        unsafe { GetPixel(surface.dc.0, x, y) },
                        COLORREF(0x00ffffff)
                    );
                }
            }
        }
    }

    #[test]
    fn edge_highlight_uses_hover_instead_of_the_initial_zoom_anchor() {
        for monitor in [
            ScreenRectPx {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            ScreenRectPx {
                left: -1920,
                top: -1080,
                right: 0,
                bottom: 0,
            },
        ] {
            for (x, y) in [
                (20, 500),
                (500, 20),
                (20, 20),
                (1900, 20),
                (20, 1000),
                (1900, 1000),
            ] {
                let focus = ScreenPointPx {
                    x: monitor.left + x,
                    y: monitor.top + y,
                };
                let state = edge_state(focus, monitor);
                let hover = state.hover.unwrap();
                if x == 20 && y == 500 {
                    assert_eq!(hover.cache.x, 5);
                    assert_eq!(state.view.as_ref().unwrap().selected().x, 20);
                }
                assert_hover_frame(&state, focus);
            }
        }
    }

    #[test]
    fn hover_frame_and_footer_follow_zoom_clamping_exit_and_reentry() {
        let focus = ScreenPointPx { x: 20, y: 500 };
        let mut state = edge_state(
            focus,
            ScreenRectPx {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
        );
        for scale in [
            ZoomScale::X8,
            ZoomScale::X16,
            ZoomScale::X32,
            ZoomScale::X16,
            ZoomScale::X8,
            ZoomScale::X4,
        ] {
            let drawn = state.view.as_ref().unwrap().drawn_rect();
            let point = ScreenPointPx {
                x: drawn.left + 3,
                y: drawn.top + 3,
            };
            state.view.as_mut().unwrap().change_scale(scale, point);
            state.hover = state.hit_test(point);
            state.refresh_text();
            assert_hover_frame(&state, point);
        }
        let bounds = state.bounds.unwrap();
        let viewport = state.view.as_ref().unwrap().viewport();
        for point in [
            ScreenPointPx {
                x: bounds.left + 20,
                y: viewport.bottom + 1,
            },
            ScreenPointPx {
                x: bounds.right - 1,
                y: bounds.top + 20,
            },
            ScreenPointPx {
                x: bounds.left - 1,
                y: bounds.top + 20,
            },
            ScreenPointPx {
                x: bounds.left + 20,
                y: bounds.top + 20,
            },
        ] {
            state.hover = state.hit_test(point);
            state.refresh_text();
            assert_hover_frame(&state, point);
        }
        let prior_hover = state.hover;
        assert_eq!(
            state.hit_test(ScreenPointPx {
                x: bounds.left + 21,
                y: bounds.top + 21,
            }),
            prior_hover,
            "small movement within the same source cell keeps the target",
        );
        let other = ScreenPointPx {
            x: bounds.left + 100,
            y: bounds.top + 100,
        };
        assert_ne!(
            state.hit_test(other),
            prior_hover,
            "clicks remap without waiting for hover"
        );
    }

    #[test]
    fn footer_tracks_hover_source_coordinates_across_pixels_and_zoom() {
        let mut snapshot = image(65, 65);
        snapshot.origin = ScreenPointPx { x: -1800, y: -500 };
        let viewport = ScreenRectPx {
            left: -900,
            top: 200,
            right: -640,
            bottom: 460,
        };
        let view = ZoomView::new(
            snapshot,
            viewport,
            ZoomScale::X4,
            CachePoint { x: 32, y: 32 },
        )
        .unwrap();
        let mut state = State {
            view: Some(view),
            ..Default::default()
        };
        let anchor = ScreenPointPx { x: -770, y: 330 };
        for scale in [ZoomScale::X4, ZoomScale::X8, ZoomScale::X16, ZoomScale::X32] {
            state.view.as_mut().unwrap().change_scale(scale, anchor);
            for offset in [0, scale.factor() as i32] {
                let cursor = ScreenPointPx {
                    x: anchor.x + offset,
                    ..anchor
                };
                state.hover = state.view.as_mut().unwrap().select_at(cursor);
                let pixel = state.hover.unwrap();
                state.refresh_text();
                assert_ne!(
                    pixel.source, cursor,
                    "coordinates refer to the frozen source"
                );
                assert_eq!(
                    String::from_utf16(&state.footer.coordinates).unwrap(),
                    format!(
                        "X {}  Y {}",
                        -1800 + pixel.cache.x as i32,
                        -500 + pixel.cache.y as i32
                    )
                );
                assert_eq!(
                    String::from_utf16(&state.footer.scale).unwrap(),
                    format!("{}×", scale.factor())
                );
            }
        }
        state.hover = None;
        state.refresh_text();
        assert_eq!(
            String::from_utf16(&state.footer.coordinates).unwrap(),
            "X —  Y —"
        );
        assert_eq!(String::from_utf16(&state.footer.hex).unwrap(), "—");
    }

    #[test]
    fn single_row_footer_fits_the_actual_pixel_width_at_supported_desktop_scaling() {
        use crate::app::i18n::{Language, language, set_language};
        let previous_language = language();
        for language in [Language::SimplifiedChinese, Language::English] {
            set_language(language);
            assert_single_row_footer_fits();
        }
        set_language(previous_language);
    }

    fn assert_single_row_footer_fits() {
        let footer = Footer {
            rgb: Some(Rgb8::new(221, 221, 221)),
            hex: "#DDDDDD".encode_utf16().collect(),
            scale: "32×".encode_utf16().collect(),
            coordinates: "X -65535  Y -65535".encode_utf16().collect(),
            compact_coordinates: "X-65535 Y-65535".encode_utf16().collect(),
        };
        for dpi in [96, 120, 144, 168, 192] {
            for width in [132, 260] {
                let surface = Surface::new(
                    width,
                    footer_height(dpi),
                    dpi,
                    false,
                    AppearanceConfig::default(),
                )
                .unwrap();
                for border in [0, dip(6, dpi)] {
                    let labels = surface.footer_labels(&footer, border).unwrap();
                    assert!(labels[0].rect.right < labels[2].rect.left);
                    for label in labels {
                        if label.text.is_empty() {
                            continue;
                        }
                        let size = surface.text_size(label.font, label.text).unwrap();
                        assert!(
                            size.cx <= label.rect.right - label.rect.left,
                            "text clips at width {width}, {dpi} DPI"
                        );
                        assert!(size.cy <= label.rect.bottom - label.rect.top);
                        assert!(label.rect.left >= 0 && label.rect.right <= width - border);
                        assert!(label.rect.bottom <= footer_height(dpi) - border);
                    }
                }
            }
        }
    }

    #[test]
    fn rendered_rows_follow_upper_and_lower_cursor_anchors_at_every_zoom() {
        let bounds = ScreenRectPx {
            left: -500,
            top: -300,
            right: -240,
            bottom: 37,
        };
        let viewport = ScreenRectPx {
            bottom: -40,
            ..bounds
        };
        let surface = Surface::new(
            260,
            337,
            168,
            false,
            AppearanceConfig {
                border_width_dip: 0,
                background_transparency_percent: 0,
            },
        )
        .unwrap();
        for anchor_y in [52, 208] {
            let mut snapshot = image(65, 65);
            for y in 0..65 {
                for x in 0..65 {
                    let offset = (y * 65 + x) * 4;
                    snapshot.bgrx[offset..offset + 4].copy_from_slice(&[
                        (x ^ y) as u8,
                        y as u8,
                        x as u8,
                        0,
                    ]);
                }
            }
            let mut view = ZoomView::new(
                snapshot,
                viewport,
                ZoomScale::X4,
                CachePoint { x: 32, y: 32 },
            )
            .unwrap();
            let anchor = ScreenPointPx {
                x: bounds.left + 130,
                y: bounds.top + anchor_y,
            };
            let expected_anchor = view.hit_test(anchor).unwrap();
            for scale in [
                ZoomScale::X4,
                ZoomScale::X8,
                ZoomScale::X16,
                ZoomScale::X32,
                ZoomScale::X16,
                ZoomScale::X8,
                ZoomScale::X4,
            ] {
                view.change_scale(scale, anchor);
                assert_eq!(view.hit_test(anchor), Some(expected_anchor));
                // Paint through the real GDI path into its own offscreen buffer.
                // No desktop capture, visible window or synthesized input.
                surface
                    .draw(surface.dc.0, &view, bounds, None, &Footer::default())
                    .unwrap();
                let source = view.source_view();
                for y in [
                    source.y,
                    source.y + source.height / 2,
                    source.y + source.height - 1,
                ] {
                    for x in [
                        source.x,
                        source.x + source.width / 2,
                        source.x + source.width - 1,
                    ] {
                        let point = view.cell_center(CachePoint { x, y }).unwrap();
                        let pixel = view.hit_test(point).unwrap();
                        let rendered = unsafe {
                            GetPixel(surface.dc.0, point.x - bounds.left, point.y - bounds.top)
                        };
                        let expected = COLORREF(
                            u32::from(pixel.rgb.r)
                                | (u32::from(pixel.rgb.g) << 8)
                                | (u32::from(pixel.rgb.b) << 16),
                        );
                        assert_eq!(
                            rendered, expected,
                            "anchor y={anchor_y}, scale={scale:?}, cache=({x},{y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn frozen_layout_uses_target_dpi_and_stays_inside_work_area() {
        let work = ScreenRectPx {
            left: -1200,
            top: -700,
            right: -200,
            bottom: 100,
        };
        for dpi in [96, 120, 144, 168, 192] {
            let (window, viewport) =
                window_layout(ScreenPointPx { x: -1199, y: -699 }, work, dpi, 65, 65).unwrap();
            assert_eq!(window.intersection(work), Some(window));
            assert_eq!(viewport.width(), viewport.height());
            assert_eq!(viewport.width(), dip(240, dpi).min(260) as u32 / 4 * 4);
            assert_eq!((viewport.left, viewport.right), (window.left, window.right));
            assert!(viewport.contains(ScreenPointPx { x: -1199, y: -699 }));
            assert_eq!(viewport.top, window.top);
            assert_eq!(window.width(), viewport.width());
            assert_eq!(
                window.height(),
                viewport.height() + footer_height(dpi) as u32
            );
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
                96,
                65,
                65,
            )
            .is_err()
        );
    }

    #[test]
    fn edge_crops_and_small_work_areas_use_rectangular_complete_cells() {
        let work = ScreenRectPx {
            left: -1000,
            top: -800,
            right: 0,
            bottom: 0,
        };
        let (window, viewport) =
            window_layout(ScreenPointPx { x: -1, y: -1 }, work, 168, 33, 65).unwrap();
        assert_eq!(viewport.width(), 132);
        assert_eq!(viewport.height(), 260);
        assert_eq!(window.height(), 309);
        assert_eq!(window.width(), 132);
        assert!(viewport.left <= -1 && viewport.right > -1);
        assert_eq!(window.intersection(work), Some(window));
        let mut view = ZoomView::new(
            image(33, 65),
            viewport,
            ZoomScale::X4,
            CachePoint { x: 16, y: 32 },
        )
        .unwrap();
        assert_eq!(
            view.drawn_rect(),
            viewport,
            "initial view has no unused margins"
        );
        let anchor = ScreenPointPx {
            x: viewport.left + 66,
            y: viewport.top + 130,
        };
        for scale in [ZoomScale::X8, ZoomScale::X16, ZoomScale::X32] {
            view.change_scale(scale, anchor);
            assert_eq!(
                view.viewport(),
                viewport,
                "zoom keeps the physical viewport fixed"
            );
            assert_eq!(view.drawn_rect().width() % scale.factor(), 0);
            assert_eq!(view.drawn_rect().height() % scale.factor(), 0);
            assert!(view.hit_test(anchor).is_some());
            assert!(
                view.hit_test(ScreenPointPx {
                    x: viewport.left,
                    y: viewport.bottom
                })
                .is_none()
            );
        }

        let small_work = ScreenRectPx {
            left: -119,
            top: -103,
            right: 0,
            bottom: 0,
        };
        let (window, viewport) =
            window_layout(ScreenPointPx { x: -1, y: -1 }, small_work, 96, 65, 65).unwrap();
        assert_eq!((viewport.width(), viewport.height()), (116, 72));
        assert_eq!(window.intersection(small_work), Some(window));
    }

    #[test]
    fn tiny_images_keep_one_complete_cell_at_the_highest_scale() {
        let work = ScreenRectPx {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 800,
        };
        for size in 1..=7 {
            let (_, viewport) =
                window_layout(ScreenPointPx { x: 50, y: 50 }, work, 168, size, 8 - size).unwrap();
            assert_eq!((viewport.width(), viewport.height()), (32, 32));
            let mut view = ZoomView::new(
                image(size, 8 - size),
                viewport,
                ZoomScale::X4,
                CachePoint { x: 0, y: 0 },
            )
            .unwrap();
            let anchor = view.cell_center(CachePoint { x: 0, y: 0 }).unwrap();
            view.change_scale(ZoomScale::X32, anchor);
            assert_eq!(view.drawn_rect(), viewport);
            assert_eq!(
                view.hit_test(anchor).unwrap().cache,
                CachePoint { x: 0, y: 0 }
            );
        }
    }

    #[test]
    fn selection_frame_clips_original_edges_to_pixels_only() {
        let pixels = RECT {
            left: 0,
            top: 0,
            right: 32,
            bottom: 32,
        };
        let edges = clipped_frame_edges(
            RECT {
                left: -1,
                top: 27,
                right: 5,
                bottom: 33,
            },
            pixels,
        );
        assert!(
            edges[1].is_none(),
            "outer bottom edge cannot enter the footer"
        );
        assert!(
            edges[2].is_none(),
            "outer left edge cannot escape the image"
        );
        assert_eq!(edges[0].unwrap().top, 27);
        assert_eq!(edges[3].unwrap().left, 4);
        for edge in edges.into_iter().flatten() {
            assert!(edge.left >= 0 && edge.top >= 0 && edge.right <= 32 && edge.bottom <= 32);
        }
        let bounds = ScreenRectPx {
            left: -100,
            top: 20,
            right: 140,
            bottom: 284,
        };
        for dpi in [96, 144] {
            for border in [0, 2, 6] {
                let first_covered_x = bounds.right - dip(i32::from(border), dpi);
                assert!(inside_uncovered_window(
                    ScreenPointPx {
                        x: first_covered_x - 1,
                        y: 100
                    },
                    bounds,
                    dpi,
                    border,
                ));
                assert!(!inside_uncovered_window(
                    ScreenPointPx {
                        x: first_covered_x,
                        y: 100
                    },
                    bounds,
                    dpi,
                    border,
                ));
            }
        }
    }

    fn image(width: u32, height: u32) -> FrozenImage {
        FrozenImage {
            origin: ScreenPointPx { x: 0, y: 0 },
            width,
            height,
            stride_bytes: width as usize * 4,
            bgrx: vec![0; width as usize * height as usize * 4],
        }
    }
}
