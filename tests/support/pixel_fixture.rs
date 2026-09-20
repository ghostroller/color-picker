//! Shared by the manual example and the explicitly ignored desktop capture test.
#![allow(dead_code)] // Each consumer uses a different subset of these helpers.

use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
};

use color_picker::core::{color::Rgb8, geometry::ScreenPointPx};
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmFlush, DwmSetWindowAttribute},
            Gdi::*,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources},
        },
        UI::{HiDpi::*, WindowsAndMessaging::*},
    },
    core::{Error, PCWSTR, Result},
};

pub const WIDTH: i32 = 384;
pub const HEIGHT: i32 = 256;
pub const STRIPE_TOP: i32 = HEIGHT - 16;

/// Test executables do not carry the application manifest. Restore their
/// original thread context when the isolated desktop test finishes.
pub struct ScopedPmv2(DPI_AWARENESS_CONTEXT, PhantomData<Rc<()>>);

impl ScopedPmv2 {
    pub fn enter() -> Result<Self> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.0.is_null() {
            return Err(Error::from_thread());
        }
        Ok(Self(previous, PhantomData))
    }
}

impl Drop for ScopedPmv2 {
    fn drop(&mut self) {
        let _ = unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

pub struct PixelFixture {
    // Window destruction clears userdata before the backing Box is released.
    window: FixtureWindow,
    data: Box<FixtureData>,
    _class: FixtureClass,
    _thread: PhantomData<Rc<()>>,
}

struct FixtureData {
    pixels: RefCell<Vec<u8>>,
    draw_failed: Cell<bool>,
}

impl PixelFixture {
    pub fn new() -> Result<Self> {
        let mut pixels = vec![0; (WIDTH * HEIGHT * 4) as usize];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let rgb = if y >= STRIPE_TOP {
                    match x % 3 {
                        0 => Rgb8::new(255, 0, 0),
                        1 => Rgb8::new(0, 255, 0),
                        _ => Rgb8::new(0, 0, 255),
                    }
                } else {
                    Rgb8::new((x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8)
                };
                let offset = ((y * WIDTH + x) * 4) as usize;
                pixels[offset..offset + 4].copy_from_slice(&[rgb.b, rgb.g, rgb.r, 0]);
            }
        }
        let data = Box::new(FixtureData {
            pixels: RefCell::new(pixels),
            draw_failed: Cell::new(false),
        });
        let class = FixtureClass::new()?;
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let mut monitor_info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
            return Err(Error::new(E_FAIL, "Could not locate the fixture work area"));
        }
        let x = monitor_info.rcWork.left + 48;
        let y = monitor_info.rcWork.top + 48;
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let extended = WS_EX_TOOLWINDOW;
        let window = FixtureWindow(unsafe {
            CreateWindowExW(
                extended,
                PCWSTR(class.name.as_ptr()),
                PCWSTR(class.name.as_ptr()),
                style,
                x,
                y,
                WIDTH,
                HEIGHT,
                None,
                None,
                Some(class.instance),
                Some((&*data as *const FixtureData).cast()),
            )?
        });
        let mut bounds = RECT {
            left: 0,
            top: 0,
            right: WIDTH,
            bottom: HEIGHT,
        };
        unsafe {
            // A newly shown window may otherwise be sampled during its DWM
            // fade-in. Disable transitions on this fixture only, leaving the
            // user's global animation preferences unchanged.
            let disabled: i32 = 1;
            DwmSetWindowAttribute(
                window.0,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&disabled as *const i32).cast(),
                std::mem::size_of::<i32>() as u32,
            )?;
            AdjustWindowRectExForDpi(
                &mut bounds,
                style,
                false,
                extended,
                GetDpiForWindow(window.0),
            )?;
            SetWindowPos(
                window.0,
                Some(HWND_TOPMOST),
                x,
                y,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                SWP_NOACTIVATE,
            )?;
            let _ = ShowWindow(window.0, SW_SHOWNOACTIVATE);
        }
        let fixture = Self {
            window,
            data,
            _class: class,
            _thread: PhantomData,
        };
        fixture.update_title()?;
        fixture.present()?;
        Ok(fixture)
    }

    pub fn origin(&self) -> Result<ScreenPointPx> {
        origin(self.window.0)
    }

    pub fn screen_point(&self, x: i32, y: i32) -> Result<ScreenPointPx> {
        check_pixel(x, y)?;
        let origin = self.origin()?;
        Ok(ScreenPointPx {
            x: origin.x + x,
            y: origin.y + y,
        })
    }

    pub fn change_pixel(&self, x: i32, y: i32, rgb: Rgb8) -> Result<()> {
        check_pixel(x, y)?;
        {
            let mut pixels = self.data.pixels.borrow_mut();
            let offset = ((y * WIDTH + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&[rgb.b, rgb.g, rgb.r, 0]);
        }
        self.present()
    }

    pub fn present(&self) -> Result<()> {
        self.data.draw_failed.set(false);
        unsafe {
            SetWindowPos(
                self.window.0,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )?;
            if !InvalidateRect(Some(self.window.0), None, false).as_bool() {
                return Err(Error::new(E_FAIL, "Could not invalidate the fixture"));
            }
            let _ = UpdateWindow(self.window.0);
            if self.data.draw_failed.get() {
                return Err(Error::new(
                    E_FAIL,
                    "Could not draw the fixture's exact pixels",
                ));
            }
            DwmFlush()?;
        }
        Ok(())
    }

    pub fn run(&self) -> Result<()> {
        while unsafe { IsWindow(Some(self.window.0)) }.as_bool() {
            let mut message = MSG::default();
            match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
                -1 => return Err(Error::from_thread()),
                0 => return Ok(()),
                _ => unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                },
            }
        }
        Ok(())
    }

    fn update_title(&self) -> Result<()> {
        update_title(self.window.0)
    }
}

