#![cfg(windows)]

#[path = "support/pixel_fixture.rs"]
mod pixel_fixture;

use color_picker::{
    app::{
        config::Config,
        i18n::{self, Language},
    },
    ui::windows::settings::SettingsWindow,
};
use pixel_fixture::ScopedPmv2;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::*,
        System::SystemServices::SS_CENTERIMAGE,
        UI::{
            Controls::*,
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

#[test]
#[ignore = "requires an interactive Windows desktop; creates and resizes only its own settings window"]
fn settings_header_and_content_recover_after_scrollbar_reflow() {
    i18n::set_language(Language::SimplifiedChinese);
    let _dpi = ScopedPmv2::enter().unwrap();
    // SAFETY: The class/title strings remain live and terminated during creation; this
    // thread owns the returned fixture window and any callback data.
    let owner = TestOwner(unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Settings layout test"),
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
    });
    let settings = SettingsWindow::new(&Config::default(), owner.0, None, true).unwrap();
    settings.process_pending().unwrap();
    let hwnd = settings.hwnd();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let viewport = unsafe { GetDlgItem(Some(hwnd), 200) }.unwrap();
    // SAFETY: The viewport owns this live scrolling document; controls retain
    // their document-relative positions while this one child HWND moves.
    let content = unsafe { GetDlgItem(Some(viewport), 201) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let status = unsafe { GetDlgItem(Some(hwnd), 14) }.unwrap();
    let mut class = [0_u16; 32];
    // SAFETY: The test HWND is borrowed for this query and the output UTF-16 slice supplies
    // its exact writable capacity.
    let length = unsafe { GetClassNameW(status, &mut class) } as usize;
    assert!(
        String::from_utf16_lossy(&class[..length]).eq_ignore_ascii_case("static"),
        "the ordinary settings hint must be a non-selectable static label"
    );
    assert_eq!(
        // SAFETY: Query scalar window style bits for the test-owned live HWND; no
        // callback pointer is interpreted.
        unsafe { GetWindowLongW(status, GWL_STYLE) } as u32 & WS_TABSTOP.0,
        0,
        "the ordinary hint must not enter the keyboard tab sequence"
    );
    // SAFETY: Query visibility on the fixture HWND without dereferencing or retaining any
    // native pointer.
    assert!(unsafe { IsWindowVisible(status) }.as_bool());
    let original_scrollbar_gutter = scrollbar_gutter(viewport);
    assert!(
        original_scrollbar_gutter > 0,
        "the default viewport must naturally scroll through the setting cards"
    );
    assert_native_text_fits(content);
    assert_form_layout(content, Some(false));
    assert_controls_inside_client(content);

    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let details = unsafe { GetDlgItem(Some(hwnd), 29) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let close = unsafe { GetDlgItem(Some(hwnd), 2) }.unwrap();
    // SAFETY: Query visibility on the fixture HWND without dereferencing or retaining any
    // native pointer.
    assert!(!unsafe { IsWindowVisible(details) }.as_bool());
    let long_error = "保存配置失败：配置文件暂时无法写入，请检查文件权限后重试。\r\n".repeat(12);
    settings.show_status(&long_error, false).unwrap();
    // SAFETY: Query visibility on the fixture HWND without dereferencing or retaining any
    // native pointer.
    assert!(!unsafe { IsWindowVisible(status) }.as_bool());
    // SAFETY: Query visibility on the fixture HWND without dereferencing or retaining any
    // native pointer.
    assert!(unsafe { IsWindowVisible(details) }.as_bool());
    assert_eq!(window_text(details), long_error);
    assert_eq!(
        // SAFETY: Both dialog and starting control belong to this live fixture tree; the
        // next handle is borrowed for comparison only.
        unsafe { GetNextDlgTabItem(hwnd, Some(close), true) }.unwrap(),
        details,
        "a visible long diagnostic must remain keyboard reachable"
    );
    let short_hint = "更改已保存。";
    settings.show_status(short_hint, true).unwrap();
    // SAFETY: Query visibility on the fixture HWND without dereferencing or retaining any
    // native pointer.
    assert!(unsafe { IsWindowVisible(status) }.as_bool());
    // SAFETY: Query visibility on the fixture HWND without dereferencing or retaining any
    // native pointer.
    assert!(!unsafe { IsWindowVisible(details) }.as_bool());
    assert_eq!(window_text(status), short_hint);
    assert_ne!(
        // SAFETY: Both dialog and starting control belong to this live fixture tree; the
        // next handle is borrowed for comparison only.
        unsafe { GetNextDlgTabItem(hwnd, Some(close), true) }.unwrap(),
        details,
        "a hidden diagnostic must leave the keyboard tab sequence"
    );

    let original = window_rect(hwnd);
    let original_client = client_on_screen(hwnd);
    // SAFETY: Query only the live fixture HWND while its window owner remains in scope.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    eprintln!("native HWND/font coverage: DPI={dpi}, CJK/English, client widths 500/360/280 DIP");
    let nonclient_width =
        original.right - original.left - (original_client.right - original_client.left);
    let nonclient_height =
        original.bottom - original.top - (original_client.bottom - original_client.top);
    let dip = |value: i32| ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32;
    // Exercise the actual HWNDs and their selected CJK/English fonts at this
    // monitor's real DPI. Other DPI font/layout combinations are tested offscreen
    // in the source unit tests, without pretending WM_DPICHANGED changes monitors.
    for language in [Language::SimplifiedChinese, Language::English] {
        i18n::set_language(language);
        settings.refresh_language().unwrap();
        for width in [500, 360, 280] {
            // SAFETY: Only this fixture is resized; its content remains native
            // and the local size calculation preserves the non-client chrome.
            unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    original.left,
                    original.top,
                    dip(width) + nonclient_width,
                    dip(350) + nonclient_height,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )
                .unwrap();
            }
            settings.process_pending().unwrap();
            assert_form_layout(
                content,
                match width {
                    500 => Some(false),
                    280 => Some(true),
                    _ => None,
                },
            );
            assert_controls_inside_client(content);
            assert_native_text_fits(content);
        }
    }
    i18n::set_language(Language::SimplifiedChinese);
    settings.refresh_language().unwrap();
    // SAFETY: Resize/reposition only the test-owned window on its UI thread; dimensions are
    // scalar and no callback borrow spans the call.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            original.left,
            original.top,
            dip(280) + nonclient_width,
            dip(350) + nonclient_height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .unwrap();
    }
    settings.process_pending().unwrap();
    let content_window = window_rect(viewport);
    let content_client = client_on_screen(viewport);
    assert!(
        content_client.right - content_client.left < content_window.right - content_window.left,
        "the short content pane must have a real vertical scrollbar"
    );
    assert_form_layout(content, Some(true));
    assert_controls_inside_client(content);

    let mut content_scroll = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE,
        ..Default::default()
    };
    // SAFETY: The owned viewport fills initialized, correctly sized native output.
    unsafe { GetScrollInfo(viewport, SB_VERT, &mut content_scroll).unwrap() };
    // SAFETY: Hide only this fixture while its temporary height exceeds the
    // desktop. All following checks inspect native geometry/fonts, not pixels.
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    // SAFETY: Enlarge only this fixture beyond the content height to verify
    // scrollbar release. No offscreen desktop pixels are read by this test.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            original.left,
            original.top,
            original.right - original.left,
            content_scroll.nMax + 1 + nonclient_height + dip(128),
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .unwrap();
    }
    settings.process_pending().unwrap();
    assert_eq!(
        scrollbar_gutter(viewport),
        0,
        "a tall viewport must release its scrollbar gutter"
    );
    assert_form_layout(content, Some(false));
    assert_native_text_fits(content);

    // SAFETY: Resize/reposition only the test-owned window on its UI thread; dimensions are
    // scalar and no callback borrow spans the call.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            original.left,
            original.top,
            original.right - original.left,
            original.bottom - original.top,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .unwrap();
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    settings.process_pending().unwrap();
    assert_eq!(
        scrollbar_gutter(viewport),
        original_scrollbar_gutter,
        "restoring the default viewport must restore natural scrolling"
    );
    assert_form_layout(content, Some(false));
    assert_native_text_fits(content);
    assert_controls_inside_client(content);
}

