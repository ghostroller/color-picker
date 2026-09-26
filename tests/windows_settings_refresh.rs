#![cfg(windows)]

#[path = "support/pixel_fixture.rs"]
mod pixel_fixture;

use std::cell::Cell;

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
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::{
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

#[test]
#[ignore = "requires an interactive Windows desktop; refreshes only its own settings window"]
fn unchanged_language_refresh_preserves_fonts_and_leaves_content_untouched() {
    i18n::set_language(Language::SimplifiedChinese);
    let _dpi = ScopedPmv2::enter().unwrap();
    // SAFETY: The class/title strings remain live and terminated during creation; this
    // thread owns the returned fixture window and any callback data.
    let owner = TestOwner(unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Settings refresh test"),
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
    let title = unsafe { GetDlgItem(Some(content), 15) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let checkbox = unsafe { GetDlgItem(Some(content), 101) }.unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let preview = unsafe { GetDlgItem(Some(content), 26) }.unwrap();
    assert_eq!(window_text(title), "偏好设置");
    let initial_fonts = [font(title), font(checkbox)];
    assert!(initial_fonts.iter().all(|font| *font != 0));
    let observer = ObservedControls::new(&[content, title, checkbox, preview]);

    for _ in 0..2 {
        apply_and_refresh(&settings, Language::SimplifiedChinese);
        assert_eq!(
            [font(title), font(checkbox)],
            initial_fonts,
            "applying the current language must not rebuild the heading and body fonts"
        );
        assert_eq!(
            observer.counts(),
            [0, 0, 0],
            "unchanged language must not reset fonts/text or reposition content"
        );
    }

    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let combo = unsafe { GetDlgItem(Some(content), 110) }.unwrap();
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(1)), None) };
    apply_and_refresh(&settings, Language::English);
    assert_eq!(window_text(title), "Preferences");
    let english_fonts = [font(title), font(checkbox)];
    for (english, previous) in english_fonts.into_iter().zip(initial_fonts) {
        assert_ne!(
            english, previous,
            "a real language switch updates localized fonts"
        );
    }
    let changed = observer.counts();
    assert!(changed[0] > 0 && changed[1] > 0);
    observer.reset();
    for _ in 0..2 {
        apply_and_refresh(&settings, Language::English);
        assert_eq!([font(title), font(checkbox)], english_fonts);
        assert_eq!(observer.counts(), [0, 0, 0]);
    }
}

fn apply_and_refresh(settings: &SettingsWindow, expected_language: Language) {
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(settings.hwnd(), WM_COMMAND, Some(WPARAM(1)), None) };
    let Some(SettingsAction::Apply(draft)) = settings.process_pending().unwrap() else {
        panic!("Apply must return a validated settings draft");
    };
    assert_eq!(draft.language, expected_language);
    // Follow the host's successful Apply UI path without writing configuration
    // or registering the draft's hotkey. Status updates may repaint the footer.
    i18n::set_language(draft.language);
    settings.refresh_language().unwrap();
    let status = i18n::tr("设置已保存。", "Settings saved.");
    settings.show_status(status, true).unwrap();
    // SAFETY: The parent belongs to this test-owned live window tree; the returned child
    // handle is borrowed only while its owner survives.
    let label = unsafe { GetDlgItem(Some(settings.hwnd()), 14) }.unwrap();
    assert_eq!(window_text(label), status);
}

#[derive(Default)]
struct MessageCounts {
    fonts: Cell<u32>,
    texts: Cell<u32>,
    positions: Cell<u32>,
}

struct ObservedControls {
    windows: Vec<HWND>,
    messages: Box<MessageCounts>,
}

impl ObservedControls {
    fn new(windows: &[HWND]) -> Self {
        let mut observer = Self {
            windows: Vec::with_capacity(windows.len()),
            messages: Box::default(),
        };
        for &hwnd in windows {
            // SAFETY: the stable messages Box lives until all installed subclasses
            // are removed; capacity was reserved before installing.
            assert!(unsafe {
                SetWindowSubclass(
                    hwnd,
                    Some(count_mutations),
                    1,
                    observer.messages.as_ref() as *const MessageCounts as usize,
                )
                .as_bool()
            });
            observer.windows.push(hwnd);
        }
        observer
    }

    fn counts(&self) -> [u32; 3] {
        [
            self.messages.fonts.get(),
            self.messages.texts.get(),
            self.messages.positions.get(),
        ]
    }

    fn reset(&self) {
        self.messages.fonts.set(0);
        self.messages.texts.set(0);
        self.messages.positions.set(0);
    }
}

impl Drop for ObservedControls {
    fn drop(&mut self) {
        for &hwnd in &self.windows {
            // SAFETY: this observer installed this procedure/ID pair; the Box
            // remains alive throughout removal.
            if !unsafe { RemoveWindowSubclass(hwnd, Some(count_mutations), 1) }.as_bool() {
                // Continuing would free a Box that an installed subclass could still reference.
                std::process::abort();
            }
        }
    }
}

/// # Safety
/// Invoked only through our installed subclass with reference pointing to the
/// observer-owned MessageCounts Box, alive until that subclass is removed.
unsafe extern "system" fn count_mutations(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass: usize,
    reference: usize,
) -> LRESULT {
    // SAFETY: SetWindowSubclass installed this stable Box address; the observer has not
    // removed the callback or released its state.
    let counts = unsafe { &*(reference as *const MessageCounts) };
    let count = match message {
        WM_SETFONT => Some(&counts.fonts),
        WM_SETTEXT => Some(&counts.texts),
        WM_WINDOWPOSCHANGED => Some(&counts.positions),
        _ => None,
    };
    if let Some(count) = count {
        count.set(count.get().saturating_add(1));
    }
    // SAFETY: preserve the current native subclass chain using unchanged callback
    // parameters; no payload pointer is retained.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

fn font(hwnd: HWND) -> isize {
    // SAFETY: These messages target live controls in this test-owned tree; control values
    // are scalar and the calls complete before owner teardown.
    unsafe { SendMessageW(hwnd, WM_GETFONT, None, None) }.0
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

struct TestOwner(HWND);

impl Drop for TestOwner {
    fn drop(&mut self) {
        // SAFETY: This thread owns the fixture HWND; synchronous teardown finishes
        // before its surrounding fixture resources are released.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
