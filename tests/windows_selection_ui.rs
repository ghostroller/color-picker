#![cfg(windows)]

//! One opt-in construction smoke check. No input, hooks or clipboard actions.

#[path = "support/pixel_fixture.rs"]
mod pixel_fixture;

use color_picker::{
    app::config::{Config, HotkeyConfig},
    core::{
        color::Rgb8,
        format::{ColorFormat, format_color},
        geometry::ScreenPointPx,
        state::SampleKind,
        zoom::FrozenImage,
    },
    platform::windows::{monitors::Monitors, session::cursor_position},
    ui::windows::{
        magnifier::MagnifierWindow,
        result::{ResultAction, ResultWindow},
        settings::{SettingsAction, SettingsWindow},
    },
};
use pixel_fixture::ScopedPmv2;
use windows::{
    Win32::{
        Foundation::{HWND, WPARAM},
        Graphics::Gdi::UpdateWindow,
        UI::WindowsAndMessaging::{
            BM_SETCHECK, CB_GETCOUNT, CreateWindowExW, DestroyWindow, ES_READONLY, GWL_STYLE,
            GetDlgItem, GetWindowLongW, GetWindowTextW, IsWindow, SendMessageW, WINDOW_EX_STYLE,
            WM_CLOSE, WM_COMMAND, WS_OVERLAPPED,
        },
    },
    core::w,
};

