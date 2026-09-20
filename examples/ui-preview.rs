//! A visual fixture using the production native windows and synthetic pixels.
//! No hotkeys, hooks, screen sampling or configuration writes are started.
//! cargo run --example ui-preview -- result [seconds]
//! Modes: result, settings, live, frozen, frozen-edge. Copy buttons use the clipboard
//! only when explicitly clicked. Settings Apply validates but never saves.

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
            Foundation::HWND,
            Graphics::Gdi::UpdateWindow,
            UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
        },
        core::{Error, Result, w},
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
        let rgb = Rgb8::new(143, 185, 158);
        let scene = match mode.as_str() {
            "result" => Scene::Result(ResultWindow::new(
                PickedColor {
                    rgb,
                    source: focus,
                    kind: SampleKind::Live,
                },
                owner.0,
            )?),
            "settings" => Scene::Settings(SettingsWindow::new(
                &Config::default(),
                owner.0,
                None,
                true,
            )?),
            "live" => {
                let window = PreviewWindow::new()?;
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
                let window = MagnifierWindow::new(
                    FrozenImage {
                        origin,
                        width: width as u32,
                        height: 65,
                        stride_bytes: width * 4,
                        bgrx,
                    },
                    focus,
                    work,
                )?;
                let bounds = window.rect().expect("shown magnifier");
                let footer_height = (24 * unsafe { GetDpiForWindow(window.hwnd()) } + 48) / 96;
                window.update_hover(ScreenPointPx {
                    x: bounds.left + (bounds.width() / 2) as i32,
                    y: bounds.top + ((bounds.height() - footer_height) / 2) as i32,
                })?;
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
            // Only this synthetic fixture is capturable. Production overlays
            // retain WDA_EXCLUDEFROMCAPTURE for sampling correctness.
            let _ = SetWindowDisplayAffinity(scene.hwnd(), WDA_NONE);
            let _ = UpdateWindow(scene.hwnd());
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
}
