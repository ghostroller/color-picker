//! Isolated, hidden native lifecycle regression. No hooks, global input or clipboard.
use super::{
    magnifier::MagnifierWindow, preview::PreviewWindow, result::ResultWindow,
    settings::SettingsWindow, window_lifetime::with_hidden_windows,
};
use crate::{
    app::config::Config,
    core::{
        color::Rgb8,
        geometry::ScreenPointPx,
        state::{PickedColor, SampleKind},
        zoom::FrozenImage,
    },
    platform::windows::{
        capture::GdiSampler,
        dib::{Dib32Layout, Dib32Surface},
        gdi::DesktopDc,
        monitors::Monitors,
    },
};
use windows::{
    Win32::{
        Foundation::{LPARAM, POINT, WPARAM},
        Graphics::Gdi::{InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
        System::Threading::{
            GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources,
            GetProcessHandleCount,
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

#[derive(Clone, Copy, Debug)]
struct Counts {
    handles: u32,
    gdi: u32,
    user: u32,
}
fn counts() -> Counts {
    let mut handles = 0;
    // SAFETY: current process is a borrowed pseudo-handle; handles is writable
    // scalar output and both GUI counters query this test process only.
    unsafe {
        let process = GetCurrentProcess();
        GetProcessHandleCount(process, &mut handles).unwrap();
        Counts {
            handles,
            gdi: GetGuiResources(process, GR_GDIOBJECTS),
            user: GetGuiResources(process, GR_USEROBJECTS),
        }
    }
}

#[test]
#[ignore = "isolated native resource measurement; run this exact filter with one test thread"]
fn four_hidden_windows_and_surfaces_plateau_after_warmup() {
    // SAFETY: this test thread alone changes its DPI context; restore before exit.
    let previous_dpi =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    assert!(!previous_dpi.is_invalid());
    let monitors = Monitors::enumerate().unwrap();
    // SAFETY: scalar physical point used only to find an actual primary monitor.
    let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    assert!(!primary.is_invalid());
    let work = monitors
        .at(ScreenPointPx { x: 0, y: 0 })
        .expect("primary origin is on a monitor")
        .work_area;
    let focus = ScreenPointPx {
        x: work.left + work.width() as i32 / 2,
        y: work.top + work.height() as i32 / 2,
    };
    // SAFETY: create a fresh hidden notification owner without native userdata.
    let owner = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Resource regression owner"),
            WS_OVERLAPPED,
            0,
            0,
            1,
            1,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    };
    let mut config = Config::default();
    config.appearance.background_transparency_percent = 0;
    let cycle = || {
        with_hidden_windows(|| {
            let preview = PreviewWindow::with_appearance(config.appearance).unwrap();
            preview
                .update(focus, Some(Rgb8::new(73, 167, 198)), work)
                .unwrap();
            let magnifier = MagnifierWindow::with_appearance(
                FrozenImage {
                    origin: ScreenPointPx {
                        x: focus.x - 32,
                        y: focus.y - 32,
                    },
                    width: 65,
                    height: 65,
                    stride_bytes: 260,
                    bgrx: vec![127; 65 * 65 * 4],
                },
                focus,
                work,
                config.appearance,
            )
            .unwrap();
            let result = ResultWindow::new_with_appearance(
                PickedColor {
                    rgb: Rgb8::new(73, 167, 198),
                    source: focus,
                    kind: SampleKind::Frozen,
                },
                owner,
                config.default_format,
                false,
                config.appearance,
            )
            .unwrap();
            let settings = SettingsWindow::new(&config, owner, None, true).unwrap();
            for hwnd in [
                preview.hwnd(),
                magnifier.hwnd(),
                result.hwnd(),
                settings.hwnd(),
            ] {
                // SAFETY: only these still-owned hidden windows receive a synchronous
                // paint request; their callback storage outlives this invocation.
                unsafe {
                    assert!(!IsWindowVisible(hwnd).as_bool());
                    let _ = InvalidateRect(Some(hwnd), None, true);
                    SendMessageW(hwnd, WM_PAINT, Some(WPARAM(0)), Some(LPARAM(0)));
                }
            }
            preview
                .update(focus, Some(Rgb8::new(73, 167, 198)), work)
                .unwrap();
            magnifier.update_hover(focus).unwrap();
            let _sampler = GdiSampler::new().unwrap();
            let desktop = DesktopDc::new().unwrap();
            let mut dib =
                Dib32Surface::new(desktop.raw(), Dib32Layout::new(17, 13).unwrap()).unwrap();
            dib.with_pixels_mut(|pixels| pixels.fill(42)).unwrap();
            assert_eq!(dib.with_pixels(|pixels| pixels[0]).unwrap(), 42);
        })
    };
    for _ in 0..10 {
        cycle();
    }
    let warm = counts();
    for _ in 0..25 {
        cycle();
    }
    let first = counts();
    for _ in 0..25 {
        cycle();
    }
    let second = counts();
    eprintln!("RESOURCE_REGRESSION warmup10={warm:?} group25={first:?} group50={second:?}");
    assert!(
        first.gdi <= warm.gdi + 1 && second.gdi <= first.gdi,
        "GDI trend: {warm:?} {first:?} {second:?}"
    );
    assert!(
        first.user <= warm.user + 1 && second.user <= first.user,
        "USER trend: {warm:?} {first:?} {second:?}"
    );
    assert!(
        first.handles <= warm.handles + 2 && second.handles <= first.handles,
        "handle trend: {warm:?} {first:?} {second:?}"
    );
    // SAFETY: all callback-bearing windows have dropped before this plain STATIC
    // owner; restoring this thread's original DPI context affects no other thread.
    unsafe {
        DestroyWindow(owner).unwrap();
        SetThreadDpiAwarenessContext(previous_dpi);
    }
}
