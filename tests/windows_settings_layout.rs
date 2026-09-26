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
        Foundation::{HWND, POINT, RECT},
        Graphics::Gdi::ClientToScreen,
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
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
    let content = unsafe { GetDlgItem(Some(hwnd), 200) }.unwrap();
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
    assert_no_scrollbar_gutter(content);
    assert_balanced_header(content);
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
    let nonclient_width =
        original.right - original.left - (original_client.right - original_client.left);
    let nonclient_height =
        original.bottom - original.top - (original_client.bottom - original_client.top);
    let dip = |value: i32| ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32;
    // SAFETY: Resize/reposition only the test-owned window on its UI thread; dimensions are
    // scalar and no callback borrow spans the call.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            original.left,
            original.top,
            dip(360) + nonclient_width,
            dip(350) + nonclient_height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .unwrap();
    }
    settings.process_pending().unwrap();
    let content_window = window_rect(content);
    let content_client = client_on_screen(content);
    assert!(
        content_client.right - content_client.left < content_window.right - content_window.left,
        "the short content pane must have a real vertical scrollbar"
    );
    assert_controls_inside_client(content);

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
    }
    settings.process_pending().unwrap();
    assert_no_scrollbar_gutter(content);
    assert_balanced_header(content);
    assert_controls_inside_client(content);
}

fn assert_balanced_header(content: HWND) {
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let title = window_rect(unsafe { GetDlgItem(Some(content), 15) }.unwrap());
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let label = window_rect(unsafe { GetDlgItem(Some(content), 28) }.unwrap());
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let combo = window_rect(unsafe { GetDlgItem(Some(content), 110) }.unwrap());
    let client = client_on_screen(content);
    let left_margin = title.left - client.left;
    let right_margin = client.right - combo.right;
    assert!(left_margin > 0 && right_margin > 0);
    assert!(
        (left_margin - right_margin).abs() <= 2,
        "header margins differ: left={left_margin}px, right={right_margin}px"
    );
    assert!(
        (label.top + label.bottom - combo.top - combo.bottom).abs() <= 2,
        "language label and collapsed combo must share their vertical center: {label:?}, {combo:?}"
    );
}

fn assert_no_scrollbar_gutter(content: HWND) {
    let client = client_on_screen(content);
    let window = window_rect(content);
    assert_eq!(
        client.right - client.left,
        window.right - window.left,
        "a full-height settings pane must release its scrollbar gutter"
    );
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
            bounds.left >= client.left && bounds.right <= client.right,
            "control {id} overflows the actual content client area: {bounds:?} in {client:?}"
        );
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
