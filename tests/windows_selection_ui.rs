#![cfg(windows)]

//! Opt-in construction smoke checks. No system input, hooks or clipboard actions.

#[path = "support/pixel_fixture.rs"]
mod pixel_fixture;

use color_picker::{
    app::{
        config::{Config, HotkeyConfig},
        i18n::{self, Language},
    },
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
            BM_CLICK, BM_SETCHECK, CB_GETCOUNT, CB_SETCURSEL, CreateWindowExW, DestroyWindow,
            ES_READONLY, GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetDlgItem, GetNextDlgTabItem,
            GetScrollInfo, GetWindowLongW, GetWindowRect, GetWindowTextW, HTCAPTION, HTCLIENT,
            IsDialogMessageW, IsIconic, IsWindow, IsWindowVisible, MSG, SB_BOTTOM, SB_TOP, SB_VERT,
            SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER,
            SendMessageW, SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WM_CLOSE, WM_COMMAND,
            WM_GETDLGCODE, WM_HSCROLL, WM_KEYDOWN, WM_KEYUP, WM_NCHITTEST, WM_SYSKEYDOWN,
            WM_SYSKEYUP, WM_USER, WM_VSCROLL, WS_EX_NOACTIVATE, WS_EX_TOPMOST, WS_OVERLAPPED,
            WS_TABSTOP,
        },
    },
    core::w,
};

