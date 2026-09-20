use std::cell::RefCell;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use windows::Win32::Foundation::{
    COLORREF, E_FAIL, E_INVALIDARG, ERROR_CLASS_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HWND,
    LPARAM, LRESULT, POINT, SetLastError, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    InvalidateRect, MONITOR_DEFAULTTONULL, MonitorFromPoint, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Error, Result, w};

use super::drawing::{Content, PaintSession, Surface, dip};
use crate::app::{config::AppearanceConfig, diagnostics};
use crate::core::color::Rgb8;
use crate::core::format::{ColorFormat, format_color};
use crate::core::geometry::{ScreenPointPx, ScreenRectPx};
use crate::core::placement::place_preview;

const CLASS_NAME: windows::core::PCWSTR = w!("ColorPicker.Preview.v1");

#[derive(Default)]
struct State {
    sample: Option<(ScreenPointPx, Option<Rgb8>)>,
    content: Option<Content>,
    surface: Option<Surface>,
    rect: Option<ScreenRectPx>,
    dpi_dirty: bool,
    paint_error: Option<Error>,
}

/// The stable allocation is borrowed by GWLP_USERDATA. Only this owner releases
/// it, after DestroyWindow has finished all synchronous destruction callbacks.
pub struct PreviewWindow {
    hwnd: HWND,
    state: Box<RefCell<State>>,
    capture_exclusion_enabled: bool,
    appearance: AppearanceConfig,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl PreviewWindow {
    pub fn new() -> Result<Self> {
        Self::with_appearance(AppearanceConfig::default())
    }

    pub fn with_appearance(appearance: AppearanceConfig) -> Result<Self> {
        appearance
            .validate()
            .map_err(|error| Error::new(E_INVALIDARG, error.to_string()))?;
        let instance = unsafe { GetModuleHandleW(None)? }.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // The class is reused for the lifetime of this process; sessions own
        // windows/resources only, so repeated sessions do not register classes.
        if unsafe { RegisterClassW(&class) } == 0
            && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
        {
            return Err(Error::from_thread());
        }
        let state = Box::new(RefCell::new(State::default()));
        let pointer = state.as_ref() as *const RefCell<State>;
        let hwnd = unsafe {
            CreateWindowExW(
                // Establish the intended z-order at creation; some desktops
                // suppress a later TOPMOST promotion of nonactivating windows.
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                CLASS_NAME,
                w!("color-picker preview"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                Some(pointer.cast()),
            )?
        };
        let mut window = Self {
            hwnd,
            state,
            capture_exclusion_enabled: false,
            appearance,
            _thread_affinity: PhantomData,
        };
        unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)? };
        // Hiding must remove the preview at the next composition flush instead
        // of leaving subsequent fade-out frames over the pixel being sampled.
        // This is part of the correctness path, so failure prevents a session.
        let disable_transitions = BOOL::from(true);
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&disable_transitions as *const BOOL).cast(),
                std::mem::size_of::<BOOL>() as u32,
            )?
        };
        match unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
            Ok(()) => window.capture_exclusion_enabled = true,
            Err(error) => {
                diagnostics::event(format_args!("preview.capture_exclusion_failed {error}"))
            }
        }
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn capture_exclusion_enabled(&self) -> bool {
        self.capture_exclusion_enabled
    }

    /// Only a currently visible rectangle can obstruct a sample.
    pub fn rect(&self) -> Option<ScreenRectPx> {
        self.state.borrow().rect
    }

    pub fn hide(&self) {
        let was_visible = self.state.borrow_mut().rect.take().is_some();
        if was_visible {
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    pub fn update(
        &self,
        point: ScreenPointPx,
        rgb: Option<Rgb8>,
        work_area: ScreenRectPx,
    ) -> Result<bool> {
        if let Some(error) = self.state.borrow_mut().paint_error.take() {
            return Err(error);
        }
        self.prepare_target_monitor(point, work_area)?;
        let content_changed = self.state.borrow().sample != Some((point, rgb));
        if content_changed {
            let color_text = rgb
                .map(|color| format_color(color, ColorFormat::Hex))
                .unwrap_or_else(|| "暂不可采样".to_owned());
            let content = Content {
                rgb,
                color_text: color_text.encode_utf16().collect(),
                coordinates: format!("X {}  Y {}", point.x, point.y)
                    .encode_utf16()
                    .collect(),
            };
            let mut state = self.state.borrow_mut();
            state.sample = Some((point, rgb));
            state.content = Some(content);
        }
        let mut changed = content_changed;
        // Moving between mixed-DPI monitors may deliver WM_DPICHANGED during
        // SetWindowPos. A second pass then uses the actual target-window DPI.
        for _ in 0..2 {
            let dpi = unsafe { GetDpiForWindow(self.hwnd) };
            if dpi == 0 || dpi > 9600 {
                return Err(Error::new(E_FAIL, "Could not determine preview DPI"));
            }
            let width = dip(168, dpi);
            let height = dip(38, dpi);
            let Some(rect) = place_preview(
                point,
                work_area,
                width as u32,
                height as u32,
                dip(16, dpi) as u32,
            ) else {
                self.hide();
                return Err(Error::new(
                    E_FAIL,
                    "工作区空间不足，无法在避开采样点的位置显示预览",
                ));
            };
            let rebuild = self.state.borrow().surface.as_ref().is_none_or(|surface| {
                surface.width != width || surface.height != height || surface.dpi != dpi
            });
            if rebuild {
                let surface = Surface::new(
                    width,
                    height,
                    dpi,
                    self.capture_exclusion_enabled,
                    self.appearance,
                )?;
                self.state.borrow_mut().surface = Some(surface);
                changed = true;
            }
            let reposition = self.rect() != Some(rect);
            // No RefCell borrow may cross a call that can reenter window_proc.
            self.state.borrow_mut().dpi_dirty = false;
            if reposition {
                unsafe {
                    SetWindowPos(
                        self.hwnd,
                        Some(HWND_TOPMOST),
                        rect.left,
                        rect.top,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    )?
                };
                self.state.borrow_mut().rect = Some(rect);
                changed = true;
            }
            if !self.state.borrow().dpi_dirty {
                break;
            }
        }
        if changed && !unsafe { InvalidateRect(Some(self.hwnd), None, false) }.as_bool() {
            return Err(Error::new(E_FAIL, "Could not invalidate the preview"));
        }
        Ok(changed)
    }

    fn prepare_target_monitor(&self, point: ScreenPointPx, work_area: ScreenRectPx) -> Result<()> {
        if work_area.is_empty() {
            self.hide();
            return Err(Error::new(E_FAIL, "预览工作区为空"));
        }
        let target = unsafe {
            MonitorFromPoint(
                POINT {
                    x: point.x,
                    y: point.y,
                },
                MONITOR_DEFAULTTONULL,
            )
        };
        if target.is_invalid() {
            self.hide();
            return Err(Error::new(E_FAIL, "预览采样点未位于有效显示器"));
        }
        if unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONULL) } != target {
            // A 400%-DPI window can be wider than a small 100%-DPI monitor even
            // though its correctly scaled preview would fit. Move a hidden,
            // one-pixel window first, then read its real target DPI for layout.
            self.hide();
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    None,
                    work_area.left,
                    work_area.top,
                    1,
                    1,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )?
            };
            if unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONULL) } != target {
                return Err(Error::new(E_FAIL, "无法将预览定位到采样点所在显示器"));
            }
        }
        Ok(())
    }
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyWindow(self.hwnd) } {
            // Normally only possible for an already-destroyed HWND. Defensively
            // detach the borrowed pointer before the owner allocation is freed.
            unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
            diagnostics::event(format_args!("preview.destroy_failed {error}"));
        }
    }
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
        if previous == 0 && unsafe { GetLastError() } != ERROR_SUCCESS {
            return LRESULT(0);
        }
        return LRESULT(1);
    }
    if message == WM_NCDESTROY {
        // The pointer is borrowed. Never Box::from_raw it in a callback.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<State>;
    if !pointer.is_null() {
        let state = unsafe { &*pointer };
        match message {
            WM_PAINT => {
                // BeginPaint may itself send WM_ERASEBKGND; borrow afterwards.
                let paint = PaintSession::begin(hwnd);
                if let Ok(mut state) = state.try_borrow_mut()
                    && let (Some(surface), Some(content), Some(rect)) =
                        (&state.surface, &state.content, state.rect)
                    && let Err(error) = surface.draw(
                        paint.dc,
                        content,
                        ScreenPointPx {
                            x: rect.left,
                            y: rect.top,
                        },
                    )
                {
                    state.paint_error = Some(error);
                }
                return LRESULT(0);
            }
            WM_DPICHANGED => {
                if let Ok(mut state) = state.try_borrow_mut() {
                    state.dpi_dirty = true;
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