fn assert_form_layout(content: HWND, expected_stacked: Option<bool>) {
    let bounds = |id| window_rect(child(content, id));
    let title = bounds(15);
    let subtitle = bounds(16);
    let language_label = bounds(28);
    let language_combo = bounds(110);
    let format_label = bounds(13);
    let format_combo = bounds(105);
    let key_label = bounds(11);
    let key = bounds(104);
    let client = client_on_screen(content);
    // SAFETY: The borrowed document remains alive for this scalar DPI query.
    let dpi = unsafe { GetDpiForWindow(content) };
    let center_tolerance = ((i64::from(dpi) + 95) / 96) as i32;
    assert_eq!(title.left, subtitle.left);
    assert!(
        title.bottom <= subtitle.top && subtitle.bottom <= language_label.top,
        "the language card must follow the separate heading and subtitle"
    );
    assert_eq!(language_combo.right, format_combo.right);
    assert_eq!(language_combo.right, key.right);
    assert_eq!(
        language_combo.right - language_combo.left,
        format_combo.right - format_combo.left
    );
    assert_eq!(
        language_combo.right - language_combo.left,
        key.right - key.left
    );
    assert_eq!(language_label.left, format_label.left);
    assert_eq!(language_label.left, key_label.left);
    assert!(
        ((language_label.left - client.left) - (client.right - bounds(107).right)).abs() <= 2,
        "form content must have symmetric inner margins"
    );
    for (id, label, editor) in [
        (28, language_label, language_combo),
        (13, format_label, format_combo),
        (11, key_label, key),
    ] {
        let stacked = label.bottom <= editor.top;
        if let Some(expected) = expected_stacked {
            assert_eq!(
                stacked, expected,
                "unexpected form-row reflow: {label:?}, {editor:?}"
            );
        }
        if !stacked {
            assert!(label.right <= editor.left);
            assert!(
                (label.top + label.bottom - editor.top - editor.bottom).abs()
                    <= 2 * center_tolerance,
                "label {id} must use the actual native collapsed/editor height for vertical alignment: {label:?}, {editor:?}"
            );
        }
    }
    assert!(
        key.bottom <= bounds(12).top,
        "the shortcut hint must occupy a row below the key button"
    );
    let border_slider = bounds(107);
    let transparency_slider = bounds(108);
    for (label, slider, value) in [
        (bounds(21), border_slider, bounds(22)),
        (bounds(23), transparency_slider, bounds(24)),
    ] {
        assert!(label.right <= value.left);
        assert!(
            slider.top >= label.bottom.max(value.bottom),
            "slider tracks must be below their label and value"
        );
        assert_eq!(slider.left, label.left);
        assert_eq!(slider.right, value.right);
    }
    assert_eq!(border_slider.left, transparency_slider.left);
    assert_eq!(border_slider.right, transparency_slider.right);
    assert_eq!(bounds(22).left, bounds(24).left);
    assert!(border_slider.bottom <= bounds(23).top);
}