#[test]
#[ignore = "requires an interactive Windows desktop; briefly displays its own magnifier and result windows"]
fn cached_selection_and_native_result_controls_smoke() {
    i18n::set_language(Language::SimplifiedChinese);
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
    // SAFETY: Query scalar window style bits for the test-owned live HWND; no callback
    // pointer is interpreted.
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
    assert_eq!(picked.source, focus);
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
        // SAFETY: Synchronously paint only the live fixture window; no callback-state
        // borrow spans this reentrant operation.
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
    // SAFETY: This is a handle-validity assertion only; it neither dereferences userdata nor
    // establishes ownership for releasing callback memory.
    assert!(!unsafe { IsWindow(Some(magnifier_hwnd)) }.as_bool());

    let owner = TestOwner(
        // SAFETY: The class/title strings remain live and terminated during creation;
        // this thread owns the returned fixture window and any callback data.
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
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let default_copy = unsafe { GetDlgItem(Some(result_hwnd), 1) }.unwrap();
    assert!(window_text(default_copy).contains("HSL"));
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let minimize = unsafe { GetDlgItem(Some(result_hwnd), 40) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let caption_close = unsafe { GetDlgItem(Some(result_hwnd), 41) }.unwrap();
    assert_eq!(window_text(minimize), "最小化");
    assert_eq!(window_text(caption_close), "关闭窗口");
    let mut client = RECT::default();
    let mut window = RECT::default();
    // SAFETY: The fixture window remains alive and the RECT/POINT outputs are writable local
    // values used only for these synchronous queries.
    unsafe {
        GetClientRect(result_hwnd, &mut client).unwrap();
        GetWindowRect(result_hwnd, &mut window).unwrap();
    }
    assert_eq!(
        (client.right - client.left, client.bottom - client.top),
        (window.right - window.left, window.bottom - window.top),
        "the custom caption must not leave a native nonclient frame"
    );
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let status = unsafe { GetDlgItem(Some(result_hwnd), 12) }.unwrap();
    // SAFETY: Query visibility on the fixture HWND without dereferencing or retaining any
    // native pointer.
    assert!(!unsafe { IsWindowVisible(status) }.as_bool());
    let mut button_bounds = RECT::default();
    // SAFETY: The fixture window remains alive on this thread and each RECT output is valid
    // writable stack storage.
    unsafe { GetWindowRect(default_copy, &mut button_bounds) }.unwrap();
    // SAFETY: Query only the live fixture HWND while its window owner remains in scope.
    let dpi = unsafe { GetDpiForWindow(result_hwnd) };
    assert!(
        window.bottom - button_bounds.bottom <= ((20 * dpi + 48) / 96) as i32,
        "an unused copy-status footer must not leave a large blank area"
    );
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let swatch = unsafe { GetDlgItem(Some(result_hwnd), 10) }.unwrap();
    let mut swatch_bounds = RECT::default();
    // SAFETY: The fixture window remains alive on this thread and each RECT output is valid
    // writable stack storage.
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
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
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
    // SAFETY: Query only the live fixture HWND while its window owner remains in scope.
    let caption_inset = ((16 * unsafe { GetDpiForWindow(result_hwnd) } + 48) / 96) as i32;
    let mut caption_point = POINT {
        x: caption_inset,
        y: caption_inset,
    };
    // SAFETY: The live fixture HWND is queried with a writable local POINT; its address is
    // not retained.
    assert!(unsafe { ClientToScreen(result_hwnd, &mut caption_point) }.as_bool());
    assert_eq!(hit_test(caption_point), HTCLIENT as isize);
    let mut caption_point = POINT {
        x: client.right * 5 / 8,
        y: caption_inset,
    };
    // SAFETY: The writable local point refers to the retained result window's right preview.
    assert!(unsafe { ClientToScreen(result_hwnd, &mut caption_point) }.as_bool());
    assert_eq!(hit_test(caption_point), HTCAPTION as isize);
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let result_content = unsafe { GetDlgItem(Some(result_hwnd), 300) }.unwrap();
    for (index, format) in ColorFormat::ALL.into_iter().enumerate() {
        // SAFETY: The parent belongs to this test-owned live window tree; the returned
        // child handle is borrowed only while its owner survives.
        let edit = unsafe { GetDlgItem(Some(result_content), 30 + index as i32) }.unwrap();
        let mut text = [0_u16; 128];
        // SAFETY: This live fixture control is queried synchronously into a writable
        // UTF-16 slice; the API receives the slice capacity.
        let length = unsafe { GetWindowTextW(edit, &mut text) };
        assert!(length > 0);
        assert_eq!(
            String::from_utf16(&text[..length as usize]).unwrap(),
            format_color(picked.rgb, format)
        );
        // SAFETY: Query scalar window style bits for the test-owned live HWND; no
        // callback pointer is interpreted.
        assert_ne!(unsafe { GetWindowLongW(edit, GWL_STYLE) } & ES_READONLY, 0);
        if index == 0 {
            let mut edit_rect = RECT::default();
            // SAFETY: The fixture window remains alive on this thread and each RECT
            // output is valid writable stack storage.
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
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(minimize, BM_CLICK, None, None) };
    assert_eq!(result.process_pending().unwrap(), None);
    // SAFETY: Query the scalar minimized state of this fixture HWND without taking native
    // ownership.
    assert!(unsafe { IsIconic(result_hwnd) }.as_bool());
    // SAFETY: The HWND is the live test-owned window and this operation does not retain a
    // Rust pointer.
    let _ = unsafe { ShowWindow(result_hwnd, SW_RESTORE) };
    assert_eq!(result.process_pending().unwrap(), None);
    // SAFETY: Query the scalar minimized state of this fixture HWND without taking native
    // ownership.
    assert!(!unsafe { IsIconic(result_hwnd) }.as_bool());
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(caption_close, BM_CLICK, None, None) };
    assert_eq!(result.process_pending().unwrap(), Some(ResultAction::Close));
    drop(result);
    // SAFETY: This is a handle-validity assertion only; it neither dereferences userdata nor
    // establishes ownership for releasing callback memory.
    assert!(!unsafe { IsWindow(Some(result_hwnd)) }.as_bool());

    let config = Config {
        language: Language::SimplifiedChinese,
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
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let settings_viewport = unsafe { GetDlgItem(Some(settings_hwnd), 200) }.unwrap();
    // SAFETY: The fixture retains the viewport and its owned scrolling document
    // while these child handles are borrowed.
    let settings_content = unsafe { GetDlgItem(Some(settings_viewport), 201) }.unwrap();
    assert_eq!(settings.process_pending().unwrap(), None);
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let format_combo = unsafe { GetDlgItem(Some(settings_content), 105) }.unwrap();
    assert_eq!(
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
        unsafe { SendMessageW(format_combo, CB_GETCOUNT, None, None) }.0,
        4
    );
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(config.clone()))
    );
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let quick = unsafe { GetDlgItem(Some(settings_content), 109) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let automatic = unsafe { GetDlgItem(Some(settings_content), 106) }.unwrap();
    for ordinary_copy in [false, true] {
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
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
            // SAFETY: Query enablement on the fixture control while its owning
            // window tree remains alive.
            !unsafe { windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(automatic) }
                .as_bool()
        );
        let mut quick_config = config.clone();
        quick_config.quick_pick = true;
        quick_config.auto_copy_on_pick = ordinary_copy;
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
        unsafe {
            SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None);
        }
        assert_eq!(
            settings.process_pending().unwrap(),
            Some(SettingsAction::Apply(quick_config))
        );
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
        unsafe {
            SendMessageW(quick, BM_CLICK, None, None);
        }
        assert_eq!(settings.process_pending().unwrap(), None);
        assert!(
            // SAFETY: Query enablement on the fixture control while its owning
            // window tree remains alive.
            unsafe { windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(automatic) }
                .as_bool()
        );
        let mut normal_config = config.clone();
        normal_config.auto_copy_on_pick = ordinary_copy;
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
        unsafe {
            SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None);
        }
        assert_eq!(
            settings.process_pending().unwrap(),
            Some(SettingsAction::Apply(normal_config)),
            "turning off quick picking restores the ordinary copy preference"
        );
    }
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let border = unsafe { GetDlgItem(Some(settings_content), 107) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let transparency = unsafe { GetDlgItem(Some(settings_content), 108) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let apply = unsafe { GetDlgItem(Some(settings_hwnd), 1) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let close = unsafe { GetDlgItem(Some(settings_hwnd), 2) }.unwrap();
    let mut original = RECT::default();
    // SAFETY: The fixture window remains alive on this thread and each RECT output is valid
    // writable stack storage.
    unsafe { GetWindowRect(settings_hwnd, &mut original) }.unwrap();
    assert!(original.top >= work.top && original.bottom <= work.bottom);
    // A narrow, short work area is simulated by resizing only this fixture.
    // Content reflows, the footer stays fixed, and focus reveals scrolled rows.
    // SAFETY: Query only the live fixture HWND while its window owner remains in scope.
    let settings_dpi = unsafe { GetDpiForWindow(settings_hwnd) };
    // SAFETY: Resize/reposition only the test-owned window on its UI thread; dimensions are
    // scalar and no callback borrow spans the call.
    unsafe {
        SetWindowPos(
            settings_hwnd,
            None,
            original.left,
            original.top,
            (380 * settings_dpi / 96) as i32,
            (360 * settings_dpi / 96) as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .unwrap();
    }
    settings.process_pending().unwrap();
    let mut viewport_bounds = RECT::default();
    let mut footer_before = RECT::default();
    // SAFETY: The fixture window remains alive on this thread and each RECT output is valid
    // writable stack storage.
    unsafe {
        GetWindowRect(settings_viewport, &mut viewport_bounds).unwrap();
        GetWindowRect(apply, &mut footer_before).unwrap();
    }
    for id in [110, 101, 102, 103, 104, 105, 106, 107, 108, 109] {
        // SAFETY: The parent belongs to this test-owned live window tree; the returned
        // child handle is borrowed only while its owner survives.
        let control = unsafe { GetDlgItem(Some(settings_content), id) }.unwrap();
        let mut bounds = RECT::default();
        // SAFETY: The fixture window remains alive on this thread and each RECT output
        // is valid writable stack storage.
        unsafe {
            GetWindowRect(control, &mut bounds).unwrap();
        }
        assert!(
            bounds.left >= viewport_bounds.left && bounds.right <= viewport_bounds.right,
            "control {id} must reflow inside the narrow viewport"
        );
    }
    for button in [apply, close] {
        let mut bounds = RECT::default();
        // SAFETY: The fixture window remains alive on this thread and each RECT output
        // is valid writable stack storage.
        unsafe {
            GetWindowRect(button, &mut bounds).unwrap();
        }
        assert!(bounds.top >= viewport_bounds.bottom);
        assert!(bounds.bottom <= original.top + (360 * settings_dpi / 96) as i32);
    }
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe {
        SendMessageW(
            settings_viewport,
            WM_VSCROLL,
            Some(WPARAM(SB_BOTTOM.0 as usize)),
            None,
        );
    }
    settings.process_pending().unwrap();
    let mut footer_after = RECT::default();
    // SAFETY: The fixture window remains alive on this thread and each RECT output is valid
    // writable stack storage.
    unsafe {
        GetWindowRect(apply, &mut footer_after).unwrap();
    }
    assert_eq!(
        footer_before, footer_after,
        "scrolling must keep Apply stationary"
    );
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let ctrl = unsafe { GetDlgItem(Some(settings_content), 101) }.unwrap();
    // SAFETY: Focus is changed only among live controls of this fixture; no Rust state
    // borrow is held across synchronous focus callbacks.
    unsafe {
        windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(apply)).unwrap();
        windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(ctrl)).unwrap();
    }
    settings.process_pending().unwrap();
    let mut control_bounds = RECT::default();
    // SAFETY: The fixture window remains alive on this thread and each RECT output is valid
    // writable stack storage.
    unsafe {
        GetWindowRect(ctrl, &mut control_bounds).unwrap();
    }
    assert!(
        control_bounds.top >= viewport_bounds.top
            && control_bounds.bottom <= viewport_bounds.bottom
    );
    // SAFETY: Focus is changed only among live controls of this fixture; no Rust state
    // borrow is held across synchronous focus callbacks.
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
    // SAFETY: The fixture dialog and stack MSG remain live for synchronous dispatch; no
    // RefCell state borrow spans this call.
    assert!(unsafe { IsDialogMessageW(settings_hwnd, &next) }.as_bool());
    settings.process_pending().unwrap();
    // SAFETY: The fixture window remains alive on this thread and each RECT output is valid
    // writable stack storage.
    unsafe {
        GetWindowRect(transparency, &mut control_bounds).unwrap();
    }
    assert!(
        control_bounds.top >= viewport_bounds.top
            && control_bounds.bottom <= viewport_bounds.bottom,
        "Tab must reveal the focused native control"
    );
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe {
        SendMessageW(
            settings_viewport,
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
            // SAFETY: Query scalar window style bits for the test-owned live HWND;
            // no callback pointer is interpreted.
            unsafe { GetWindowLongW(slider, GWL_STYLE) } as u32 & WS_TABSTOP.0,
            0
        );
        assert_eq!(
            // SAFETY: These messages target live controls in this test-owned tree;
            // control values are scalar and the calls complete before owner
            // teardown.
            unsafe { SendMessageW(slider, TBM_GETRANGEMIN, None, None) }.0,
            0
        );
        assert_eq!(
            // SAFETY: These messages target live controls in this test-owned tree;
            // control values are scalar and the calls complete before owner
            // teardown.
            unsafe { SendMessageW(slider, TBM_GETRANGEMAX, None, None) }.0,
            maximum
        );
        // TBM_GETPOS is the WM_USER alias omitted by windows-rs.
        assert_eq!(
            // SAFETY: These messages target live controls in this test-owned tree;
            // control values are scalar and the calls complete before owner
            // teardown.
            unsafe { SendMessageW(slider, WM_USER, None, None) }.0,
            isize::from(expected)
        );
    }
    assert_eq!(
        // SAFETY: Both dialog and starting control belong to this live fixture tree; the
        // next handle is borrowed for comparison only.
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
        // SAFETY: The parent belongs to this test-owned live window tree; the returned
        // child handle is borrowed only while its owner survives.
        window_text(unsafe { GetDlgItem(Some(settings_content), 22) }.unwrap()),
        "3 DIP"
    );
    assert_eq!(
        // SAFETY: The parent belongs to this test-owned live window tree; the returned
        // child handle is borrowed only while its owner survives.
        window_text(unsafe { GetDlgItem(Some(settings_content), 24) }.unwrap()),
        "80%"
    );
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let appearance_preview = unsafe { GetDlgItem(Some(settings_content), 26) }.unwrap();
    assert!(window_text(appearance_preview).contains("边框 3 DIP，背景透明度 80%"));
    // SAFETY: Synchronously paint only the live fixture window; no callback-state borrow
    // spans this reentrant operation.
    assert!(unsafe { UpdateWindow(appearance_preview) }.as_bool());
    let mut edited = config.clone();
    edited.appearance.border_width_dip = 3;
    edited.appearance.background_transparency_percent = 80;
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(edited.clone()))
    );
    // Changing the language remains a draft until Apply. Relabeling the same
    // window must preserve both edited controls and the user's scroll position.
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let language = unsafe { GetDlgItem(Some(settings_content), 110) }.unwrap();
    assert_eq!(
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
        unsafe { SendMessageW(language, CB_GETCOUNT, None, None) }.0,
        2
    );
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(language, CB_SETCURSEL, Some(WPARAM(1)), None) };
    assert_eq!(settings.process_pending().unwrap(), None);
    assert_eq!(i18n::language(), Language::SimplifiedChinese);
    assert_eq!(window_text(apply), "应用");
    let mut english_draft = edited.clone();
    english_draft.language = Language::English;
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe {
        SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None);
    }
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(english_draft.clone()))
    );
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe {
        SendMessageW(
            settings_viewport,
            WM_VSCROLL,
            Some(WPARAM(SB_BOTTOM.0 as usize)),
            None,
        );
    }
    settings.process_pending().unwrap();
    let mut before_language_switch = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS,
        ..Default::default()
    };
    // SAFETY: The test-owned viewport writes a correctly sized scroll output.
    unsafe { GetScrollInfo(settings_viewport, SB_VERT, &mut before_language_switch).unwrap() };
    assert!(before_language_switch.nPos > 0);
    i18n::set_language(Language::English);
    settings.refresh_language().unwrap();
    assert_eq!(window_text(settings_hwnd), "Settings — Color Picker");
    assert_eq!(window_text(apply), "Apply");
    assert_eq!(window_text(close), "Close");
    assert_eq!(
        // SAFETY: The parent belongs to this test-owned live window tree; the returned
        // child handle is borrowed only while its owner survives.
        window_text(unsafe { GetDlgItem(Some(settings_content), 15) }.unwrap()),
        "Preferences"
    );
    assert!(window_text(appearance_preview).contains("border 3 DIP, transparency 80%"));
    let mut after_language_switch = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        ..Default::default()
    };
    // SAFETY: The test-owned viewport writes a correctly sized scroll output.
    unsafe { GetScrollInfo(settings_viewport, SB_VERT, &mut after_language_switch).unwrap() };
    let new_maximum = (after_language_switch.nMax - after_language_switch.nPage as i32 + 1)
        .max(after_language_switch.nMin);
    assert_eq!(
        after_language_switch.nPos,
        before_language_switch
            .nPos
            .clamp(after_language_switch.nMin, new_maximum),
        "language changes preserve native scrolling, clamping only if the measured document shrinks"
    );
    assert!(after_language_switch.nPos > 0);
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(english_draft))
    );
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe {
        SendMessageW(language, CB_SETCURSEL, Some(WPARAM(0)), None);
        SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None);
    }
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(edited.clone()))
    );
    i18n::set_language(Language::SimplifiedChinese);
    settings.refresh_language().unwrap();
    assert_eq!(window_text(apply), "应用");
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe {
        SendMessageW(
            settings_viewport,
            WM_VSCROLL,
            Some(WPARAM(SB_TOP.0 as usize)),
            None,
        );
    }
    settings.process_pending().unwrap();
    // Exercise the native key control, including the dialog message path.
    // These messages target only this fixture; no global input is generated.
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let key = unsafe { GetDlgItem(Some(settings_content), 104) }.unwrap();
    assert!(window_text(key).contains("F11"));
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(key, BM_CLICK, None, None) };
    settings.process_pending().unwrap();
    press_key(key, 0x7b, false); // F12 is reserved; keep listening.
    assert_eq!(settings.process_pending().unwrap(), None);
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
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
    // SAFETY: The live fixture control receives a pointer to the stack MSG for this
    // synchronous query only; it is not queued or retained.
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
    // SAFETY: The fixture dialog and stack MSG remain live for synchronous dispatch; no
    // RefCell state borrow spans this call.
    assert!(unsafe { IsDialogMessageW(settings_hwnd, &held_enter) }.as_bool());
    assert_eq!(settings.process_pending().unwrap(), None);
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(key, WM_KEYUP, Some(WPARAM(0x0d)), Some(LPARAM(0xc000_0001))) };
    edited.hotkey.key = "K".into();
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(edited.clone()))
    );

    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(key, BM_CLICK, None, None) };
    settings.process_pending().unwrap();
    let enter = MSG {
        hwnd: key,
        message: WM_KEYDOWN,
        wParam: WPARAM(0x0d),
        lParam: LPARAM(1),
        ..Default::default()
    };
    // SAFETY: The live fixture control receives a pointer to the stack MSG for this
    // synchronous query only; it is not queued or retained.
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
    // SAFETY: The fixture dialog and stack MSG remain live for synchronous dispatch; no
    // RefCell state borrow spans this call.
    assert!(unsafe { IsDialogMessageW(settings_hwnd, &enter) }.as_bool());
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(key, WM_KEYUP, Some(WPARAM(0x0d)), Some(LPARAM(0xc000_0001))) };
    assert_eq!(settings.process_pending().unwrap(), None);
    press_key(key, 0x1b, false); // Esc cancels recording, not the settings window.
    assert_eq!(settings.process_pending().unwrap(), None);
    assert!(window_text(key).contains('K'));

    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(key, BM_CLICK, None, None) };
    settings.process_pending().unwrap();
    press_key(key, 0x79, true); // F10 arrives as a system-key message.
    settings.process_pending().unwrap();
    edited.hotkey.key = "F10".into();
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Apply(edited))
    );

    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(key, BM_CLICK, None, None) };
    settings.process_pending().unwrap();
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(key, WM_KEYDOWN, Some(WPARAM(0x0d)), Some(LPARAM(1))) };
    let tab = MSG {
        hwnd: key,
        message: WM_KEYDOWN,
        wParam: WPARAM(0x09),
        lParam: LPARAM(1),
        ..Default::default()
    };
    // SAFETY: The fixture dialog and stack MSG remain live for synchronous dispatch; no
    // RefCell state borrow spans this call.
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
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let alt = unsafe { GetDlgItem(Some(settings_content), 102) }.unwrap();
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe {
        SendMessageW(alt, BM_SETCHECK, Some(WPARAM(0)), None);
        SendMessageW(settings_hwnd, WM_COMMAND, Some(WPARAM(1)), None);
    }
    assert_eq!(settings.process_pending().unwrap(), None);
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
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
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(settings_hwnd, WM_CLOSE, None, None) };
    assert_eq!(
        settings.process_pending().unwrap(),
        Some(SettingsAction::Close)
    );
    drop(settings);
    // SAFETY: This is a handle-validity assertion only; it neither dereferences userdata nor
    // establishes ownership for releasing callback memory.
    assert!(!unsafe { IsWindow(Some(settings_hwnd)) }.as_bool());

    let readonly = SettingsWindow::new(&config, owner.0, None, false).unwrap();
    assert!(
        // SAFETY: The parent belongs to this test-owned live window tree; the returned
        // child handle is borrowed only while its owner survives.
        !unsafe {
            windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(
                GetDlgItem(Some(readonly.hwnd()), 1).unwrap(),
            )
        }
        .as_bool()
    );
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(readonly.hwnd(), WM_COMMAND, Some(WPARAM(1)), None) };
    assert_eq!(readonly.process_pending().unwrap(), None);
    drop(readonly);
    // SAFETY: This is a handle-validity assertion only; it neither dereferences userdata nor
    // establishes ownership for releasing callback memory.
    assert!(unsafe { IsWindow(Some(owner.0)) }.as_bool());
    let owner_hwnd = owner.0;
    drop(owner);
    // SAFETY: This is a handle-validity assertion only; it neither dereferences userdata nor
    // establishes ownership for releasing callback memory.
    assert!(!unsafe { IsWindow(Some(owner_hwnd)) }.as_bool());
}

