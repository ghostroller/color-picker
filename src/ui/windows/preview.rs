use super::messages;
use super::window_lifetime::{WindowInit, WindowLifetime, WindowRole};
use crate::platform::windows::gdi::PaintSession;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use windows::Win32::Foundation::{
    COLORREF, E_FAIL, E_INVALIDARG, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM,
    LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    InvalidateRect, MONITOR_DEFAULTTONULL, MonitorFromPoint, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Error, PCWSTR, Result, w};

use super::drawing::{Content, Surface, dip, live_preview_height_dip};
use crate::app::{config::AppearanceConfig, diagnostics, i18n::tr};
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

#[derive(Default)]
struct CallbackData {
    lifetime: WindowLifetime,
    state: RefCell<State>,
}

/// The stable allocation is borrowed by GWLP_USERDATA. Only this owner releases
/// it, after DestroyWindow has finished all synchronous destruction callbacks.
pub struct PreviewWindow {
    hwnd: HWND,
    state: Box<CallbackData>,
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
        // SAFETY: The process module is borrowed for class/window creation; no handle ownership transfers.
        let instance = unsafe { GetModuleHandleW(None)? }.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // The class is reused for the lifetime of this process; sessions own
        // windows/resources only, so repeated sessions do not register classes.
        // SAFETY: The class uses a static callback/name and the process module remains live.
        if unsafe { RegisterClassW(&class) } == 0
            // SAFETY: Read the error from the immediately preceding native class registration.
            && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
        {
            return Err(Error::from_thread());
        }
        let state = Box::new(CallbackData::default());
        let pointer = state.as_ref() as *const CallbackData;
        let init = WindowInit {
            state: pointer.cast(),
            role: WindowRole::Root,
        };
        let title: Vec<u16> = tr("实时取色 — Color Picker", "Live picker — Color Picker")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: The stack WindowInit lives through synchronous creation; only its stable Box pointer is retained.
        let hwnd = unsafe {
            super::window_lifetime::create_window(|| {
                CreateWindowExW(
                    // Establish the intended z-order at creation; some desktops
                    // suppress a later TOPMOST promotion of nonactivating windows.
                    WS_EX_TOPMOST
                        | WS_EX_TOOLWINDOW
                        | WS_EX_NOACTIVATE
                        | WS_EX_LAYERED
                        | WS_EX_TRANSPARENT,
                    CLASS_NAME,
                    PCWSTR(title.as_ptr()),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    None,
                    None,
                    Some(instance),
                    Some((&init as *const WindowInit).cast()),
                )
            })
        };
        let hwnd = state.lifetime.creation_result("preview", hwnd)?;
        let mut window = Self {
            hwnd,
            state,
            capture_exclusion_enabled: false,
            appearance,
            _thread_affinity: PhantomData,
        };
        // SAFETY: This owned live window and the correctly sized attribute value outlive the synchronous call.
        unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)? };
        // Hiding must remove the preview at the next composition flush instead
        // of leaving subsequent fade-out frames over the pixel being sampled.
        // This is part of the correctness path, so failure prevents a session.
        let disable_transitions = BOOL::from(true);
        // SAFETY: This owned live window and the correctly sized attribute value outlive the synchronous call.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&disable_transitions as *const BOOL).cast(),
                std::mem::size_of::<BOOL>() as u32,
            )?
        };
        // SAFETY: Apply the capture policy to our live same-process overlay window.
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
        self.state.state.borrow().rect
    }

    pub fn hide(&self) {
        let was_visible = self.state.state.borrow_mut().rect.take().is_some();
        if was_visible {
            // SAFETY: The window owner retains the native window and no RefCell borrow spans this reentrant call.
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    pub fn update(
        &self,
        point: ScreenPointPx,
        rgb: Option<Rgb8>,
        work_area: ScreenRectPx,
    ) -> Result<bool> {
        if let Some(error) = self.state.state.borrow_mut().paint_error.take() {
            return Err(error);
        }
        self.prepare_target_monitor(point, work_area)?;
        let content_changed = self.state.state.borrow().sample != Some((point, rgb));
        if content_changed {
            let color_text = rgb
                .map(|color| format_color(color, ColorFormat::Hex))
                .unwrap_or_else(|| tr("暂不可采样", "Unavailable").to_owned());
            let content = Content {
                rgb,
                color_text: color_text.encode_utf16().collect(),
                coordinates: format!("X {}  Y {}", point.x, point.y)
                    .encode_utf16()
                    .collect(),
            };
            let mut state = self.state.state.borrow_mut();
            state.sample = Some((point, rgb));
            state.content = Some(content);
        }
        let mut changed = content_changed;
        // Moving between mixed-DPI monitors may deliver WM_DPICHANGED during
        // SetWindowPos. A second pass then uses the actual target-window DPI.
        for _ in 0..2 {
            // SAFETY: Query the live window/control handle during its owner or callback lifetime.
            let dpi = unsafe { GetDpiForWindow(self.hwnd) };
            if dpi == 0 || dpi > 9600 {
                return Err(Error::new(E_FAIL, "Could not determine preview DPI"));
            }
            let width = dip(168, dpi);
            let height = dip(
                live_preview_height_dip(self.appearance.border_width_dip),
                dpi,
            );
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
                    tr(
                        "工作区空间不足，无法在避开采样点的位置显示预览",
                        "Not enough screen space to show the preview away from the sampled pixel",
                    ),
                ));
            };
            let rebuild = self
                .state
                .state
                .borrow()
                .surface
                .as_ref()
                .is_none_or(|surface| {
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
                self.state.state.borrow_mut().surface = Some(surface);
                changed = true;
            }
            let reposition = self.rect() != Some(rect);
            // No RefCell borrow may cross a call that can reenter window_proc.
            self.state.state.borrow_mut().dpi_dirty = false;
            if reposition {
                // SAFETY: The owned window is live; all mutable state borrows have ended before possible reentry.
                unsafe {
                    SetWindowPos(
                        self.hwnd,
                        Some(HWND_TOPMOST),
                        rect.left,
                        rect.top,
                        width,
                        height,
                        SWP_NOACTIVATE
                            | if super::window_lifetime::show_native_windows() {
                                SWP_SHOWWINDOW
                            } else {
                                SET_WINDOW_POS_FLAGS(0)
                            },
                    )?
                };
                self.state.state.borrow_mut().rect = Some(rect);
                changed = true;
            }
            if !self.state.state.borrow().dpi_dirty {
                break;
            }
        }
        // SAFETY: Queue painting only for the still-owned overlay; no payload pointer is retained.
        if changed && !unsafe { InvalidateRect(Some(self.hwnd), None, false) }.as_bool() {
            return Err(Error::new(E_FAIL, "Could not invalidate the preview"));
        }
        Ok(changed)
    }

    fn prepare_target_monitor(&self, point: ScreenPointPx, work_area: ScreenRectPx) -> Result<()> {
        if work_area.is_empty() {
            self.hide();
            return Err(Error::new(
                E_FAIL,
                tr(
                    "预览工作区为空",
                    "No screen space is available for the preview",
                ),
            ));
        }
        // SAFETY: Pass an initialized scalar screen point; the API returns a borrowed monitor handle.
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
            return Err(Error::new(
                E_FAIL,
                tr(
                    "预览采样点未位于有效显示器",
                    "The sampled pixel is outside the available displays",
                ),
            ));
        }
        // SAFETY: The owned window is live; all mutable state borrows have ended before possible reentry.
        if unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONULL) } != target {
            // A 400%-DPI window can be wider than a small 100%-DPI monitor even
            // though its correctly scaled preview would fit. Move a hidden,
            // one-pixel window first, then read its real target DPI for layout.
            self.hide();
            // SAFETY: The owned window is live; all mutable state borrows have ended before possible reentry.
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
            // SAFETY: The owner keeps this window live while querying its current borrowed monitor.
            if unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONULL) } != target {
                return Err(Error::new(
                    E_FAIL,
                    tr(
                        "无法将预览定位到采样点所在显示器",
                        "Could not move the preview to the sampled display",
                    ),
                ));
            }
        }
        Ok(())
    }
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        self.state.lifetime.destroy("preview");
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: The registered native procedure supplies the matching message payload and stable userdata.
    catch_unwind(AssertUnwindSafe(|| unsafe {
        window_message(hwnd, message, wparam, lparam)
    }))
    .unwrap_or_else(|_| std::process::abort())
}