pub fn gdi_objects() -> u32 {
    unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) }
}

fn check_pixel(x: i32, y: i32) -> Result<()> {
    if !(0..WIDTH).contains(&x) || !(0..HEIGHT).contains(&y) {
        return Err(Error::new(E_INVALIDARG, "Fixture pixel is out of range"));
    }
    Ok(())
}

fn origin(hwnd: HWND) -> Result<ScreenPointPx> {
    let mut point = POINT::default();
    if !unsafe { ClientToScreen(hwnd, &mut point) }.as_bool() {
        return Err(Error::new(
            E_FAIL,
            "Could not locate the fixture's physical origin",
        ));
    }
    Ok(ScreenPointPx {
        x: point.x,
        y: point.y,
    })
}

fn update_title(hwnd: HWND) -> Result<()> {
    let point = origin(hwnd)?;
    let title: Vec<u16> = format!("Pixels: origin=({}, {}) | 1:1 SDR", point.x, point.y)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe { SetWindowTextW(hwnd, PCWSTR(title.as_ptr())) }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    std::panic::catch_unwind(|| unsafe { window_proc_inner(hwnd, message, wparam, lparam) })
        .unwrap_or_else(|_| std::process::abort())
}

unsafe fn window_proc_inner(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_NCCREATE => {
            let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
            LRESULT(1)
        }
        WM_NCDESTROY => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_MOVE => {
            let _ = update_title(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const FixtureData;
            if pointer.is_null() {
                return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
            }
            // Box ownership stays with PixelFixture until after DestroyWindow.
            let data = unsafe { &*pointer };
            let mut paint = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: WIDTH,
                    biHeight: -HEIGHT,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let rows = {
                let pixels = data.pixels.borrow();
                // Equal source/destination sizes copy physical pixels with no
                // interpolation, antialiasing, text rendering, or image files.
                unsafe {
                    StretchDIBits(
                        dc,
                        0,
                        0,
                        WIDTH,
                        HEIGHT,
                        0,
                        0,
                        WIDTH,
                        HEIGHT,
                        Some(pixels.as_ptr().cast()),
                        &info,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    )
                }
            };
            if rows != HEIGHT {
                data.draw_failed.set(true);
            }
            let _ = unsafe { EndPaint(hwnd, &paint) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

struct FixtureWindow(HWND);
impl Drop for FixtureWindow {
    fn drop(&mut self) {
        if unsafe { IsWindow(Some(self.0)) }.as_bool() {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }
}

struct FixtureClass {
    name: Vec<u16>,
    instance: HINSTANCE,
}
impl FixtureClass {
    fn new() -> Result<Self> {
        static SERIAL: AtomicU32 = AtomicU32::new(0);
        let name: Vec<u16> = format!(
            "ColorPicker.PixelFixture.{}.{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        )
        .encode_utf16()
        .chain(Some(0))
        .collect();
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None)? }.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(Error::from_thread());
        }
        Ok(Self { name, instance })
    }
}
impl Drop for FixtureClass {
    fn drop(&mut self) {
        let _ = unsafe { UnregisterClassW(PCWSTR(self.name.as_ptr()), Some(self.instance)) };
    }
}
