//! A visual fixture using the production native windows and synthetic pixels.
//! No hotkeys, hooks, color sampling or configuration writes are started.
//! The information strip may read its local backdrop to render frosted glass.
//! cargo run --example ui-preview -- result [seconds]
//! Modes: result, settings, live, frozen, frozen-edge. Copy buttons use the clipboard
//! only when explicitly clicked. Settings Apply validates but never saves.
//! Add --backdrop after the lifetime to preview material over a synthetic pattern.
//! --border=N / --transparency=N override appearance for this fixture only.
//! --output=path.bmp exports this fixture's client area, then exits.

#[cfg(not(windows))]
fn main() {
    eprintln!("ui-preview requires Windows");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    fixture::run()
}

#[cfg(windows)]
mod fixture {
    use color_picker::{
        app::config::Config,
        core::{
            color::Rgb8,
            geometry::ScreenPointPx,
            state::{PickedColor, SampleKind},
            zoom::FrozenImage,
        },
        platform::windows::{check_environment, monitors::Monitors, session::cursor_position},
        ui::windows::{
            magnifier::MagnifierWindow,
            preview::PreviewWindow,
            result::ResultWindow,
            settings::{SettingsAction, SettingsWindow},
        },
    };
    use windows::{
        Win32::{
            Foundation::{COLORREF, E_FAIL, HWND, LPARAM, LRESULT, RECT, WPARAM},
            Graphics::{Dwm::DwmFlush, Gdi::*},
            System::LibraryLoader::GetModuleHandleW,
            UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
        },
        core::{BOOL, Error, Result, w},
    };

    enum Scene {
        Result(ResultWindow),
        Settings(SettingsWindow),
        Live(PreviewWindow),
        Frozen(MagnifierWindow),
    }

    impl Scene {
        fn hwnd(&self) -> HWND {
            match self {
                Self::Result(window) => window.hwnd(),
                Self::Settings(window) => window.hwnd(),
                Self::Live(window) => window.hwnd(),
                Self::Frozen(window) => window.hwnd(),
            }
        }

        fn process(&self) -> Result<bool> {
            match self {
                Self::Result(window) => Ok(window.process_pending()?.is_some()),
                Self::Settings(window) => match window.process_pending()? {
                    Some(SettingsAction::Close) => Ok(true),
                    Some(SettingsAction::Apply(_)) => {
                        window.show_status("设置有效。当前为界面预览，未写入配置。", true)?;
                        Ok(false)
                    }
                    None => Ok(false),
                },
                _ => Ok(false),
            }
        }
    }

    struct Owner(HWND);
    impl Drop for Owner {
        fn drop(&mut self) {
            let _ = unsafe { KillTimer(Some(self.0), 1) };
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }

