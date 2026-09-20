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
        Foundation::{HWND, LPARAM, WPARAM},
        Graphics::Gdi::UpdateWindow,
        UI::WindowsAndMessaging::{
            BM_CLICK, BM_SETCHECK, CB_GETCOUNT, CreateWindowExW, DestroyWindow, ES_READONLY,
            GWL_EXSTYLE, GWL_STYLE, GetDlgItem, GetWindowLongW, GetWindowTextW, IsDialogMessageW,
            IsWindow, MSG, SendMessageW, WINDOW_EX_STYLE, WM_CLOSE, WM_COMMAND, WM_GETDLGCODE,
            WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WS_EX_NOACTIVATE, WS_EX_TOPMOST,
            WS_OVERLAPPED,
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
    let overlay_style = unsafe { GetWindowLongW(magnifier_hwnd, GWL_EXSTYLE) } as u32;
    assert_eq!(
        overlay_style & (WS_EX_TOPMOST | WS_EX_NOACTIVATE).0,
        (WS_EX_TOPMOST | WS_EX_NOACTIVATE).0
    );
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
    let format_combo = unsafe { GetDlgItem(Some(settings_hwnd), 105) }.unwrap();
    assert_eq!(
        unsafe { SendMessageW(format_combo, CB_GETCOUNT, None, None) }.0,
        4
    );
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(config.clone()))
    );
    // Exercise the native key control, including the dialog message path.
    // These messages target only this fixture; no global input is generated.
    let key = unsafe { GetDlgItem(Some(settings_hwnd), 104) }.unwrap();
    assert!(window_text(key).contains("F11"));
    unsafe { SendMessageW(key, BM_CLICK, None, None) };
    settings.process_pending().unwrap();
    press_key(key, 0x7b, false); // F12 is reserved; keep listening.
    assert_eq!(settings.process_pending().unwrap(), None);
    unsafe { SendMessageW(key, WM_KEYDOWN, Some(WPARAM(0x0d)), Some(LPARAM(1))) };
    press_key(key, 0x4b, false); // K replaces F11.
    settings.process_pending().unwrap();
    assert!(window_text(key).contains('K'));
    let held_enter = MSG {
        hwnd: key,
        message: WM_KEYDOWN,
        wParam: WPARAM(0x0d),
        lParam: LPARAM(0x4000_0001),
        ..Default::default()
    };
    let wants_repeat = unsafe {
        SendMessageW(
            key,
            WM_GETDLGCODE,
            Some(held_enter.wParam),
            Some(LPARAM((&raw const held_enter) as isize)),
        )
    }
    .0;
    assert_ne!(
        wants_repeat & 4,
        0,
        "accepting K must not release a still-held Enter"
    );
    assert!(unsafe { IsDialogMessageW(settings_hwnd, &held_enter) }.as_bool());
    assert_eq!(settings.process_pending().unwrap(), None);
    unsafe { SendMessageW(key, WM_KEYUP, Some(WPARAM(0x0d)), Some(LPARAM(0xc000_0001))) };
    let mut edited = config.clone();
    edited.hotkey.key = "K".into();
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(edited.clone()))
    );

    unsafe { SendMessageW(key, BM_CLICK, None, None) };
    settings.process_pending().unwrap();
    let enter = MSG {
        hwnd: key,
        message: WM_KEYDOWN,
        wParam: WPARAM(0x0d),
        lParam: LPARAM(1),
        ..Default::default()
    };
    let wants_enter = unsafe {
        SendMessageW(
            key,
            WM_GETDLGCODE,
            Some(enter.wParam),
            Some(LPARAM((&raw const enter) as isize)),
        )
    }
    .0;
    assert_ne!(
        wants_enter & 4,
        0,
        "recording must claim Enter before the dialog applies settings"
    );
    assert!(unsafe { IsDialogMessageW(settings_hwnd, &enter) }.as_bool());
    unsafe { SendMessageW(key, WM_KEYUP, Some(WPARAM(0x0d)), Some(LPARAM(0xc000_0001))) };
    assert_eq!(settings.process_pending().unwrap(), None);
    press_key(key, 0x1b, false); // Esc cancels recording, not the settings window.
    assert_eq!(settings.process_pending().unwrap(), None);
    assert!(window_text(key).contains('K'));

    unsafe { SendMessageW(key, BM_CLICK, None, None) };
    settings.process_pending().unwrap();
    press_key(key, 0x79, true); // F10 arrives as a system-key message.
    settings.process_pending().unwrap();
    edited.hotkey.key = "F10".into();
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(edited))
    );

    unsafe { SendMessageW(key, BM_CLICK, None, None) };
    settings.process_pending().unwrap();
    unsafe { SendMessageW(key, WM_KEYDOWN, Some(WPARAM(0x0d)), Some(LPARAM(1))) };
    let tab = MSG {
        hwnd: key,
        message: WM_KEYDOWN,
        wParam: WPARAM(0x09),
        lParam: LPARAM(1),
        ..Default::default()
    };
    assert!(unsafe { IsDialogMessageW(settings_hwnd, &tab) }.as_bool());
    settings.process_pending().unwrap();
    assert!(
        window_text(key).contains("F10"),
        "Tab cancels recording and retains the old value"
    );
    let repeat_after_tab = MSG {
        hwnd: format_combo,
        ..held_enter
    };
    assert!(
        settings.filter_key_message(&repeat_after_tab),
        "held Enter must not activate the newly focused control"
    );
    let released = MSG {
        message: WM_KEYUP,
        lParam: LPARAM(0xc000_0001),
        ..repeat_after_tab
    };
    assert!(settings.filter_key_message(&released));
    let fresh = MSG {
        lParam: LPARAM(1),
        ..repeat_after_tab
    };
    assert!(!settings.filter_key_message(&fresh));
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

fn press_key(hwnd: HWND, key: usize, system: bool) {
    unsafe {
        SendMessageW(
            hwnd,
            if system { WM_SYSKEYDOWN } else { WM_KEYDOWN },
            Some(WPARAM(key)),
            Some(LPARAM(1)),
        );
        SendMessageW(
            hwnd,
            if system { WM_SYSKEYUP } else { WM_KEYUP },
            Some(WPARAM(key)),
            Some(LPARAM(0xc000_0001)),
        );
    }
}

struct TestOwner(HWND);

impl Drop for TestOwner {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