#[test]
#[ignore = "requires an interactive Windows desktop; briefly displays its own magnifier and result windows"]
fn cached_selection_and_native_result_controls_smoke() {
    let _dpi = ScopedPmv2::enter().unwrap();
    let cursor = cursor_position().unwrap();
    let monitors = Monitors::enumerate().unwrap();
    let work = monitors.at(cursor).unwrap().work_area;
    let focus = ScreenPointPx {
        x: (i64::from(work.left) + i64::from(work.width()) / 2) as i32,
        y: (i64::from(work.top) + i64::from(work.height()) / 2) as i32,
    };
    let origin = ScreenPointPx {
        x: focus.x - 32,
        y: focus.y - 32,
    };
    let mut bgrx = Vec::with_capacity(65 * 65 * 4);
    for y in 0_u8..65 {
        for x in 0_u8..65 {
            bgrx.extend_from_slice(&[x ^ y, y, x, 0xff]);
        }
    }
    let magnifier = MagnifierWindow::new(
        FrozenImage {
            origin,
            width: 65,
            height: 65,
            stride_bytes: 65 * 4,
            bgrx,
        },
        focus,
        work,
    )
    .unwrap();
    let magnifier_hwnd = magnifier.hwnd();
    let bounds = magnifier.rect().unwrap();
    // The square viewport center uses the window width; its text footer sits
    // below the square and must never produce a color selection.
    let hover = ScreenPointPx {
        x: (i64::from(bounds.left) + i64::from(bounds.width()) / 2) as i32,
        y: (i64::from(bounds.top) + i64::from(bounds.width()) / 2) as i32,
    };
    let picked = magnifier
        .hit_test(hover)
        .expect("the image center must be selectable");
    let x = (picked.source.x - origin.x) as u8;
    let y = (picked.source.y - origin.y) as u8;
    assert_eq!(picked.rgb, Rgb8::new(x, y, x ^ y));
    assert_eq!(picked.kind, SampleKind::Frozen);
    assert_eq!(magnifier.scale_factor(), 4);
    assert!(
        magnifier
            .hit_test(ScreenPointPx {
                x: hover.x,
                y: bounds.bottom - 1
            })
            .is_none()
    );
    for factor in [8, 16, 32] {
        assert!(magnifier.change_scale(true, hover).unwrap());
        assert!(unsafe { UpdateWindow(magnifier.hwnd()) }.as_bool());
        magnifier.update_hover(hover).unwrap(); // propagates any paint failure
        assert_eq!(magnifier.scale_factor(), factor);
        assert_eq!(
            magnifier.rect(),
            Some(bounds),
            "zoom must keep a fixed window"
        );
        assert_eq!(magnifier.hit_test(hover), Some(picked));
    }
    for factor in [16, 8, 4] {
        assert!(magnifier.change_scale(false, hover).unwrap());
        assert_eq!(magnifier.scale_factor(), factor);
        assert_eq!(magnifier.hit_test(hover), Some(picked));
    }
    assert!(!magnifier.change_scale(false, hover).unwrap());
    drop(magnifier);
    assert!(!unsafe { IsWindow(Some(magnifier_hwnd)) }.as_bool());

    let owner = TestOwner(
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("color-picker UI smoke owner"),
                WS_OVERLAPPED,
                work.left,
                work.top,
                1,
                1,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap(),
    );
    let result = ResultWindow::new_with_options(picked, owner.0, ColorFormat::Hsl, false).unwrap();
    let result_hwnd = result.hwnd();
    assert_eq!(result.process_pending().unwrap(), None);
    let default_copy = unsafe { GetDlgItem(Some(result_hwnd), 1) }.unwrap();
    assert!(window_text(default_copy).contains("HSL"));
    for (index, format) in ColorFormat::ALL.into_iter().enumerate() {
        let edit = unsafe { GetDlgItem(Some(result_hwnd), 30 + index as i32) }.unwrap();
        let mut text = [0_u16; 128];
        let length = unsafe { GetWindowTextW(edit, &mut text) };
        assert!(length > 0);
        assert_eq!(
            String::from_utf16(&text[..length as usize]).unwrap(),
            format_color(picked.rgb, format)
        );
        assert_ne!(unsafe { GetWindowLongW(edit, GWL_STYLE) } & ES_READONLY, 0);
    }
    // Send only this test's own close intention. Do not pump external messages
    // or click any copy control: the user's clipboard is never touched.
    unsafe { SendMessageW(result_hwnd, WM_CLOSE, None, None) };
    assert_eq!(result.process_pending().unwrap(), Some(ResultAction::Close));
    drop(result);
    assert!(!unsafe { IsWindow(Some(result_hwnd)) }.as_bool());

    let config = Config {
        hotkey: HotkeyConfig {
            ctrl: false,
            alt: true,
            shift: true,
            key: "F11".to_owned(),
        },
        default_format: ColorFormat::CssRgb,
        auto_copy_on_pick: true,
        ..Config::default()
    };
    let settings =
        SettingsWindow::new(&config, owner.0, Some("控件测试；不会保存配置"), true).unwrap();
    let settings_hwnd = settings.hwnd();
    assert_eq!(settings.process_pending().unwrap(), None);
    for (id, count) in [(104, 47), (105, 4)] {
        let combo = unsafe { GetDlgItem(Some(settings_hwnd), id) }.unwrap();
        assert_eq!(
            unsafe { SendMessageW(combo, CB_GETCOUNT, None, None) }.0,
            count
        );
    }
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(config.clone()))
    );
    // Removing both Ctrl and Alt is invalid; no draft may reach persistence.
    let alt = unsafe { GetDlgItem(Some(settings_hwnd), 102) }.unwrap();
    unsafe {
        SendMessageW(alt, BM_SETCHECK, Some(WPARAM(0)), None);
        SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None);
    }
    assert_eq!(settings.process_pending().unwrap(), None);
    unsafe { SendMessageW(settings_hwnd, WM_CLOSE, None, None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Close)
    );
    drop(settings);
    assert!(!unsafe { IsWindow(Some(settings_hwnd)) }.as_bool());

    let readonly = SettingsWindow::new(&config, owner.0, None, false).unwrap();
    unsafe { SendMessageW(readonly.hwnd(), WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(readonly.process_pending().unwrap(), None);
    drop(readonly);
    assert!(unsafe { IsWindow(Some(owner.0)) }.as_bool());
    let owner_hwnd = owner.0;
    drop(owner);
    assert!(!unsafe { IsWindow(Some(owner_hwnd)) }.as_bool());
}

fn window_text(hwnd: HWND) -> String {
    let mut text = [0_u16; 256];
    let length = unsafe { GetWindowTextW(hwnd, &mut text) };
    String::from_utf16(&text[..length as usize]).unwrap()
}

struct TestOwner(HWND);

impl Drop for TestOwner {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