    pub fn run() -> Result<()> {
        check_environment()?;
        let mode = std::env::args().nth(1).unwrap_or_else(|| "result".into());
        let mut config = Config::default();
        for arg in std::env::args().skip(2) {
            let value = if let Some(value) = arg.strip_prefix("--border=") {
                Some((&mut config.appearance.border_width_dip, value))
            } else {
                arg.strip_prefix("--transparency=").map(|value| {
                    (
                        &mut config.appearance.background_transparency_percent,
                        value,
                    )
                })
            };
            if let Some((target, value)) = value {
                *target = value.parse().map_err(|_| {
                    Error::new(
                        windows::Win32::Foundation::E_INVALIDARG,
                        "Appearance options require whole numbers",
                    )
                })?;
            }
        }
        config.validate().map_err(|error| {
            Error::new(windows::Win32::Foundation::E_INVALIDARG, error.to_string())
        })?;
        let seconds = std::env::args()
            .nth(2)
            .and_then(|arg| arg.parse::<u32>().ok())
            .unwrap_or(60)
            .clamp(1, 600);
        let cursor = cursor_position()?;
        let monitors = Monitors::enumerate()?;
        let work = monitors.at(cursor).expect("cursor monitor").work_area;
        let focus = ScreenPointPx {
            x: work.left + (work.width() / 2) as i32,
            y: work.top + (work.height() / 2) as i32,
        };
        let _backdrop = if std::env::args().any(|arg| arg == "--backdrop") {
            Some(backdrop(focus)?)
        } else {
            None
        };
        let owner = Owner(unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Color Picker UI fixture"),
                WS_OVERLAPPED,
                focus.x,
                focus.y,
                1,
                1,
                None,
                None,
                None,
                None,
            )?
        });
        let mut rgb = Rgb8::new(143, 185, 158);
        for argument in std::env::args().skip(2) {
            if let Some(hex) = argument.strip_prefix("--color=") {
                if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(Error::new(
                        windows::Win32::Foundation::E_INVALIDARG,
                        "--color requires six hexadecimal digits, e.g. --color=498BA7",
                    ));
                }
                let color = u32::from_str_radix(hex, 16).expect("validated six hex digits");
                rgb = Rgb8::new((color >> 16) as u8, (color >> 8) as u8, color as u8);
            }
        }
        let scene = match mode.as_str() {
            "result" => Scene::Result(ResultWindow::new_with_appearance(
                PickedColor {
                    rgb,
                    source: focus,
                    kind: SampleKind::Live,
                },
                owner.0,
                config.default_format,
                false,
                config.appearance,
            )?),
            "settings" => Scene::Settings(SettingsWindow::new(&config, owner.0, None, true)?),
            "live" => {
                let window = PreviewWindow::with_appearance(config.appearance)?;
                window.update(focus, Some(rgb), work)?;
                Scene::Live(window)
            }
            "frozen" | "frozen-edge" => {
                let width = if mode == "frozen-edge" { 33_usize } else { 65 };
                let origin = ScreenPointPx {
                    x: focus.x - (width / 2) as i32,
                    y: focus.y - 32,
                };
                let mut bgrx = Vec::with_capacity(width * 65 * 4);
                for y in 0..65 {
                    for x in 0..width {
                        let cell = ((x / 8) + (y / 8)) % 2;
                        let color = if cell == 0 {
                            rgb
                        } else {
                            Rgb8::new(207, 225, 213)
                        };
                        bgrx.extend_from_slice(&[color.b, color.g, color.r, 255]);
                    }
                }
                let window = MagnifierWindow::with_appearance(
                    FrozenImage {
                        origin,
                        width: width as u32,
                        height: 65,
                        stride_bytes: width * 4,
                        bgrx,
                    },
                    focus,
                    work,
                    config.appearance,
                )?;
                window.update_hover(focus)?;
                Scene::Frozen(window)
            }
            _ => {
                return Err(Error::new(
                    windows::Win32::Foundation::E_INVALIDARG,
                    "Mode must be result, settings, live, frozen, or frozen-edge",
                ));
            }
        };
        scene.process()?;
        unsafe {
            // Cache the material while this overlay is still excluded from
            // capture. PrintWindow can then use it without sampling itself.
            let _ = UpdateWindow(scene.hwnd());
            // Only this synthetic fixture is capturable. Production overlays
            // retain WDA_EXCLUDEFROMCAPTURE for sampling correctness.
            let _ = SetWindowDisplayAffinity(scene.hwnd(), WDA_NONE);
            let _ = UpdateWindow(scene.hwnd());
            // Let the compositor finish the capture-affinity change before
            // exporting a layered preview's own backing surface.
            DwmFlush()?;
        }
        if let Some(output) = std::env::args()
            .find_map(|arg| arg.strip_prefix("--output=").map(std::path::PathBuf::from))
        {
            export_bitmap(scene.hwnd(), &output)?;
            println!("Exported {mode} fixture to {}", output.display());
            return Ok(());
        }
        unsafe {
            if SetTimer(Some(owner.0), 1, seconds * 1000, None) == 0 {
                return Err(Error::from_thread());
            }
        }
        println!(
            "mode={mode} hwnd={} dpi={} timeout={seconds}s",
            scene.hwnd().0 as usize,
            unsafe { GetDpiForWindow(scene.hwnd()) }
        );
        let mut message = MSG::default();
        loop {
            let received = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
            if received == -1 {
                return Err(Error::from_thread());
            }
            if received == 0 || (message.hwnd == owner.0 && message.message == WM_TIMER) {
                break;
            }
            let dialog = matches!(scene, Scene::Result(_) | Scene::Settings(_));
            let recorded_key = match &scene {
                Scene::Settings(window) => window.filter_key_message(&message),
                _ => false,
            };
            if !recorded_key
                && (!dialog || !unsafe { IsDialogMessageW(scene.hwnd(), &message) }.as_bool())
            {
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            if scene.process()? {
                break;
            }
        }
        Ok(())
    }

    // This user32 API is exposed under windows-rs's Storage_Xps feature. Keep
    // its binding local to the fixture rather than add that production feature.
    #[link(name = "user32")]
    unsafe extern "system" {
        fn PrintWindow(hwnd: HWND, target: HDC, flags: u32) -> BOOL;
    }

    struct ExportSurface {
        dc: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
    }

    impl Drop for ExportSurface {
        fn drop(&mut self) {
            unsafe {
                if !self.previous.is_invalid() {
                    SelectObject(self.dc, self.previous);
                }
                if !self.bitmap.is_invalid() {
                    let _ = DeleteObject(self.bitmap.into());
                }
                let _ = DeleteDC(self.dc);
            }
        }
    }

    fn export_bitmap(hwnd: HWND, output: &std::path::Path) -> Result<()> {
        use std::io::Write;

        let mut client = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client)? };
        let width = client.right - client.left;
        let height = client.bottom - client.top;
        let pixel_bytes = width
            .checked_mul(height)
            .and_then(|area| area.checked_mul(4))
            .filter(|bytes| width > 0 && height > 0 && *bytes <= 64 * 1024 * 1024)
            .ok_or_else(|| Error::new(E_FAIL, "Invalid fixture export dimensions"))?
            as u32;
        let mut surface = ExportSurface {
            dc: unsafe { CreateCompatibleDC(None) },
            bitmap: HBITMAP::default(),
            previous: HGDIOBJ::default(),
        };
        if surface.dc.is_invalid() {
            return Err(Error::from_thread());
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: 40,
                biWidth: width,
                biHeight: -height, // Top-down BGRX matches the exported BMP.
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: pixel_bytes,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = std::ptr::null_mut();
        surface.bitmap = unsafe {
            CreateDIBSection(
                Some(surface.dc),
                &info,
                DIB_RGB_COLORS,
                &mut pixels,
                None,
                0,
            )?
        };
        if pixels.is_null() {
            return Err(Error::new(E_FAIL, "Fixture bitmap has no pixel storage"));
        }
        surface.previous = unsafe { SelectObject(surface.dc, surface.bitmap.into()) };
        if surface.previous.is_invalid() {
            return Err(Error::new(E_FAIL, "Could not select fixture bitmap"));
        }
        unsafe {
            let _ = RedrawWindow(
                Some(hwnd),
                None,
                None,
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
            );
            // Print only this process's synthetic fixture into an offscreen
            // bitmap. No screen DC or pixels from other windows are read here.
            if !PrintWindow(hwnd, surface.dc, 1 | PW_RENDERFULLCONTENT).as_bool() {
                return Err(Error::new(E_FAIL, "Could not print fixture window"));
            }
            if !GdiFlush().as_bool() {
                return Err(Error::new(E_FAIL, "Could not flush fixture bitmap"));
            }
        }
        let pixels =
            unsafe { std::slice::from_raw_parts(pixels.cast::<u8>(), pixel_bytes as usize) };
        // Write the 14-byte file and 40-byte DIB headers explicitly, avoiding
        // Rust struct padding and any dependency on an image encoding crate.
        let mut header = Vec::with_capacity(54);
        header.extend_from_slice(b"BM");
        header.extend_from_slice(&(54 + pixel_bytes).to_le_bytes());
        header.extend_from_slice(&[0; 4]);
        header.extend_from_slice(&54_u32.to_le_bytes());
        header.extend_from_slice(&40_u32.to_le_bytes());
        header.extend_from_slice(&width.to_le_bytes());
        header.extend_from_slice(&(-height).to_le_bytes());
        header.extend_from_slice(&1_u16.to_le_bytes());
        header.extend_from_slice(&32_u16.to_le_bytes());
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(&pixel_bytes.to_le_bytes());
        header.extend_from_slice(&[0; 16]);
        let write = || -> std::io::Result<()> {
            let mut file = std::fs::File::create(output)?;
            file.write_all(&header)?;
            file.write_all(pixels)
        };
        write().map_err(|error| Error::new(E_FAIL, format!("Could not save fixture: {error}")))
    }

    fn backdrop(focus: ScreenPointPx) -> Result<Owner> {
        let instance = unsafe { GetModuleHandleW(None)? }.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(backdrop_proc),
            hInstance: instance,
            lpszClassName: w!("ColorPicker.MaterialFixture"),
            ..Default::default()
        };
        unsafe { RegisterClassW(&class) };
        let window = Owner(unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.lpszClassName,
                w!("Synthetic material backdrop"),
                WS_POPUP,
                focus.x - 180,
                focus.y - 180,
                600,
                420,
                None,
                None,
                Some(instance),
                None,
            )?
        });
        unsafe {
            let _ = ShowWindow(window.0, SW_SHOWNOACTIVATE);
            let _ = UpdateWindow(window.0);
            DwmFlush()?;
        }
        Ok(window)
    }

    unsafe extern "system" fn backdrop_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_PAINT {
            let mut paint = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            let brush = HBRUSH(unsafe { GetStockObject(DC_BRUSH) }.0);
            for row in 0..7 {
                for col in 0..10 {
                    let color = if (row + col) % 2 == 0 {
                        COLORREF(0x00dbc5a1)
                    } else {
                        COLORREF(0x0087a5d6)
                    };
                    let rect = RECT {
                        left: col * 60,
                        top: row * 60,
                        right: (col + 1) * 60,
                        bottom: (row + 1) * 60,
                    };
                    unsafe {
                        SetDCBrushColor(dc, color);
                        FillRect(dc, &rect, brush);
                    }
                }
            }
            let _ = unsafe { EndPaint(hwnd, &paint) };
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}
