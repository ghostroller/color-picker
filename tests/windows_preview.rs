#![cfg(windows)]

//! Explicit interactive-desktop checks; never part of ordinary cargo test.
//! Run on a stable display configuration without switching foreground apps:
//! cargo test --test windows_preview --locked -- --ignored --test-threads=1 --nocapture
//! No input is synthesized and no captured pixels are saved to disk.

use std::sync::Mutex;

use color_picker::core::color::Rgb8;
use color_picker::core::geometry::{ScreenPointPx, ScreenRectPx};
use color_picker::platform::windows::capture::GdiSampler;
use color_picker::platform::windows::monitors::Monitors;
use color_picker::platform::windows::session::{cursor_position, flush_composition};
use color_picker::ui::windows::preview::PreviewWindow;
use windows::Win32::Foundation::{ERROR_SUCCESS, GetLastError, HWND, RECT, SetLastError};
use windows::Win32::Graphics::Gdi::{GetUpdateRect, UpdateWindow};
use windows::Win32::System::Threading::{
    GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowDisplayAffinity, GetWindowLongW,
    GetWindowRect, IsWindowVisible, SetWindowDisplayAffinity, WDA_NONE, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

#[path = "support/pixel_fixture.rs"]
mod pixel_fixture;

use pixel_fixture::{PixelFixture, ScopedPmv2};

static DESKTOP_TEST: Mutex<()> = Mutex::new(());
const FIRST_COLOR: Rgb8 = Rgb8 {
    r: 64,
    g: 158,
    b: 255,
};
const SECOND_COLOR: Rgb8 = Rgb8 {
    r: 230,
    g: 20,
    b: 90,
};

#[test]
#[ignore = "requires an interactive Windows desktop, stable DPI/work area, and no foreground-app changes"]
fn preview_nonactivating_updates_and_resource_lifecycle() {
    let _serial = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _dpi = ScopedPmv2::enter().expect("the test thread must support PerMonitorV2");
    let point = cursor_position().expect("GetCursorPos requires an accessible desktop");
    let monitors = Monitors::enumerate().expect("enumerate the current physical monitors");
    let work_area = monitors
        .at(point)
        .expect("cursor must be on an actual monitor")
        .work_area;
    let foreground = unsafe { GetForegroundWindow() };

    {
        let preview = PreviewWindow::new().expect("create nonactivating preview");
        assert!(!unsafe { IsWindowVisible(preview.hwnd()) }.as_bool());
        assert!(preview.update(point, Some(FIRST_COLOR), work_area).unwrap());
        paint(&preview);
        assert_visible_beside(&preview, point, work_area);
        assert_foreground(foreground);

        let required = WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW;
        let extended_style = unsafe { GetWindowLongW(preview.hwnd(), GWL_EXSTYLE) } as u32;
        assert_eq!(extended_style & required.0, required.0);
        assert_ne!(extended_style & WS_EX_TOPMOST.0, 0);
        assert_ne!(
            unsafe { GetWindowLongW(preview.hwnd(), GWL_STYLE) } as u32 & WS_POPUP.0,
            0
        );

        assert!(!preview.update(point, Some(FIRST_COLOR), work_area).unwrap());
        assert!(
            !unsafe { GetUpdateRect(preview.hwnd(), None, false) }.as_bool(),
            "an unchanged sample must not schedule another paint"
        );
        assert!(
            preview
                .update(point, Some(SECOND_COLOR), work_area)
                .unwrap()
        );
        paint(&preview);
        assert!(
            !preview
                .update(point, Some(SECOND_COLOR), work_area)
                .unwrap()
        );
        assert!(preview.update(point, None, work_area).unwrap());
        paint(&preview);
        assert!(!preview.update(point, None, work_area).unwrap());

        preview.hide();
        assert_eq!(preview.rect(), None);
        assert!(!unsafe { IsWindowVisible(preview.hwnd()) }.as_bool());
        assert!(preview.update(point, Some(FIRST_COLOR), work_area).unwrap());
        paint(&preview);
        assert_visible_beside(&preview, point, work_area);
        assert_foreground(foreground);

        let tiny = ScreenRectPx {
            left: work_area.left,
            top: work_area.top,
            right: work_area.left + 1,
            bottom: work_area.top + 1,
        };
        assert!(
            preview.update(point, Some(FIRST_COLOR), tiny).is_err(),
            "a session without room for visible feedback must report failure"
        );
        assert_eq!(preview.rect(), None);
        assert!(!unsafe { IsWindowVisible(preview.hwnd()) }.as_bool());
    }

    // RegisterClass and first-use GDI/font caches are process-scoped. Warm them
    // before measuring repeated session ownership rather than charging them to
    // a leak. Each cycle uses one stable coordinate and display configuration.
    for _ in 0..5 {
        preview_cycle(point, work_area);
    }
    let baseline = gui_counts();
    let mut after_25 = baseline;
    for cycle in 1..=100 {
        preview_cycle(point, work_area);
        if cycle == 25 {
            after_25 = gui_counts();
        }
    }
    let after_100 = gui_counts();
    eprintln!(
        "preview GUI resources: baseline={baseline:?}, after_25={after_25:?}, after_100={after_100:?}"
    );
    for (label, initial, middle, final_count) in [
        ("GDI", baseline.gdi, after_25.gdi, after_100.gdi),
        ("USER", baseline.user, after_25.user, after_100.user),
    ] {
        assert!(
            middle <= initial + 2,
            "{label} resources grew after 25 sessions: {initial} -> {middle}"
        );
        assert!(
            final_count <= initial + 2,
            "{label} resources grew after 100 sessions: {initial} -> {final_count}"
        );
        assert!(
            final_count <= middle + 2,
            "{label} resources continued growing between 25 and 100 sessions: {middle} -> {final_count}"
        );
    }
    assert_foreground(foreground);
}

#[test]
#[ignore = "requires an interactive SDR desktop, stable display settings, and unobscured pixel fixture"]
fn hiding_an_intersecting_preview_restores_exact_underlying_sampling() {
    let _serial = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _dpi = ScopedPmv2::enter().unwrap();
    let fixture = PixelFixture::new().unwrap();
    let origin = fixture.origin().unwrap();
    let initial_point = fixture.screen_point(0, 0).unwrap();
    let monitors = Monitors::enumerate().unwrap();
    let work_area = monitors.at(initial_point).unwrap().work_area;
    let preview = PreviewWindow::new().unwrap();
    // Prove hide + composition synchronization works without the auxiliary
    // capture exclusion masking any remaining preview content.
    unsafe { SetWindowDisplayAffinity(preview.hwnd(), WDA_NONE) }.unwrap();
    let mut affinity = u32::MAX;
    unsafe { GetWindowDisplayAffinity(preview.hwnd(), &mut affinity) }.unwrap();
    assert_eq!(affinity, WDA_NONE.0);
    assert!(
        preview
            .update(initial_point, Some(FIRST_COLOR), work_area)
            .unwrap()
    );
    paint(&preview);
    let fixture_bounds = ScreenRectPx {
        left: origin.x,
        top: origin.y,
        right: origin.x + pixel_fixture::WIDTH,
        bottom: origin.y + pixel_fixture::HEIGHT,
    };
    let overlap = preview
        .rect()
        .unwrap()
        .intersection(fixture_bounds)
        .expect("this display must allow the preview to overlap the controlled fixture");
    let covered_point = ScreenPointPx {
        x: overlap.left,
        y: overlap.top,
    };
    let expected = Rgb8 {
        r: 17,
        g: 221,
        b: 93,
    };
    // Changing the fixture raises it. Prepare its known pixel, then show the
    // preview again so its old rectangle really lies above that source pixel.
    preview.hide();
    fixture
        .change_pixel(
            covered_point.x - origin.x,
            covered_point.y - origin.y,
            expected,
        )
        .unwrap();
    assert!(
        preview
            .update(initial_point, Some(FIRST_COLOR), work_area)
            .unwrap()
    );
    paint(&preview);
    assert!(preview.rect().unwrap().contains(covered_point));

    // Simulate the new sample coordinate without moving the actual cursor. This
    // is the explicit correctness path, independent of capture-exclusion flags.
    if preview
        .rect()
        .is_some_and(|rect| rect.contains(covered_point))
    {
        preview.hide();
        flush_composition().unwrap();
    }
    assert!(!unsafe { IsWindowVisible(preview.hwnd()) }.as_bool());
    let mut sampler = GdiSampler::new().unwrap();
    let sampled = sampler.sample_pixel(covered_point).unwrap();
    assert_eq!(
        sampled, expected,
        "the hidden preview must not contaminate the source pixel"
    );
    assert!(
        preview
            .update(covered_point, Some(sampled), work_area)
            .unwrap()
    );
    paint(&preview);
    assert_visible_beside(&preview, covered_point, work_area);
    assert!(
        !preview
            .update(covered_point, Some(sampled), work_area)
            .unwrap()
    );
}

fn preview_cycle(point: ScreenPointPx, work_area: ScreenRectPx) {
    let preview = PreviewWindow::new().unwrap();
    assert!(preview.update(point, Some(FIRST_COLOR), work_area).unwrap());
    paint(&preview);
    assert_visible_beside(&preview, point, work_area);
    // Drawing failures are reported by the next update, so exercise that path.
    assert!(!preview.update(point, Some(FIRST_COLOR), work_area).unwrap());
}

fn paint(preview: &PreviewWindow) {
    assert!(
        unsafe { UpdateWindow(preview.hwnd()) }.as_bool(),
        "UpdateWindow failed"
    );
    assert!(
        !unsafe { GetUpdateRect(preview.hwnd(), None, false) }.as_bool(),
        "BeginPaint/EndPaint must consume the update region"
    );
}

fn assert_visible_beside(preview: &PreviewWindow, point: ScreenPointPx, work_area: ScreenRectPx) {
    assert!(unsafe { IsWindowVisible(preview.hwnd()) }.as_bool());
    let rect = preview
        .rect()
        .expect("the work area must have room for a preview beside this point");
    let dpi = unsafe { GetDpiForWindow(preview.hwnd()) };
    assert!(dpi > 0);
    assert_eq!(
        rect.width(),
        (208 * dpi + 48) / 96,
        "preview width must use its current window DPI"
    );
    assert_eq!(
        rect.height(),
        (58 * dpi + 48) / 96,
        "preview height must use its current window DPI"
    );
    assert!(
        !rect.contains(point),
        "preview must never cover its sampled point"
    );
    assert_eq!(rect.intersection(work_area), Some(rect));
    let mut actual = RECT::default();
    unsafe { GetWindowRect(preview.hwnd(), &mut actual) }.unwrap();
    assert_eq!(
        rect,
        ScreenRectPx {
            left: actual.left,
            top: actual.top,
            right: actual.right,
            bottom: actual.bottom
        }
    );
}

fn assert_foreground(expected: HWND) {
    assert_eq!(
        unsafe { GetForegroundWindow() },
        expected,
        "foreground changed; this is a failure or an inconclusive run if the user switched apps concurrently"
    );
}

#[derive(Debug, Clone, Copy)]
struct GuiCounts {
    gdi: u32,
    user: u32,
}

fn gui_counts() -> GuiCounts {
    let process = unsafe { GetCurrentProcess() };
    let read = |kind| {
        unsafe { SetLastError(ERROR_SUCCESS) };
        let count = unsafe { GetGuiResources(process, kind) };
        let error = unsafe { GetLastError() };
        assert!(
            count != 0 || error == ERROR_SUCCESS,
            "GetGuiResources failed: {error:?}"
        );
        count
    };
    GuiCounts {
        gdi: read(GR_GDIOBJECTS),
        user: read(GR_USEROBJECTS),
    }
}
