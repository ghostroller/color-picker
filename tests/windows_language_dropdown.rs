#![cfg(windows)]

#[path = "support/pixel_fixture.rs"]
mod pixel_fixture;

use color_picker::{
    app::{
        config::Config,
        i18n::{self, Language},
    },
    ui::windows::settings::{SettingsAction, SettingsWindow},
};
use pixel_fixture::ScopedPmv2;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT, WPARAM},
        Graphics::Gdi::{
            GGI_MARK_NONEXISTING_GLYPHS, GetDC, GetGlyphIndicesW, HGDIOBJ, ReleaseDC, SelectObject,
            UpdateWindow,
        },
        UI::{
            Controls::{COMBOBOXINFO, GetComboBoxInfo},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

#[test]
#[ignore = "requires an interactive Windows desktop; opens only its own settings dropdowns"]
fn language_choices_are_visible_on_the_first_open_after_applying() {
    i18n::set_language(Language::SimplifiedChinese);
    let _dpi = ScopedPmv2::enter().unwrap();
    // SAFETY: The class/title strings remain live and terminated during creation; this
    // thread owns the returned fixture window and any callback data.
    let owner = TestOwner(unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Language dropdown test"),
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
    let config = Config {
        language: Language::SimplifiedChinese,
        ..Config::default()
    };
    let settings = SettingsWindow::new(&config, owner.0, None, true).unwrap();
    settings.process_pending().unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let content = unsafe { GetDlgItem(Some(settings.hwnd()), 200) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let combo = unsafe { GetDlgItem(Some(content), 110) }.unwrap();
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: The owned settings combo is live and the output COMBOBOXINFO has its required
    // size initialized.
    unsafe {
        GetComboBoxInfo(combo, &mut info).unwrap();
    }
    for target in [
        Language::English,
        Language::SimplifiedChinese,
        Language::English,
    ] {
        let index = usize::from(target == Language::English);
        // Materialize the native popup before changing the language, then
        // exercise Apply and inspect the very first reopening, not a warm-up.
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
        unsafe {
            SendMessageW(combo, CB_SHOWDROPDOWN, Some(WPARAM(1)), None);
            SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
            SendMessageW(combo, CB_SHOWDROPDOWN, Some(WPARAM(0)), None);
            SendMessageW(settings.hwnd(), WM_COMMAND, Some(WPARAM(1)), None);
        }
        let Some(SettingsAction::Apply(draft)) = settings.process_pending().unwrap() else {
            panic!("Expected the selected language to reach Apply");
        };
        assert_eq!(draft.language, target);
        i18n::set_language(target);
        settings.refresh_language().unwrap();
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
        unsafe {
            SendMessageW(combo, CB_SHOWDROPDOWN, Some(WPARAM(1)), None);
            let _ = UpdateWindow(info.hwndList);
        }
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
        let top = unsafe { SendMessageW(combo, CB_GETTOPINDEX, None, None) }.0;
        assert_eq!(top, 0, "The first language must not be scrolled out");
        assert_eq!(
            // SAFETY: These messages target live controls in this test-owned tree;
            // control values are scalar and the calls complete before owner
            // teardown.
            unsafe { SendMessageW(combo, CB_GETCURSEL, None, None) }.0,
            index as isize
        );
        let mut client = RECT::default();
        // SAFETY: The fixture window remains alive and the RECT/POINT outputs are
        // writable local values used only for these synchronous queries.
        unsafe {
            GetClientRect(info.hwndList, &mut client).unwrap();
        }
        for (index, expected) in ["简体中文", "English"].iter().enumerate() {
            let mut text = [0_u16; 32];
            // SAFETY: This fixture combo contains only the two known short language
            // labels; the 32-unit output buffer lives throughout this synchronous
            // message.
            let length = unsafe {
                SendMessageW(
                    combo,
                    CB_GETLBTEXT,
                    Some(WPARAM(index)),
                    Some(LPARAM(text.as_mut_ptr() as isize)),
                )
            }
            .0;
            assert!(length > 0);
            assert_eq!(
                String::from_utf16(&text[..length as usize]).unwrap(),
                *expected
            );
            let mut item = RECT::default();
            // SAFETY: The known live listbox receives a valid item index and a
            // writable RECT that lives for the synchronous message.
            let result = unsafe {
                SendMessageW(
                    info.hwndList,
                    LB_GETITEMRECT,
                    Some(WPARAM(index)),
                    Some(LPARAM((&raw mut item) as isize)),
                )
            }
            .0;
            assert_ne!(result, LB_ERR as isize);
            assert!(
                item.bottom > item.top && item.top >= client.top && item.bottom <= client.bottom,
                "Language {index} must be fully visible on the first opening: {item:?} in {client:?}"
            );
        }
        assert_chinese_glyphs_available(combo);
        assert_chinese_glyphs_available(info.hwndList);
        // SAFETY: These messages target live controls in this test-owned tree; control
        // values are scalar and the calls complete before owner teardown.
        unsafe {
            SendMessageW(combo, CB_SHOWDROPDOWN, Some(WPARAM(0)), None);
        }
    }
}

fn assert_chinese_glyphs_available(hwnd: HWND) {
    // The bilingual selector must render its Chinese name directly, including
    // in English mode, without depending on the first paint to resolve fallback.
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    let font = unsafe { SendMessageW(hwnd, WM_GETFONT, None, None) }.0;
    assert_ne!(font, 0);
    // SAFETY: Borrow the live control DC on this thread and pair it below with ReleaseDC for
    // the same HWND.
    let dc = unsafe { GetDC(Some(hwnd)) };
    assert!(!dc.is_invalid());
    // SAFETY: The control font/DC are borrowed while their owners live; restore the previous
    // object before releasing the DC.
    let old = unsafe { SelectObject(dc, HGDIOBJ(font as *mut _)) };
    let mut glyphs = [0; 4];
    // SAFETY: The selected borrowed control font is alive; four UTF-16 characters and four
    // output glyph slots cover the requested count.
    let result = unsafe {
        GetGlyphIndicesW(
            dc,
            w!("简体中文"),
            4,
            glyphs.as_mut_ptr(),
            GGI_MARK_NONEXISTING_GLYPHS,
        )
    };
    // SAFETY: The control font/DC are borrowed while their owners live; restore the previous
    // object before releasing the DC.
    unsafe {
        SelectObject(dc, old);
        ReleaseDC(Some(hwnd), dc);
    }
    assert_ne!(result, u32::MAX);
    assert!(
        !glyphs.contains(&0xffff),
        "The language font must include all Chinese label glyphs"
    );
}

struct TestOwner(HWND);
impl Drop for TestOwner {
    fn drop(&mut self) {
        // SAFETY: This thread owns the fixture HWND; synchronous teardown finishes
        // before its surrounding fixture resources are released.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
