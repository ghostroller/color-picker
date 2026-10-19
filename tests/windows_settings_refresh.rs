#![cfg(windows)]

#[path = "support/pixel_fixture.rs"]
mod pixel_fixture;

use std::cell::Cell;

use color_picker::{
    app::{
        appearance::{ThemeMode, set_theme_mode, theme_mode},
        config::Config,
        i18n::{self, Language},
    },
    ui::windows::settings::{SettingsAction, SettingsWindow},
};
use pixel_fixture::ScopedPmv2;
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{Dwm::DwmFlush, Gdi::*},
        UI::{
            Controls::TBM_SETPOS,
            Input::KeyboardAndMouse::SetFocus,
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

#[test]
#[ignore = "requires an interactive Windows desktop; paints only its own settings window"]
fn theme_switch_repaints_visible_and_previously_clipped_slider_backgrounds() {
    let _dpi = ScopedPmv2::enter().unwrap();
    let _theme = ScopedTheme(theme_mode());
    set_theme_mode(ThemeMode::Dark);
    i18n::set_language(Language::SimplifiedChinese);
    let config = Config {
        language: Language::SimplifiedChinese,
        theme: ThemeMode::Dark,
        ..Config::default()
    };
    // This fixture never runs the application host, writes configuration, registers
    // a hotkey, or touches the clipboard. Apply only returns an in-memory draft.
    let settings = SettingsWindow::new(&config, HWND::default(), None, true).unwrap();
    // SAFETY: Resize and raise only this fixture's owned root, keeping it on screen
    // while actual WM_PAINT output is sampled from its control DCs.
    unsafe {
        SetWindowPos(
            settings.hwnd(),
            Some(HWND_TOPMOST),
            20,
            20,
            700,
            700,
            SWP_NOACTIVATE,
        )
        .unwrap();
    }
    settings.process_pending().unwrap();
    let viewport = child(settings.hwnd(), 200);
    let content = child(viewport, 201);
    let sliders = [child(content, 107), child(content, 108)];
    let title = child(content, 15);
    let checkbox = child(content, 101);
    let fonts = [font(title), font(checkbox)];
    // Keep uncommitted values distinct from startup values throughout repainting.
    // SAFETY: These scalar messages target live controls in the fixture's own tree.
    unsafe {
        SendMessageW(sliders[0], TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(5)));
        SendMessageW(sliders[1], TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(43)));
        SendMessageW(child(content, 105), CB_SETCURSEL, Some(WPARAM(1)), None);
    }
    let draft = take_draft(&settings);
    assert_eq!(draft.appearance.border_width_dip, 5);
    assert_eq!(draft.appearance.background_transparency_percent, 43);

    reveal_sliders(&settings, sliders[1]);
    assert_slider_points_visible(viewport, &sliders);
    assert_slider_backgrounds(&sliders, ThemeMode::Dark, "initial dark paint");
    let offset = scroll_position(viewport);
    assert!(offset > 0, "fixture must scroll to expose both sliders");

    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        set_theme_mode(mode);
        settings.refresh_theme().unwrap();
        paint_owned_tree(&settings);
        assert_slider_backgrounds(&sliders, mode, "switch with visible sliders");
        assert_eq!(scroll_position(viewport), offset);
        assert_eq!([font(title), font(checkbox)], fonts);
        assert_eq!(take_draft(&settings), draft);
    }

    scroll(viewport, SB_TOP);
    paint_owned_tree(&settings);
    let viewport_rect = window_rect(viewport);
    for slider in sliders {
        assert!(
            window_rect(slider).top >= viewport_rect.bottom,
            "both sliders must be clipped out of view during the final switch"
        );
    }
    set_theme_mode(ThemeMode::Light);
    settings.refresh_theme().unwrap();
    paint_owned_tree(&settings);
    assert_eq!(scroll_position(viewport), 0);
    assert_eq!([font(title), font(checkbox)], fonts);
    assert_eq!(take_draft(&settings), draft);
    reveal_sliders(&settings, sliders[1]);
    assert_slider_points_visible(viewport, &sliders);
    assert_slider_backgrounds(&sliders, ThemeMode::Light, "first paint after reveal");
    assert_eq!(scroll_position(viewport), offset);
}

fn child(parent: HWND, id: i32) -> HWND {
    // SAFETY: Every parent passed by the fixture belongs to its live settings tree.
    unsafe { GetDlgItem(Some(parent), id) }.unwrap()
}

