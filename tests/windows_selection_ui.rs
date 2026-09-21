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
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, POINT, RECT, WPARAM},
        Graphics::Gdi::{ClientToScreen, UpdateWindow},
        UI::Controls::{TBM_GETRANGEMAX, TBM_GETRANGEMIN, TBM_SETPOS},
        UI::WindowsAndMessaging::{
            BM_CLICK, BM_SETCHECK, CB_GETCOUNT, CreateWindowExW, DestroyWindow, ES_READONLY,
            GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetDlgItem, GetNextDlgTabItem, GetWindowLongW,
            GetWindowRect, GetWindowTextW, HTCAPTION, HTCLIENT, IsDialogMessageW, IsIconic,
            IsWindow, IsWindowVisible, MSG, SB_BOTTOM, SB_TOP, SW_RESTORE, SWP_NOACTIVATE,
            SWP_NOZORDER, SendMessageW, SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WM_CLOSE,
            WM_COMMAND, WM_GETDLGCODE, WM_HSCROLL, WM_KEYDOWN, WM_KEYUP, WM_NCHITTEST,
            WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER, WM_VSCROLL, WS_EX_NOACTIVATE, WS_EX_TOPMOST,
            WS_OVERLAPPED, WS_TABSTOP,
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
    // The original focus stays over its source pixel even when the footer
    // adapts to the available width and current display scaling.
    let hover = focus;
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
    let minimize = unsafe { GetDlgItem(Some(result_hwnd), 40) }.unwrap();
    let caption_close = unsafe { GetDlgItem(Some(result_hwnd), 41) }.unwrap();
    assert_eq!(window_text(minimize), "最小化");
    assert_eq!(window_text(caption_close), "关闭窗口");
    let mut client = RECT::default();
    let mut window = RECT::default();
    unsafe {
        GetClientRect(result_hwnd, &mut client).unwrap();
        GetWindowRect(result_hwnd, &mut window).unwrap();
    }
    assert_eq!(
        (client.right - client.left, client.bottom - client.top),
        (window.right - window.left, window.bottom - window.top),
        "the custom caption must not leave a native nonclient frame"
    );
    let status = unsafe { GetDlgItem(Some(result_hwnd), 12) }.unwrap();
    assert!(!unsafe { IsWindowVisible(status) }.as_bool());
    let mut button_bounds = RECT::default();
    unsafe { GetWindowRect(default_copy, &mut button_bounds) }.unwrap();
    let dpi = unsafe { GetDpiForWindow(result_hwnd) };
    assert!(
        window.bottom - button_bounds.bottom <= ((20 * dpi + 48) / 96) as i32,
        "an unused copy-status footer must not leave a large blank area"
    );
    let swatch = unsafe { GetDlgItem(Some(result_hwnd), 10) }.unwrap();
    let mut swatch_bounds = RECT::default();
    unsafe { GetWindowRect(swatch, &mut swatch_bounds) }.unwrap();
    assert_eq!(
        window.right - swatch_bounds.right,
        ((2 * dpi + 48) / 96) as i32,
        "the swatch child must leave the configured right border exposed"
    );
    let hit_test = |point: POINT| {
        // WM_NCHITTEST packs signed screen coordinates into two 16-bit words.
        // Keep the bit patterns for monitors left of or above the primary one.
        let packed = u32::from(point.x as u16) | (u32::from(point.y as u16) << 16);
        unsafe {
            SendMessageW(
                result_hwnd,
                WM_NCHITTEST,
                None,
                Some(LPARAM(packed as isize)),
            )
        }
        .0
    };
    let caption_inset = ((16 * unsafe { GetDpiForWindow(result_hwnd) } + 48) / 96) as i32;
    let mut caption_point = POINT {
        x: caption_inset,
        y: caption_inset,
    };
    assert!(unsafe { ClientToScreen(result_hwnd, &mut caption_point) }.as_bool());
    assert_eq!(hit_test(caption_point), HTCAPTION as isize);
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
        if index == 0 {
            let mut edit_rect = RECT::default();
            unsafe { GetWindowRect(edit, &mut edit_rect) }.unwrap();
            assert_eq!(
                hit_test(POINT {
                    x: edit_rect.left + (edit_rect.right - edit_rect.left) / 2,
                    y: edit_rect.top + (edit_rect.bottom - edit_rect.top) / 2,
                }),
                HTCLIENT as isize,
                "the value area must remain client input, not caption dragging"
            );
        }
    }
    // Exercise only this fixture's caption controls; no system input or copy.
    unsafe { SendMessageW(minimize, BM_CLICK, None, None) };
    assert_eq!(result.process_pending().unwrap(), None);
    assert!(unsafe { IsIconic(result_hwnd) }.as_bool());
    let _ = unsafe { ShowWindow(result_hwnd, SW_RESTORE) };
    assert_eq!(result.process_pending().unwrap(), None);
    assert!(!unsafe { IsIconic(result_hwnd) }.as_bool());
    unsafe { SendMessageW(caption_close, BM_CLICK, None, None) };
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
    let settings_content = unsafe { GetDlgItem(Some(settings_hwnd), 200) }.unwrap();
    assert_eq!(settings.process_pending().unwrap(), None);
    let format_combo = unsafe { GetDlgItem(Some(settings_content), 105) }.unwrap();
    assert_eq!(
        unsafe { SendMessageW(format_combo, CB_GETCOUNT, None, None) }.0,
        4
    );
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(config.clone()))
    );
    let quick = unsafe { GetDlgItem(Some(settings_content), 109) }.unwrap();
    let automatic = unsafe { GetDlgItem(Some(settings_content), 106) }.unwrap();
    for ordinary_copy in [false, true] {
        unsafe {
            SendMessageW(
                automatic,
                BM_SETCHECK,
                Some(WPARAM(usize::from(ordinary_copy))),
                None,
            );
            SendMessageW(quick, BM_CLICK, None, None);
        }
        assert_eq!(
            settings.process_pending().unwrap(),
            None,
            "quick picking remains a draft"
        );
        assert!(
            !unsafe { windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(automatic) }
                .as_bool()
        );
        let mut quick_config = config.clone();
        quick_config.quick_pick = true;
        quick_config.auto_copy_on_pick = ordinary_copy;
        unsafe {
            SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None);
        }
        assert_eq!(
            settings.process_pending().unwrap(),
            Some(SettingsAction::Apply(quick_config))
        );
        unsafe {
            SendMessageW(quick, BM_CLICK, None, None);
        }
        assert_eq!(settings.process_pending().unwrap(), None);
        assert!(
            unsafe { windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(automatic) }
                .as_bool()
        );
        let mut normal_config = config.clone();
        normal_config.auto_copy_on_pick = ordinary_copy;
        unsafe {
            SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None);
        }
        assert_eq!(
            settings.process_pending().unwrap(),
            Some(SettingsAction::Apply(normal_config)),
            "turning off quick picking restores the ordinary copy preference"
        );
    }
    let border = unsafe { GetDlgItem(Some(settings_content), 107) }.unwrap();
    let transparency = unsafe { GetDlgItem(Some(settings_content), 108) }.unwrap();
    let apply = unsafe { GetDlgItem(Some(settings_hwnd), 1) }.unwrap();
    let close = unsafe { GetDlgItem(Some(settings_hwnd), 2) }.unwrap();
    let mut original = RECT::default();
    unsafe { GetWindowRect(settings_hwnd, &mut original) }.unwrap();
    assert!(original.top >= work.top && original.bottom <= work.bottom);
    // A short work area is simulated by resizing only this fixture. The footer
    // stays fixed while native controls scroll, and keyboard focus reveals them.
    let settings_dpi = unsafe { GetDpiForWindow(settings_hwnd) };
    unsafe {
        SetWindowPos(
            settings_hwnd,
            None,
            original.left,
            original.top,
            original.right - original.left,
            (360 * settings_dpi / 96) as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .unwrap();
    }
    settings.process_pending().unwrap();
    let mut viewport_bounds = RECT::default();
    let mut footer_before = RECT::default();
    unsafe {
        GetWindowRect(settings_content, &mut viewport_bounds).unwrap();
        GetWindowRect(apply, &mut footer_before).unwrap();
    }
    for button in [apply, close] {
        let mut bounds = RECT::default();
        unsafe {
            GetWindowRect(button, &mut bounds).unwrap();
        }
        assert!(bounds.top >= viewport_bounds.bottom);
        assert!(bounds.bottom <= original.top + (360 * settings_dpi / 96) as i32);
    }
    unsafe {
        SendMessageW(
            settings_content,
            WM_VSCROLL,
            Some(WPARAM(SB_BOTTOM.0 as usize)),
            None,
        );
    }
    settings.process_pending().unwrap();
    let mut footer_after = RECT::default();
    unsafe {
        GetWindowRect(apply, &mut footer_after).unwrap();
    }
    assert_eq!(
        footer_before, footer_after,
        "scrolling must keep Apply stationary"
    );
    let ctrl = unsafe { GetDlgItem(Some(settings_content), 101) }.unwrap();
    unsafe {
        windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(apply)).unwrap();
        windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(ctrl)).unwrap();
    }
    settings.process_pending().unwrap();
    let mut control_bounds = RECT::default();
    unsafe {
        GetWindowRect(ctrl, &mut control_bounds).unwrap();
    }
    assert!(
        control_bounds.top >= viewport_bounds.top
            && control_bounds.bottom <= viewport_bounds.bottom
    );
    unsafe {
        windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(border)).unwrap();
    }
    settings.process_pending().unwrap();
    let next = MSG {
        hwnd: border,
        message: WM_KEYDOWN,
        wParam: WPARAM(0x09),
        lParam: LPARAM(1),
        ..Default::default()
    };
    assert!(unsafe { IsDialogMessageW(settings_hwnd, &next) }.as_bool());
    settings.process_pending().unwrap();
    unsafe {
        GetWindowRect(transparency, &mut control_bounds).unwrap();
    }
    assert!(
        control_bounds.top >= viewport_bounds.top
            && control_bounds.bottom <= viewport_bounds.bottom,
        "Tab must reveal the focused native control"
    );
    unsafe {
        SendMessageW(
            settings_content,
            WM_VSCROLL,
            Some(WPARAM(SB_TOP.0 as usize)),
            None,
        );
        SetWindowPos(
            settings_hwnd,
            None,
            original.left,
            original.top,
            original.right - original.left,
            original.bottom - original.top,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .unwrap();
    }
    settings.process_pending().unwrap();
    for (slider, maximum, expected) in [
        (border, 6, config.appearance.border_width_dip),
        (
            transparency,
            80,
            config.appearance.background_transparency_percent,
        ),
    ] {
        assert_ne!(
            unsafe { GetWindowLongW(slider, GWL_STYLE) } as u32 & WS_TABSTOP.0,
            0
        );
        assert_eq!(
            unsafe { SendMessageW(slider, TBM_GETRANGEMIN, None, None) }.0,
            0
        );
        assert_eq!(
            unsafe { SendMessageW(slider, TBM_GETRANGEMAX, None, None) }.0,
            maximum
        );
        // TBM_GETPOS is the WM_USER alias omitted by windows-rs.
        assert_eq!(
            unsafe { SendMessageW(slider, WM_USER, None, None) }.0,
            isize::from(expected)
        );
    }
    assert_eq!(
        unsafe { GetNextDlgTabItem(settings_hwnd, Some(border), false) }.unwrap(),
        transparency
    );
    press_key(border, 0x27, false); // Right increments the native trackbar.
    press_key(transparency, 0x23, false); // End selects its maximum.
    assert_eq!(
        settings.process_pending().unwrap(),
        None,
        "slider changes remain a draft"
    );
    assert_eq!(
        window_text(unsafe { GetDlgItem(Some(settings_content), 22) }.unwrap()),
        "3 DIP"
    );
    assert_eq!(
        window_text(unsafe { GetDlgItem(Some(settings_content), 24) }.unwrap()),
        "80%"
    );
    let appearance_preview = unsafe { GetDlgItem(Some(settings_content), 26) }.unwrap();
    assert!(window_text(appearance_preview).contains("边框 3 DIP，背景透明度 80%"));
    assert!(unsafe { UpdateWindow(appearance_preview) }.as_bool());
    let mut edited = config.clone();
    edited.appearance.border_width_dip = 3;
    edited.appearance.background_transparency_percent = 80;
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(edited.clone()))
    );
    // Exercise the native key control, including the dialog message path.
    // These messages target only this fixture; no global input is generated.
    let key = unsafe { GetDlgItem(Some(settings_content), 104) }.unwrap();
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
    let alt = unsafe { GetDlgItem(Some(settings_content), 102) }.unwrap();
    unsafe {
        SendMessageW(alt, BM_SETCHECK, Some(WPARAM(0)), None);
        SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None);
    }
    assert_eq!(settings.process_pending().unwrap(), None);
    unsafe {
        SendMessageW(border, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(0)));
        SendMessageW(
            settings_hwnd,
            WM_HSCROLL,
            None,
            Some(LPARAM(border.0 as isize)),
        );
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
    assert!(
        !unsafe {
            windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(
                GetDlgItem(Some(readonly.hwnd()), 1).unwrap(),
            )
        }
        .as_bool()
    );
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