#[test]
#[ignore = "requires an interactive Windows desktop; selects only fixture result text without system input or clipboard access"]
fn result_text_selection_is_exclusive_and_never_displays_a_caret() {
    use std::mem::size_of;
    use windows::Win32::{
        System::{SystemServices::MK_LBUTTON, Threading::GetCurrentThreadId},
        UI::{
            Controls::{EM_GETSEL, EM_POSFROMCHAR, EM_SETSEL},
            Input::KeyboardAndMouse::{
                GetFocus, GetKeyboardState, SetFocus, SetKeyboardState, VK_CONTROL, VK_LCONTROL,
                VK_LEFT, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL, VK_RIGHT, VK_RMENU, VK_RSHIFT,
                VK_SHIFT,
            },
            WindowsAndMessaging::{
                ES_NOHIDESEL, GUI_CARETBLINKING, GUITHREADINFO, GetGUIThreadInfo, SWP_NOMOVE,
                WM_GETFONT, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
                WM_SETFONT,
            },
        },
    };

    struct ThreadKeyboardState([u8; 256]);

    impl ThreadKeyboardState {
        fn save() -> Self {
            let mut state = [0; 256];
            // SAFETY: The local array supplies the complete writable keyboard-state buffer.
            unsafe { GetKeyboardState(&mut state) }.unwrap();
            Self(state)
        }

        fn set_shift(&self, pressed: bool) {
            let mut state = self.0;
            for key in [
                VK_SHIFT,
                VK_LSHIFT,
                VK_RSHIFT,
                VK_CONTROL,
                VK_LCONTROL,
                VK_RCONTROL,
                VK_MENU,
                VK_LMENU,
                VK_RMENU,
            ] {
                state[key.0 as usize] &= 0x7f;
            }
            if pressed {
                state[VK_SHIFT.0 as usize] |= 0x80;
                state[VK_LSHIFT.0 as usize] |= 0x80;
            }
            // SAFETY: SetKeyboardState changes only this test thread's queue state, not
            // physical/system input; the guard restores all original bytes on drop.
            unsafe { SetKeyboardState(&state) }.unwrap();
        }
    }

    impl Drop for ThreadKeyboardState {
        fn drop(&mut self) {
            // SAFETY: Restore the exact state saved on this same test thread; no input is sent.
            let _ = unsafe { SetKeyboardState(&self.0) };
        }
    }

    fn selection(edit: HWND) -> (u32, u32) {
        let (mut start, mut end) = (0_u32, 0_u32);
        // SAFETY: The fixture owns this EDIT and both local outputs remain uniquely
        // writable through synchronous EM_GETSEL processing; no pointer is retained.
        unsafe {
            SendMessageW(
                edit,
                EM_GETSEL,
                Some(WPARAM((&raw mut start) as usize)),
                Some(LPARAM((&raw mut end) as isize)),
            );
        }
        (start, end)
    }

    fn set_selection(edit: HWND, start: usize, end: isize) {
        // SAFETY: The fixture retains its EDIT; EM_SETSEL carries character positions only.
        unsafe { SendMessageW(edit, EM_SETSEL, Some(WPARAM(start)), Some(LPARAM(end))) };
    }

    #[track_caller]
    fn assert_no_caret(edit: HWND) {
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: Query this test thread, rather than the user's foreground thread, into
        // a correctly sized local structure that the API never retains.
        unsafe { GetGUIThreadInfo(GetCurrentThreadId(), &mut info) }.unwrap();
        assert_eq!(info.hwndFocus, edit, "the value must retain keyboard focus");
        assert!(
            !info.flags.contains(GUI_CARETBLINKING),
            "read-only result text must never display an insertion caret: {edit:?}"
        );
    }

    fn character_point(edit: HWND, index: usize) -> LPARAM {
        // SAFETY: The live EDIT processes a scalar character-index query synchronously.
        let position = unsafe { SendMessageW(edit, EM_POSFROMCHAR, Some(WPARAM(index)), None) };
        assert_ne!(position.0, -1, "the fixture character must be visible");
        let x = (position.0 as u16 as i16) as i32 + 1;
        let mut bounds = RECT::default();
        // SAFETY: The fixture retains this EDIT and bounds is a writable local output.
        unsafe { GetClientRect(edit, &mut bounds) }.unwrap();
        let y = (bounds.bottom - bounds.top) / 2;
        LPARAM(((x as u16 as u32) | ((y as u16 as u32) << 16)) as isize)
    }

    fn drag_selection(edit: HWND, start: usize, end: usize) {
        let first = character_point(edit, start);
        let last = character_point(edit, end);
        for (message, buttons, point) in [
            (WM_LBUTTONDOWN, MK_LBUTTON.0 as usize, first),
            (WM_MOUSEMOVE, MK_LBUTTON.0 as usize, last),
            (WM_LBUTTONUP, 0, last),
        ] {
            // SAFETY: These scalar mouse messages target only the live fixture EDIT;
            // they send no system input and the normal button-up releases its capture.
            unsafe { SendMessageW(edit, message, Some(WPARAM(buttons)), Some(point)) };
            assert_no_caret(edit);
        }
        let (start, end) = selection(edit);
        assert!(end > start, "native mouse dragging must still select text");
    }

    let _dpi = ScopedPmv2::enter().unwrap();
    let keyboard = ThreadKeyboardState::save();
    keyboard.set_shift(false);
    let result = ResultWindow::new_after_copy_failure(
        color_picker::core::state::PickedColor {
            rgb: Rgb8::new(224, 0, 0),
            source: cursor_position().unwrap(),
            kind: SampleKind::Live,
        },
        HWND::default(),
        ColorFormat::Hex,
        Default::default(),
        "Synthetic copy failure for read-only selection testing",
    )
    .unwrap();
    assert_eq!(result.process_pending().unwrap(), None);
    let child = |parent, id| {
        // SAFETY: Each parent belongs to this test-owned live result tree.
        unsafe { GetDlgItem(Some(parent), id) }.unwrap()
    };
    let content = child(result.hwnd(), 300);
    let edits = std::array::from_fn::<_, 4, _>(|index| child(content, 30 + index as i32));
    let status = child(result.hwnd(), 12);

    // Each focus transfer must collapse the previous real selection; hiding its
    // highlight alone would allow stale text to reappear when that field is revisited.
    let mut previous = None;
    for edit in [status].into_iter().chain(edits) {
        // SAFETY: Query scalar style bits on the fixture-owned EDIT.
        assert_eq!(unsafe { GetWindowLongW(edit, GWL_STYLE) } & ES_NOHIDESEL, 0);
        // SAFETY: Focus moves only among this fixture's controls on their owning thread.
        unsafe { SetFocus(Some(edit)) }.unwrap();
        assert_no_caret(edit);
        if let Some(previous) = previous {
            let (start, end) = selection(previous);
            assert_eq!(start, end, "the previous field must be deselected");
        }
        set_selection(edit, 0, -1);
        let (start, end) = selection(edit);
        assert!(end > start);
        assert_no_caret(edit);
        previous = Some(edit);
    }
    // SAFETY: Focus moves to the first fixture EDIT, which previously had a selection.
    unsafe { SetFocus(Some(edits[0])) }.unwrap();
    let (start, end) = selection(edits[0]);
    assert_eq!(start, end, "returning must not restore the old selection");
    assert_no_caret(edits[0]);

    set_selection(edits[0], 0, -1);
    // Let native EDIT mouse handling perform the next cross-field focus transfer.
    drag_selection(edits[1], 0, 3);
    let (start, end) = selection(edits[0]);
    assert_eq!(
        start, end,
        "mouse selection must deselect the previous field"
    );

    let point = character_point(edits[2], 1);
    for (message, buttons) in [
        (WM_LBUTTONDOWN, MK_LBUTTON.0 as usize),
        (WM_LBUTTONUP, 0),
        (WM_LBUTTONDBLCLK, MK_LBUTTON.0 as usize),
        (WM_LBUTTONUP, 0),
    ] {
        // SAFETY: The complete native double-click sequence targets this fixture's
        // CSS RGB EDIT only and generates no system mouse input.
        unsafe { SendMessageW(edits[2], message, Some(WPARAM(buttons)), Some(point)) };
        assert_no_caret(edits[2]);
    }
    let (start, end) = selection(edits[2]);
    assert!(
        end > start,
        "native double-click word selection must remain available"
    );
    let (start, end) = selection(edits[1]);
    assert_eq!(start, end);
    let old = selection(edits[2]);
    drag_selection(edits[2], 4, 7);
    assert_ne!(
        selection(edits[2]),
        old,
        "a new drag must replace the same-field selection"
    );

    // Shift extends the native selection even though its insertion caret is hidden.
    // SAFETY: Focus moves only to the fixture HSL EDIT on its creating thread.
    unsafe { SetFocus(Some(edits[3])) }.unwrap();
    set_selection(edits[3], 0, 0);
    keyboard.set_shift(true);
    press_key(edits[3], VK_RIGHT.0 as usize, false);
    assert_eq!(selection(edits[3]), (0, 1));
    assert_no_caret(edits[3]);
    press_key(edits[3], VK_RIGHT.0 as usize, false);
    assert_eq!(selection(edits[3]), (0, 2));
    assert_no_caret(edits[3]);
    keyboard.set_shift(false);
    press_key(edits[3], VK_LEFT.0 as usize, false);
    let (start, end) = selection(edits[3]);
    assert_eq!(start, end);
    assert_no_caret(edits[3]);

    // Font and geometry updates can recreate the native insertion caret even
    // while focus stays in the same control.
    // SAFETY: These messages borrow the fixture EDIT's existing font and copy
    // scalar layout values; the fixture retains every handle throughout.
    unsafe {
        let font = SendMessageW(edits[3], WM_GETFONT, None, None);
        SendMessageW(
            edits[3],
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        assert_no_caret(edits[3]);
        let mut bounds = RECT::default();
        GetClientRect(edits[3], &mut bounds).unwrap();
        SetWindowPos(
            edits[3],
            None,
            0,
            0,
            bounds.right + 1,
            bounds.bottom,
            SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .unwrap();
        assert_no_caret(edits[3]);
    }

    let copy = child(content, 103);
    // SAFETY: Focus the row button only; no BM_CLICK is sent and no copy is queued.
    unsafe { SetFocus(Some(copy)) }.unwrap();
    // SAFETY: GetFocus is a scalar query on this test thread.
    assert_eq!(unsafe { GetFocus() }, copy);
    let (start, end) = selection(edits[3]);
    assert_eq!(start, end);
    assert_eq!(result.process_pending().unwrap(), None);
}

#[test]
#[ignore = "requires an interactive Windows desktop; briefly displays synthetic magnifier windows without input or desktop sampling"]
fn edge_freeze_preserves_initial_source_and_standard_window_size() {
    let _dpi = ScopedPmv2::enter().unwrap();
    let cursor = cursor_position().unwrap();
    let monitors = Monitors::enumerate().unwrap();
    let monitor = monitors.at(cursor).unwrap();
    let work = monitor.work_area;
    let center = ScreenPointPx {
        x: (i64::from(work.left) + i64::from(work.width()) / 2) as i32,
        y: (i64::from(work.top) + i64::from(work.height()) / 2) as i32,
    };
    // These edge positions are inside the pixel area, clear of the footer and
    // right border. Those surfaces intentionally do not select a source pixel.
    let edge_x = work.left + 20.min(work.width() / 2) as i32;
    let edge_y = work.top + 20.min(work.height() / 2) as i32;
    let mut standard_size = None;
    for focus in [
        center,
        ScreenPointPx {
            x: edge_x,
            y: center.y,
        },
        ScreenPointPx {
            x: center.x,
            y: edge_y,
        },
        ScreenPointPx {
            x: edge_x,
            y: edge_y,
        },
    ] {
        let mut captures = 0;
        let mut capture_size = None;
        let appearance = color_picker::app::config::AppearanceConfig {
            background_transparency_percent: 0,
            ..Default::default()
        };
        let magnifier = MagnifierWindow::capture_with_appearance(
            focus,
            monitor.bounds,
            work,
            appearance,
            |rect| {
                captures += 1;
                assert_eq!(rect.intersection(monitor.bounds), Some(rect));
                assert!(rect.contains(focus));
                capture_size = Some((rect.width(), rect.height()));
                let mut bgrx = Vec::with_capacity((rect.width() * rect.height() * 4) as usize);
                for y in 0..rect.height() {
                    for x in 0..rect.width() {
                        let r = (rect.left + x as i32) as u8;
                        let g = (rect.top + y as i32) as u8;
                        bgrx.extend_from_slice(&[r ^ g, g, r, 0xff]);
                    }
                }
                Ok(FrozenImage {
                    origin: ScreenPointPx {
                        x: rect.left,
                        y: rect.top,
                    },
                    width: rect.width(),
                    height: rect.height(),
                    stride_bytes: rect.width() as usize * 4,
                    bgrx,
                })
            },
        )
        .unwrap();
        assert_eq!(captures, 1, "freeze must use one immutable snapshot");
        let bounds = magnifier.rect().unwrap();
        // SAFETY: Query only the live fixture HWND while its window owner remains in scope.
        let dpi = unsafe { GetDpiForWindow(magnifier.hwnd()) };
        let footer_height = (28 * dpi + 48) / 96;
        assert_eq!(
            capture_size,
            Some((
                (bounds.width() / 4).max(65).min(monitor.bounds.width()),
                ((bounds.height() - footer_height) / 4)
                    .max(65)
                    .min(monitor.bounds.height()),
            )),
            "capture grows with the DPI-scaled viewport without changing pixel magnification"
        );
        assert_eq!(bounds.intersection(work), Some(bounds));
        let size = (bounds.width(), bounds.height());
        assert_eq!(size, *standard_size.get_or_insert(size));
        let picked = magnifier
            .hit_test(focus)
            .expect("initial pixel is selectable");
        assert_eq!(picked.source, focus);
        let r = focus.x as u8;
        let g = focus.y as u8;
        assert_eq!(picked.rgb, Rgb8::new(r, g, r ^ g));
        assert_eq!(picked.kind, SampleKind::Frozen);
        assert_eq!(magnifier.scale_factor(), 4);
        let hwnd = magnifier.hwnd();
        // SAFETY: Query scalar window style bits for the test-owned live HWND; no
        // callback pointer is interpreted.
        let style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
        assert_eq!(
            style & (WS_EX_TOPMOST | WS_EX_NOACTIVATE).0,
            (WS_EX_TOPMOST | WS_EX_NOACTIVATE).0
        );
        drop(magnifier);
        // SAFETY: This is a handle-validity assertion only; it neither dereferences
        // userdata nor establishes ownership for releasing callback memory.
        assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
    }
}

#[test]
#[ignore = "requires an interactive Windows desktop; constructs only a hidden magnifier with a failed synthetic capture"]
fn failed_capture_destroys_the_hidden_magnifier() {
    use windows::{
        Win32::{
            Foundation::E_FAIL,
            System::Threading::GetCurrentThreadId,
            UI::WindowsAndMessaging::{EnumThreadWindows, GetClassNameW},
        },
        core::{BOOL, Error},
    };

    /// # Safety
    /// EnumThreadWindows calls this synchronously with the local Vec pointer
    /// supplied by magnifiers; the pointer must remain unique for enumeration.
    unsafe extern "system" fn collect(hwnd: HWND, context: LPARAM) -> BOOL {
        // SAFETY: EnumThreadWindows synchronously passes the unique Vec pointer supplied
        // below; no other borrow exists during enumeration.
        let windows = unsafe { &mut *(context.0 as *mut Vec<HWND>) };
        if windows.try_reserve(1).is_err() {
            return false.into();
        }
        windows.push(hwnd);
        true.into()
    }

    let magnifiers = || {
        let mut windows = Vec::<HWND>::new();
        assert!(
            // SAFETY: The callback receives the live local Vec pointer synchronously
            // on this thread; it does not retain the pointer.
            unsafe {
                EnumThreadWindows(
                    GetCurrentThreadId(),
                    Some(collect),
                    LPARAM((&raw mut windows) as isize),
                )
            }
            .as_bool()
        );
        windows.retain(|hwnd| {
            let mut class = [0_u16; 64];
            // SAFETY: The test HWND is borrowed for this query and the output UTF-16
            // slice supplies its exact writable capacity.
            let length = unsafe { GetClassNameW(*hwnd, &mut class) } as usize;
            String::from_utf16_lossy(&class[..length]) == "ColorPicker.Magnifier.v1"
        });
        windows
    };
    let _dpi = ScopedPmv2::enter().unwrap();
    let cursor = cursor_position().unwrap();
    let monitors = Monitors::enumerate().unwrap();
    let monitor = monitors.at(cursor).unwrap();
    let before = magnifiers();
    let mut hidden = None;
    let outcome = MagnifierWindow::capture_with_appearance(
        cursor,
        monitor.bounds,
        monitor.work_area,
        Default::default(),
        |_| {
            let created: Vec<_> = magnifiers()
                .into_iter()
                .filter(|hwnd| !before.contains(hwnd))
                .collect();
            assert_eq!(created.len(), 1);
            let hwnd = created[0];
            // SAFETY: Query visibility on the fixture HWND without dereferencing or
            // retaining any native pointer.
            assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool());
            let mut rect = RECT::default();
            // SAFETY: The fixture window remains alive on this thread and each RECT
            // output is valid writable stack storage.
            unsafe { GetWindowRect(hwnd, &mut rect) }.unwrap();
            assert_eq!((rect.right - rect.left, rect.bottom - rect.top), (1, 1));
            hidden = Some(hwnd);
            Err(Error::new(E_FAIL, "Synthetic capture failure"))
        },
    );
    assert!(outcome.is_err());
    let hidden = hidden.expect("the freeze operation must reach the capture callback");
    // SAFETY: This is a handle-validity assertion only; it neither dereferences userdata nor
    // establishes ownership for releasing callback memory.
    assert!(!unsafe { IsWindow(Some(hidden)) }.as_bool());
    assert_eq!(magnifiers(), before);
}

fn window_text(hwnd: HWND) -> String {
    let mut text = [0_u16; 256];
    // SAFETY: This live fixture control is queried synchronously into a writable UTF-16
    // slice; the API receives the slice capacity.
    let length = unsafe { GetWindowTextW(hwnd, &mut text) };
    String::from_utf16(&text[..length as usize]).unwrap()
}

fn press_key(hwnd: HWND, key: usize, system: bool) {
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
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
        // SAFETY: This thread owns the fixture HWND; synchronous teardown finishes
        // before its surrounding fixture resources are released.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