fn take_draft(settings: &SettingsWindow) -> Config {
    // SAFETY: Apply is a scalar command to the fixture-owned root. No application
    // host consumes the result, so it cannot persist or register anything.
    unsafe { SendMessageW(settings.hwnd(), WM_COMMAND, Some(WPARAM(1)), None) };
    let Some(SettingsAction::Apply(draft)) = settings.process_pending().unwrap() else {
        panic!("Apply must return a validated settings draft");
    };
    draft
}

fn scroll(viewport: HWND, command: SCROLLBAR_COMMAND) {
    // SAFETY: The fixture owns this viewport; scrolling carries only a scalar command.
    unsafe { SendMessageW(viewport, WM_VSCROLL, Some(WPARAM(command.0 as usize)), None) };
}

fn reveal_sliders(settings: &SettingsWindow, last_slider: HWND) {
    // SAFETY: Move focus only inside the test-owned tree. Its normal focus-scroll
    // path reveals the lower slider without scrolling the first past the viewport.
    unsafe {
        let _ = SetFocus(Some(settings.hwnd()));
        let _ = SetFocus(Some(last_slider));
    }
    paint_owned_tree(settings);
}

fn assert_slider_points_visible(viewport: HWND, sliders: &[HWND; 2]) {
    let viewport = window_rect(viewport);
    for &slider in sliders {
        let slider = window_rect(slider);
        assert!(
            slider.top >= viewport.top && slider.bottom <= viewport.bottom,
            "fixture must expose the entire slider: slider {slider:?}, viewport {viewport:?}"
        );
    }
}

fn scroll_position(viewport: HWND) -> i32 {
    let mut info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS,
        ..Default::default()
    };
    // SAFETY: The live viewport writes only the advertised local SCROLLINFO slot.
    unsafe { GetScrollInfo(viewport, SB_VERT, &mut info) }.unwrap();
    info.nPos
}

fn window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: The fixture keeps this window alive while its bounds are queried.
    unsafe { GetWindowRect(hwnd, &mut rect) }.unwrap();
    rect
}

fn paint_owned_tree(settings: &SettingsWindow) {
    let mut message = MSG::default();
    // SAFETY: Pump only this root's messages and descendants. Every dispatched native
    // payload remains valid through its synchronous dispatch on the owning UI thread.
    unsafe {
        while PeekMessageW(&mut message, Some(settings.hwnd()), 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    settings.process_pending().unwrap();
    // SAFETY: Force actual WM_PAINT for this owned tree, rather than WM_PRINTCLIENT,
    // which can bypass the native trackbar's stale screen-paint cache.
    unsafe {
        assert!(
            RedrawWindow(
                Some(settings.hwnd()),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            )
            .as_bool()
        );
        DwmFlush().unwrap();
    }
}

fn assert_slider_backgrounds(sliders: &[HWND; 2], mode: ThemeMode, phase: &str) {
    let expected = match mode {
        ThemeMode::Light => COLORREF(0xffffff),
        ThemeMode::Dark => COLORREF(0x332017),
    };
    let mut samples = Vec::with_capacity(sliders.len());
    for (index, &slider) in sliders.iter().enumerate() {
        let mut rect = RECT::default();
        // SAFETY: Each live trackbar remains owned by the fixture during this query.
        unsafe { GetClientRect(slider, &mut rect) }.unwrap();
        // Sample lower corners, clear of the centered channel and movable thumb.
        let points = [(3, rect.bottom - 3), (rect.right - 4, rect.bottom - 3)];
        // SAFETY: GetDC borrows only this owned live control. ReleaseDC pairs it
        // before any assertion can unwind the window fixture.
        let colors = unsafe {
            let dc = GetDC(Some(slider));
            assert!(!dc.0.is_null(), "owned slider DC must be available");
            let colors = points.map(|(x, y)| GetPixel(dc, x, y));
            assert_eq!(ReleaseDC(Some(slider), dc), 1);
            colors
        };
        samples.push((index, rect.right, rect.bottom, points, colors));
    }
    assert!(
        samples.iter().all(|sample| sample.4 == [expected; 2]),
        "{phase}: expected {mode:?} background {expected:?}; slider samples \
         (index, width, height, client points, colors): {samples:?}"
    );
}

struct ScopedTheme(ThemeMode);

impl Drop for ScopedTheme {
    fn drop(&mut self) {
        set_theme_mode(self.0);
    }
}

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
    let viewport = unsafe { GetDlgItem(Some(settings.hwnd()), 200) }.unwrap();
    // SAFETY: The fixture retains the viewport and its owned scrolling document
    // while these child handles are borrowed.
    let content = unsafe { GetDlgItem(Some(viewport), 201) }.unwrap();
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