/// # Safety
/// Called only by this registered Win32 procedure with the native payload for
/// `message`; callback userdata is a stable Box retained until root termination.
unsafe fn window_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: native creation supplies the stack init for this synchronous
        // call; the stable callback allocation is retained by its Rust owner.
        return unsafe {
            messages::with_window_init(lparam, |init| {
                let callback = &*(init.state as *const CallbackData);
                LRESULT(isize::from(callback.lifetime.attach(hwnd, init)))
            })
        }
        .unwrap_or(LRESULT(0));
    }
    // SAFETY: userdata comes from the stable CallbackData allocation and its
    // owner waits for this root's WM_NCDESTROY before releasing it.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const CallbackData;
    // SAFETY: null means no attachment; otherwise the allocation outlives this call.
    if let Some(callback) = unsafe { pointer.as_ref() } {
        if message == WM_NCDESTROY {
            // SAFETY: finish the native procedure before marking the root terminal.
            let result = unsafe {
                let result = DefWindowProcW(hwnd, message, wparam, lparam);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                result
            };
            callback.lifetime.terminated(hwnd);
            return result;
        }
        let state = &callback.state;
        match message {
            WM_PAINT => {
                // BeginPaint may itself send WM_ERASEBKGND; borrow afterwards.
                // SAFETY: establish WM_PAINT before borrowing mutable callback state.
                let paint = match unsafe { PaintSession::begin(hwnd) } {
                    Ok(paint) => paint,
                    Err(error) => {
                        if let Ok(mut state) = state.try_borrow_mut() {
                            state.paint_error = Some(error);
                        }
                        return LRESULT(0);
                    }
                };
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
    // SAFETY: Forward the original native callback arguments without retaining message storage.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

    struct ReentryProbe {
        callback: *const CallbackData,
        erasures: Cell<u32>,
    }

    /// # Safety
    /// Installed only while the stack ReentryProbe and preview owner remain
    /// alive on this thread; all payloads are synchronous native messages.
    unsafe extern "system" fn probe_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        reference: usize,
    ) -> LRESULT {
        catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: the test removes this subclass before releasing the stack probe.
            let probe = unsafe { &*(reference as *const ReentryProbe) };
            if message == WM_ERASEBKGND {
                // SAFETY: preview's stable callback allocation outlives the probe.
                let callback = unsafe { &*probe.callback };
                let borrow = callback
                    .state
                    .try_borrow_mut()
                    .expect("BeginPaint must precede state borrowing");
                drop(borrow);
                probe.erasures.set(probe.erasures.get() + 1);
                let suggested = RECT {
                    left: 0,
                    top: 0,
                    right: 1,
                    bottom: 1,
                };
                // SAFETY: synchronous DPI fixture has initialized RECT storage,
                // and no state borrow remains across this reentrant message.
                unsafe {
                    SendMessageW(
                        hwnd,
                        WM_DPICHANGED,
                        Some(WPARAM(96 | (96 << 16))),
                        Some(LPARAM(&suggested as *const RECT as isize)),
                    );
                }
            }
            if message == WM_NCDESTROY {
                // SAFETY: remove only this test's installed subclass.
                let _ = unsafe { RemoveWindowSubclass(hwnd, Some(probe_proc), id) };
            }
            // SAFETY: preserve the same native subclass chain and borrowed payload.
            unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
        }))
        .unwrap_or_else(|_| std::process::abort())
    }

    #[test]
    fn begin_paint_reentry_can_deliver_dpi_without_conflicting_state_borrow() {
        // Declare the probe first so window teardown also precedes probe release
        // if a fixture assertion unwinds before explicit subclass removal.
        let mut probe = ReentryProbe {
            callback: std::ptr::null(),
            erasures: Cell::new(0),
        };
        let window = PreviewWindow::new().unwrap();
        probe.callback = window.state.as_ref();
        // SAFETY: window and stack probe remain alive until explicit removal below.
        assert!(
            // SAFETY: window and probe stay alive through subclass removal or native teardown.
            unsafe {
                SetWindowSubclass(
                    window.hwnd,
                    Some(probe_proc),
                    1,
                    &probe as *const ReentryProbe as usize,
                )
            }
            .as_bool()
        );
        // SAFETY: use a zero-alpha, nonactivating one-pixel fixture so Windows
        // maintains a real visible update region without changing desktop pixels.
        unsafe {
            SetLayeredWindowAttributes(window.hwnd, COLORREF(0), 0, LWA_ALPHA).unwrap();
            let _ = ShowWindow(window.hwnd, SW_SHOWNOACTIVATE);
        }
        // SAFETY: invalidate only this transparent owned fixture, then dispatch its
        // WM_PAINT synchronously; BeginPaint delivers the pending erase request.
        unsafe {
            assert!(InvalidateRect(Some(window.hwnd), None, true).as_bool());
            SendMessageW(window.hwnd, WM_PAINT, None, None);
        }
        // SAFETY: remove the subclass before the stack reference can expire.
        assert!(unsafe { RemoveWindowSubclass(window.hwnd, Some(probe_proc), 1) }.as_bool());
        assert!(
            probe.erasures.get() > 0,
            "native BeginPaint must actually exercise the synchronous erase path"
        );
        assert!(window.state.state.borrow().dpi_dirty);
        assert!(window.state.state.borrow().paint_error.is_none());
    }
}