fn scrollbar_gutter(content: HWND) -> i32 {
    let client = client_on_screen(content);
    let window = window_rect(content);
    (window.right - window.left) - (client.right - client.left)
}

fn child(parent: HWND, id: i32) -> HWND {
    // SAFETY: These borrowed handles remain in the fixture's owned live tree.
    unsafe { GetDlgItem(Some(parent), id) }.unwrap()
}

fn assert_controls_inside_client(content: HWND) {
    let client = client_on_screen(content);
    for id in [
        15, 16, 28, 110, 10, 101, 102, 103, 11, 104, 12, 17, 13, 105, 109, 106, 20, 21, 107, 22,
        23, 108, 24, 25, 26, 27, 18, 19,
    ] {
        // SAFETY: The parent belongs to this test-owned live window tree; the returned
        // child handle is borrowed only while its owner survives.
        let control = unsafe { GetDlgItem(Some(content), id) }.unwrap();
        let bounds = window_rect(control);
        assert!(
            bounds.left >= client.left
                && bounds.right <= client.right
                && bounds.top >= client.top
                && bounds.bottom <= client.bottom,
            "control {id} overflows the actual content client area: {bounds:?} in {client:?}"
        );
    }
}

fn assert_native_text_fits(content: HWND) {
    let ids = [
        15, 16, 28, 110, 10, 101, 102, 103, 11, 104, 12, 17, 13, 105, 109, 106, 20, 21, 107, 22,
        23, 108, 24, 25, 26, 27, 18, 19,
    ];
    // Check the actual native window rectangles independently of the layout plan.
    // The cards are painted by the parent and are intentionally not child HWNDs.
    for (index, id) in ids.iter().copied().enumerate() {
        let bounds = window_rect(child(content, id));
        for other in ids.iter().copied().skip(index + 1) {
            let other_bounds = window_rect(child(content, other));
            assert!(
                bounds.right <= other_bounds.left
                    || other_bounds.right <= bounds.left
                    || bounds.bottom <= other_bounds.top
                    || other_bounds.bottom <= bounds.top,
                "native controls {id} and {other} overlap: {bounds:?}, {other_bounds:?}"
            );
        }
    }

    for id in [
        15, 16, 28, 10, 11, 12, 17, 13, 20, 21, 22, 23, 24, 25, 27, 18, 19,
    ] {
        let control = child(content, id);
        let client = client_on_screen(control);
        // SS_CENTERIMAGE is a native single-line static. Other labels wrap at
        // their real client width, using their own currently selected font.
        // SAFETY: Query style bits on a borrowed live test control.
        let single_line =
            unsafe { GetWindowLongW(control, GWL_STYLE) } as u32 & SS_CENTERIMAGE.0 != 0;
        assert_text_extent(control, id, &window_text(control), client, single_line);
    }

    for id in [101, 102, 103, 109, 106] {
        let control = child(content, id);
        let mut caption = client_on_screen(control);
        // SAFETY: All handles are borrowed from the live test fixture; native
        // theme queries only read their current DPI/style, and the theme closes
        // before any assertion can unwind.
        let glyph_width = unsafe {
            let dpi = GetDpiForWindow(control);
            let theme = OpenThemeData(Some(control), w!("Button"));
            let width = if theme.0 == 0 {
                GetSystemMetricsForDpi(SM_CXMENUCHECK, dpi)
            } else {
                let size = GetThemePartSize(
                    theme,
                    None,
                    BP_CHECKBOX.0,
                    CBS_UNCHECKEDNORMAL.0,
                    None,
                    TS_TRUE,
                );
                let _ = CloseThemeData(theme);
                size.map(|size| size.cx)
                    .unwrap_or_else(|_| GetSystemMetricsForDpi(SM_CXMENUCHECK, dpi))
            };
            width + ((u64::from(dpi) * 4 + 48) / 96) as i32
        };
        caption.left += glyph_width;
        assert_text_extent(control, id, &window_text(control), caption, false);
    }

    let key = child(content, 104);
    assert_text_extent(key, 104, &window_text(key), client_on_screen(key), true);
    for id in [110, 105] {
        let combo = child(content, id);
        let mut info = COMBOBOXINFO {
            cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: The live combo writes a correctly sized output and copies each
        // string into a buffer allocated from that item's reported length.
        unsafe {
            GetComboBoxInfo(combo, &mut info).unwrap();
            let count = SendMessageW(combo, CB_GETCOUNT, None, None).0;
            assert!(count > 0);
            for index in 0..count {
                let length =
                    SendMessageW(combo, CB_GETLBTEXTLEN, Some(WPARAM(index as usize)), None).0;
                assert!(length >= 0);
                let mut text = vec![0_u16; length as usize + 1];
                let copied = SendMessageW(
                    combo,
                    CB_GETLBTEXT,
                    Some(WPARAM(index as usize)),
                    Some(LPARAM(text.as_mut_ptr() as isize)),
                )
                .0;
                assert_eq!(copied, length);
                assert_text_extent(
                    combo,
                    id,
                    &String::from_utf16(&text[..copied as usize]).unwrap(),
                    info.rcItem,
                    true,
                );
            }
        }
    }
}

fn assert_text_extent(control: HWND, id: i32, text: &str, available: RECT, single_line: bool) {
    let width = available.right - available.left;
    let height = available.bottom - available.top;
    let measured = measure_native_text(control, text, width, single_line);
    assert!(
        measured.cx <= width && measured.cy <= height,
        "control {id} clips its native-font text {text:?}: measured {measured:?}, available {width}x{height}"
    );
}

fn measure_native_text(control: HWND, text: &str, width: i32, single_line: bool) -> SIZE {
    let mut text: Vec<u16> = text.encode_utf16().collect();
    let mut measured = RECT {
        right: width,
        ..Default::default()
    };
    let flags = DT_CALCRECT
        | DT_NOPREFIX
        | if single_line {
            DT_SINGLELINE
        } else {
            DT_WORDBREAK
        };
    // SAFETY: This thread owns the live control. Its font is borrowed and selected
    // only into a temporary DC. Restore/release happens before any assertion;
    // DrawText does not modify the UTF-16 storage without DT_MODIFYSTRING.
    unsafe {
        let dc = GetDC(Some(control));
        assert!(!dc.0.is_null());
        let font = SendMessageW(control, WM_GETFONT, None, None);
        let previous = SelectObject(dc, HGDIOBJ(font.0 as *mut _));
        DrawTextW(dc, &mut text, &mut measured, flags);
        SelectObject(dc, previous);
        ReleaseDC(Some(control), dc);
    }
    SIZE {
        cx: measured.right - measured.left,
        cy: measured.bottom - measured.top,
    }
}

fn window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: The fixture window remains alive on this thread and each RECT output is valid
    // writable stack storage.
    unsafe { GetWindowRect(hwnd, &mut rect).unwrap() };
    rect
}

fn window_text(hwnd: HWND) -> String {
    // SAFETY: The fixture control remains alive on this thread while its text length is queried.
    let length = unsafe { GetWindowTextLengthW(hwnd) } as usize;
    let mut text = vec![0_u16; length + 1];
    // SAFETY: This live fixture control is queried synchronously into a writable UTF-16
    // slice; the API receives the slice capacity.
    let copied = unsafe { GetWindowTextW(hwnd, &mut text) } as usize;
    String::from_utf16(&text[..copied]).unwrap()
}

fn client_on_screen(hwnd: HWND) -> RECT {
    let mut client = RECT::default();
    let mut origin = POINT::default();
    // SAFETY: The fixture window remains alive and the RECT/POINT outputs are writable local
    // values used only for these synchronous queries.
    unsafe {
        GetClientRect(hwnd, &mut client).unwrap();
        assert!(ClientToScreen(hwnd, &mut origin).as_bool());
    }
    RECT {
        left: origin.x + client.left,
        top: origin.y + client.top,
        right: origin.x + client.right,
        bottom: origin.y + client.bottom,
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
