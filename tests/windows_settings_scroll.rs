#![cfg(windows)]

//! Ignored desktop regression and continuous screen recorder for settings.
//! The automatic phase sends test-local wheel messages, not physical input.
//! For an external native scrollbar drag, set COLOR_PICKER_SCROLL_RECORD_MS
//! and COLOR_PICKER_SCROLL_CAPTURE_DIR, then drag after RECORD_READY appears.
//! COLOR_PICKER_SCROLL_ARM_MS allows external activation before the reference.
//! Manual recordings can explicitly exclude a pointer-feedback strip with
//! COLOR_PICKER_SCROLL_POINTER_MARGIN_PX; the default automatic test is full width.
//! A separate thread captures the actual screen throughout both phases. It
//! never calls PrintWindow, UpdateWindow or RedrawWindow while recording.

#[path = "support/pixel_fixture.rs"]
mod pixel_fixture;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use color_picker::{
    app::{
        appearance::{ThemeMode, set_theme_mode},
        config::Config,
        i18n::{self, Language},
    },
    ui::windows::settings::SettingsWindow,
};
use pixel_fixture::ScopedPmv2;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{Dwm::DwmFlush, Gdi::*},
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{GetCapture, GetFocus, SetFocus},
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

const CONTROL_IDS: [i32; 30] = [
    15, 16, 28, 110, 30, 111, 10, 101, 102, 103, 11, 104, 12, 17, 13, 105, 109, 106, 20, 21, 107,
    22, 23, 108, 24, 25, 26, 27, 18, 19,
];

#[test]
#[ignore = "requires a stable interactive desktop; continuously samples only its own visible settings viewport"]
fn settings_continuous_scroll_uses_one_document_and_complete_screen_frames() {
    let _dpi = ScopedPmv2::enter().unwrap();
    i18n::set_language(Language::SimplifiedChinese);
    // SAFETY: This test owns the fixture window; the terminated class/title
    // strings stay live during synchronous creation.
    let owner = Owner(unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Settings scroll recorder owner"),
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
    let mut config = Config::default();
    if std::env::var("COLOR_PICKER_SCROLL_THEME").as_deref() == Ok("dark") {
        config.theme = ThemeMode::Dark;
    }
    set_theme_mode(config.theme);
    let settings = SettingsWindow::new(&config, owner.0, None, true).unwrap();
    settings.process_pending().unwrap();
    let root = settings.hwnd();
    let viewport = child(root, 200);
    let document = child(viewport, 201);
    place_visible_fixture(root);
    // SAFETY: Focus only a footer control in the owned fixture; verify the
    // resulting focus rather than interpreting a null previous HWND as failure.
    unsafe {
        let _ = SetFocus(Some(child(root, 1)));
        assert_eq!(GetFocus(), child(root, 1));
    }
    pump_for(&settings, Duration::from_millis(80));
    let range = scroll_info(viewport);
    assert!(
        range.nMax + 1 > range.nPage as i32 + 100,
        "the shortened fixture must have overflowing content to exercise scrolling"
    );
    let full_viewport_rect = client_on_screen(viewport);
    assert_owned_visible(root, full_viewport_rect);
    let record_ms = std::env::var("COLOR_PICKER_SCROLL_RECORD_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(60_000);
    let pointer_margin = std::env::var("COLOR_PICKER_SCROLL_POINTER_MARGIN_PX")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    assert!(
        pointer_margin >= 0 && pointer_margin < full_viewport_rect.right - full_viewport_rect.left
    );
    assert!(
        pointer_margin == 0 || record_ms != 0,
        "pointer-feedback exclusion is only available for explicit external recordings"
    );
    let viewport_rect = RECT {
        right: full_viewport_rect.right - pointer_margin,
        ..full_viewport_rect
    };
    let local_before: Vec<_> = CONTROL_IDS
        .iter()
        .map(|&id| (id, local_rect(document, child(document, id))))
        .collect();
    let observer = ChildObserver::new(document);
    let native_scroll = NativeScrollObserver::new(viewport);

    let before = window_rect(document);
    scroll_command(viewport, SB_LINEDOWN);
    let after = window_rect(document);
    assert!(
        after.top < before.top,
        "native scrolling must move the document before process_pending"
    );
    assert_eq!(
        observer.counts(),
        [0, 0],
        "pure scrolling must not arrange or reset fonts on child controls"
    );
    settings.process_pending().unwrap();
    assert_eq!(
        observer.counts(),
        [0, 0],
        "document movement must not queue a full child layout"
    );
    assert_local_positions(document, &local_before);

    let arm_ms = std::env::var("COLOR_PICKER_SCROLL_ARM_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(60_000);
    if arm_ms != 0 {
        eprintln!(
            "ARM_READY hwnd={} arm_ms={arm_ms}; activate this fixture and leave pointer feedback outside the sampled area before reference capture",
            root.0 as usize
        );
        pump_for(&settings, Duration::from_millis(arm_ms));
    }
    let atlas = Arc::new(build_clean_atlas(&settings, viewport, viewport_rect));
    observer.reset();
    scroll_command(viewport, SB_TOP);
    pump_for(&settings, Duration::from_millis(60));
    // SAFETY: Reference preparation ends before the independent recording
    // thread starts; flush only this completed reference frame.
    unsafe { DwmFlush().unwrap() };

    let output_root = std::env::var_os("COLOR_PICKER_SCROLL_CAPTURE_DIR").map(PathBuf::from);
    let save_frames = output_root.is_some() || record_ms != 0;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_capture = Arc::clone(&stop);
    let capture_atlas = Arc::clone(&atlas);
    let capture_tracking = Arc::clone(&native_scroll.events.tracking);
    let viewport_value = viewport.0 as usize;
    let capture = thread::spawn(move || {
        record_screen(
            viewport_value,
            viewport_rect,
            capture_atlas,
            stop_capture,
            capture_tracking,
            config.theme,
            save_frames,
        )
    });

    // This is a continuously driven message sequence. Sampling runs on another
    // thread, including during SetWindowPos/painting; no clean-frame redraw or
    // DwmFlush is inserted between inputs and samples here.
    for _ in 0..8 {
        for delta in [-120_i16; 8].into_iter().chain([120_i16; 8]) {
            // SAFETY: Send a scalar wheel message only to the owned viewport.
            // This deliberately does not synthesize global mouse input.
            unsafe {
                SendMessageW(
                    viewport,
                    WM_MOUSEWHEEL,
                    Some(WPARAM(usize::from(delta as u16) << 16)),
                    None,
                );
            }
            pump_for(&settings, Duration::from_millis(24));
        }
    }
    exercise_captured_thumb(&settings, viewport, document);
    exercise_held_scroll(viewport);
    if record_ms != 0 {
        eprintln!(
            "RECORD_READY hwnd={} sampled_rect={viewport_rect:?} excluded_right_strip_px={pointer_margin} duration_ms={record_ms}; drag the native scrollbar now",
            root.0 as usize
        );
        // Additional manual input uses the same production capture handlers as
        // the scripted own-window thumb messages exercised above.
        pump_for(&settings, Duration::from_millis(record_ms));
    }
    stop.store(true, Ordering::Release);
    let mut report = capture.join().expect("independent screen capture thread");
    report.thumb_messages = native_scroll.events.thumb_messages.get();
    report.excluded_right_strip_px = pointer_margin;
    report.thumb_positions = native_scroll
        .events
        .thumb_positions
        .borrow()
        .iter()
        .copied()
        .collect();
    assert_local_positions(document, &local_before);
    assert_eq!(
        observer.counts(),
        [0, 0],
        "continuous scroll/drag must leave child HWND layout and fonts untouched"
    );

    if save_frames || report.invalid_frames != 0 || report.scrollbar_bad_frames != 0 {
        let root = output_root.unwrap_or_else(|| PathBuf::from("target/settings-scroll-captures"));
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let output = root.join(format!("run-{stamp}"));
        write_recording(&output, &report, atlas.width, atlas.viewport_height);
        eprintln!("SCROLL_RECORDING {}", output.display());
    }
    eprintln!(
        "SCROLL_SEQUENCE frames={} changing_position_frames={} distinct_positions={} changing_matched_frames={} distinct_matched_offsets={} invalid_frames={} native_thumb_messages={} native_thumb_positions={} tracking_frames={} tracking_changing_frames={} tracking_distinct_matched_offsets={} excluded_right_strip_px={pointer_margin}",
        report.frames.len(),
        report.changing_frames,
        report.positions.len(),
        report.changing_matched_frames,
        report.matched_offsets.len(),
        report.invalid_frames,
        report.thumb_messages,
        report.thumb_positions.len(),
        report.tracking_frames,
        report.tracking_changing_frames,
        report.tracking_matched_offsets.len()
    );
    eprintln!(
        "SCROLLBAR_STYLE invalid_track_frames={}",
        report.scrollbar_bad_frames
    );
    assert_eq!(
        report.scrollbar_bad_frames, 0,
        "scrolling and captured dragging must never expose the stock scrollbar track"
    );
    assert!(
        report.frames.len() >= 12,
        "recording must contain an actual screen-frame sequence"
    );
    assert!(
        report.positions.len() >= 5,
        "recording must cover multiple scrolling positions"
    );
    assert!(
        report.changing_frames >= 5,
        "capture must overlap continuing scroll motion"
    );
    assert!(
        report.matched_offsets.len() >= 5,
        "actual screen pixels must show multiple document offsets; changing scrollbar positions alone do not prove visible scrolling"
    );
    assert!(
        report.changing_matched_frames >= 5,
        "actual screen pixels must keep changing during the sequence; a frozen clean document slice must fail even when scrollbar positions change"
    );
    assert_eq!(
        report.invalid_frames, 0,
        "screen frames must match complete document slices, including while scroll/drag is active"
    );
    {
        assert!(
            report.thumb_messages >= 3 && report.thumb_positions.len() >= 3,
            "recording must include captured scrollbar dragging with continuing position updates"
        );
        assert!(
            report.tracking_matched_offsets.len() >= 3 && report.tracking_changing_frames >= 2,
            "actual screen pixels must show at least three document offsets and two visual changes while the scrollbar owns mouse capture"
        );
    }
}

fn exercise_held_scroll(viewport: HWND) {
    const REPEAT_TIMER: usize = 0x5343; // Production scrollbar's local input timer.
    let pack =
        |x: i32, y: i32| LPARAM(((u32::from(y as u16) << 16) | u32::from(x as u16)) as isize);
    let mut bar = SCROLLBARINFO {
        cbSize: std::mem::size_of::<SCROLLBARINFO>() as u32,
        ..Default::default()
    };
    scroll_command(viewport, SB_TOP);
    // SAFETY: all messages and queries target this owned viewport only. Timer
    // messages directly exercise production repeat logic without waiting or
    // injecting global mouse input.
    unsafe {
        GetScrollBarInfo(viewport, OBJID_VSCROLL, &mut bar).unwrap();
        let x = (bar.rcScrollBar.left + bar.rcScrollBar.right) / 2;
        let y = bar.rcScrollBar.bottom - 2;
        let origin = client_on_screen(viewport);
        SendMessageW(
            viewport,
            WM_NCLBUTTONDOWN,
            Some(WPARAM(HTVSCROLL as usize)),
            Some(pack(x, y)),
        );
        let first = scroll_info(viewport).nPos;
        assert!(first > 0);
        SendMessageW(viewport, WM_TIMER, Some(WPARAM(REPEAT_TIMER)), None);
        let repeated = scroll_info(viewport).nPos;
        assert!(repeated > first, "held arrow must keep scrolling");
        SendMessageW(
            viewport,
            WM_MOUSEMOVE,
            Some(WPARAM(1)),
            Some(pack(-20, y - origin.top)),
        );
        SendMessageW(viewport, WM_TIMER, Some(WPARAM(REPEAT_TIMER)), None);
        assert_eq!(
            scroll_info(viewport).nPos,
            repeated,
            "leaving the pressed region pauses repeat"
        );
        SendMessageW(
            viewport,
            WM_MOUSEMOVE,
            Some(WPARAM(1)),
            Some(pack(x - origin.left, y - origin.top)),
        );
        SendMessageW(viewport, WM_TIMER, Some(WPARAM(REPEAT_TIMER)), None);
        let resumed = scroll_info(viewport).nPos;
        assert!(
            resumed > repeated,
            "returning to the pressed region resumes repeat"
        );
        SendMessageW(
            viewport,
            WM_LBUTTONUP,
            None,
            Some(pack(x - origin.left, y - origin.top)),
        );
        assert_ne!(GetCapture(), viewport);
        SendMessageW(viewport, WM_TIMER, Some(WPARAM(REPEAT_TIMER)), None);
        assert_eq!(
            scroll_info(viewport).nPos,
            resumed,
            "released buttons cannot repeat"
        );

        scroll_command(viewport, SB_TOP);
        GetScrollBarInfo(viewport, OBJID_VSCROLL, &mut bar).unwrap();
        let page_y = (bar.rcScrollBar.top + bar.xyThumbBottom + bar.rcScrollBar.bottom
            - bar.dxyLineButton)
            / 2;
        SendMessageW(
            viewport,
            WM_NCLBUTTONDOWN,
            Some(WPARAM(HTVSCROLL as usize)),
            Some(pack(x, page_y)),
        );
        for _ in 0..20 {
            SendMessageW(viewport, WM_TIMER, Some(WPARAM(REPEAT_TIMER)), None);
        }
        let stopped = scroll_info(viewport).nPos;
        assert!(stopped > 0);
        SendMessageW(viewport, WM_TIMER, Some(WPARAM(REPEAT_TIMER)), None);
        assert_eq!(
            scroll_info(viewport).nPos,
            stopped,
            "page repeat stops when the thumb reaches the pointer"
        );
        SendMessageW(viewport, WM_CANCELMODE, None, None);
        assert_ne!(GetCapture(), viewport);
        SendMessageW(viewport, WM_TIMER, Some(WPARAM(REPEAT_TIMER)), None);
        assert_eq!(
            scroll_info(viewport).nPos,
            stopped,
            "cancel stops repeat and releases capture"
        );
    }
}

fn exercise_captured_thumb(settings: &SettingsWindow, viewport: HWND, document: HWND) {
    scroll_command(viewport, SB_TOP);
    pump_for(settings, Duration::from_millis(40));
    let mut bar = SCROLLBARINFO {
        cbSize: std::mem::size_of::<SCROLLBARINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: query the fixture's native nonclient geometry into initialized output.
    unsafe { GetScrollBarInfo(viewport, OBJID_VSCROLL, &mut bar).unwrap() };
    let x = (bar.rcScrollBar.left + bar.rcScrollBar.right) / 2;
    let start_y = bar.rcScrollBar.top + (bar.xyThumbTop + bar.xyThumbBottom) / 2;
    let pack =
        |x: i32, y: i32| LPARAM(((u32::from(y as u16) << 16) | u32::from(x as u16)) as isize);
    // SAFETY: scalar mouse messages are sent only to this owned fixture. No
    // global pointer input is injected and no USER32 modal loop is entered.
    unsafe {
        SendMessageW(
            viewport,
            WM_NCLBUTTONDOWN,
            Some(WPARAM(HTVSCROLL as usize)),
            Some(pack(x, start_y)),
        );
        assert_eq!(GetCapture(), viewport, "thumb press must own mouse capture");
    }
    let origin = client_on_screen(viewport);
    let travel = bar.rcScrollBar.bottom
        - bar.rcScrollBar.top
        - 2 * bar.dxyLineButton
        - (bar.xyThumbBottom - bar.xyThumbTop);
    let mut previous_position = 0;
    for step in 1..=8 {
        let y = start_y + travel * step / 8;
        let before = window_rect(document);
        // SAFETY: captured mouse messages use viewport-client coordinates and
        // remain scoped to this test-owned HWND.
        unsafe {
            SendMessageW(
                viewport,
                WM_MOUSEMOVE,
                Some(WPARAM(1)),
                Some(pack(x - origin.left, y - origin.top)),
            );
        }
        let position = scroll_info(viewport).nPos;
        assert!(position > previous_position);
        assert!(
            window_rect(document).top < before.top,
            "drag must move content synchronously before process_pending"
        );
        previous_position = position;
        pump_for(settings, Duration::from_millis(32));
    }
    let info = scroll_info(viewport);
    assert_eq!(info.nPos, info.nMax - info.nPage as i32 + 1);
    // SAFETY: test-local captured moves beyond both track ends verify clamping;
    // release must finish capture using the same production message path.
    unsafe {
        SendMessageW(
            viewport,
            WM_MOUSEMOVE,
            Some(WPARAM(1)),
            Some(pack(0, 30_000)),
        );
        assert_eq!(scroll_info(viewport).nPos, info.nPos);
        SendMessageW(
            viewport,
            WM_MOUSEMOVE,
            Some(WPARAM(1)),
            Some(pack(0, -30_000)),
        );
        assert_eq!(scroll_info(viewport).nPos, 0);
        SendMessageW(viewport, WM_LBUTTONUP, None, Some(pack(0, -30_000)));
        assert_ne!(GetCapture(), viewport);
    }
    pump_for(settings, Duration::from_millis(40));
}

fn pump_for(settings: &SettingsWindow, duration: Duration) {
    let deadline = Instant::now() + duration;
    loop {
        settings.process_pending().unwrap();
        let mut message = MSG::default();
        // SAFETY: Pump only this fixture thread's queue using stack MSG storage;
        // no Rust callback-state borrow spans native dispatch.
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn place_visible_fixture(root: HWND) {
    let bounds = window_rect(root);
    let client = client_on_screen(root);
    // SAFETY: Query the live fixture DPI and the monitor containing its current rectangle.
    let dpi = unsafe { GetDpiForWindow(root) };
    let dip = |value: i32| ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32;
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let mut cursor = POINT::default();
    // SAFETY: Native queries fill initialized local outputs and retain no pointers.
    unsafe {
        assert!(
            GetMonitorInfoW(
                MonitorFromRect(&bounds, MONITOR_DEFAULTTONEAREST),
                &mut info
            )
            .as_bool()
        );
        GetCursorPos(&mut cursor).unwrap();
    }
    let work = info.rcWork;
    let width = bounds.right - bounds.left;
    let chrome = bounds.bottom - bounds.top - (client.bottom - client.top);
    let height = (dip(320) + chrome).min(work.bottom - work.top);
    let (x, y) = [
        (work.left, work.top),
        (work.right - width, work.top),
        (work.left, work.bottom - height),
        (work.right - width, work.bottom - height),
    ]
    .into_iter()
    .find(|&(x, y)| cursor.x < x || cursor.x >= x + width || cursor.y < y || cursor.y >= y + height)
    .expect("the work area must have room for an unobstructed fixture away from the cursor");
    // SAFETY: Reposition/activate only this test-owned HWND, without changing the
    // display work area or moving the user's pointer.
    unsafe {
        SetWindowPos(
            root,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_SHOWWINDOW,
        )
        .unwrap();
        let _ = SetForegroundWindow(root);
    }
    let actual = window_rect(root);
    assert!(
        actual.left >= work.left
            && actual.top >= work.top
            && actual.right <= work.right
            && actual.bottom <= work.bottom
    );
}

struct Atlas {
    width: usize,
    viewport_height: usize,
    pixels: Vec<u8>,
    signatures: HashMap<u64, Vec<usize>>,
}

impl Atlas {
    fn matching_offset(&self, frame: &[u8]) -> Option<usize> {
        let signature = frame_signature(frame, self.width);
        self.signatures
            .get(&signature)?
            .iter()
            .copied()
            .find(|&offset| {
                let start = offset * self.width * 4;
                self.pixels[start..start + frame.len()] == *frame
            })
    }
}

fn build_clean_atlas(settings: &SettingsWindow, viewport: HWND, rect: RECT) -> Atlas {
    let width = (rect.right - rect.left) as usize;
    let viewport_height = (rect.bottom - rect.top) as usize;
    let height = (scroll_info(viewport).nMax + 1) as usize;
    let mut pixels = vec![0; width * height * 4];
    let mut filled = vec![false; height];
    scroll_command(viewport, SB_TOP);
    loop {
        pump_for(settings, Duration::from_millis(1));
        // SAFETY: Explicit full reference repaint is confined to preparation;
        // this operation is never performed during continuous recording.
        unsafe {
            assert!(
                RedrawWindow(
                    Some(viewport),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW
                )
                .as_bool()
            );
            DwmFlush().unwrap();
        }
        let frame = capture_viewport(rect);
        let info = scroll_info(viewport);
        let offset = info.nPos as usize;
        for row in 0..viewport_height {
            let target_row = offset + row;
            let target = target_row * width * 4;
            let source = row * width * 4;
            if filled[target_row] {
                assert_eq!(
                    pixels[target..target + width * 4],
                    frame[source..source + width * 4],
                    "clean reference slices must share stable document coordinates"
                );
            } else {
                pixels[target..target + width * 4]
                    .copy_from_slice(&frame[source..source + width * 4]);
                filled[target_row] = true;
            }
        }
        if info.nPos >= (info.nMax - info.nPage as i32 + 1).max(0) {
            break;
        }
        scroll_command(viewport, SB_LINEDOWN);
        assert!(scroll_info(viewport).nPos > info.nPos);
    }
    assert!(filled.iter().all(|&row| row));
    let rows: Vec<_> = pixels.chunks_exact(width * 4).map(row_signature).collect();
    let mut signatures = HashMap::<u64, Vec<usize>>::new();
    let mut signature = 0_u64;
    let mut factor = 1_u64;
    for &row in &rows[..viewport_height] {
        signature = signature.wrapping_mul(31).wrapping_add(row);
    }
    for _ in 1..viewport_height {
        factor = factor.wrapping_mul(31);
    }
    signatures.entry(signature).or_default().push(0);
    for offset in 1..=height - viewport_height {
        signature = signature
            .wrapping_sub(rows[offset - 1].wrapping_mul(factor))
            .wrapping_mul(31)
            .wrapping_add(rows[offset + viewport_height - 1]);
        signatures.entry(signature).or_default().push(offset);
    }
    Atlas {
        width,
        viewport_height,
        pixels,
        signatures,
    }
}

fn row_signature(row: &[u8]) -> u64 {
    row.iter().fold(0xcbf29ce484222325_u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn frame_signature(frame: &[u8], width: usize) -> u64 {
    frame.chunks_exact(width * 4).fold(0_u64, |hash, row| {
        hash.wrapping_mul(31).wrapping_add(row_signature(row))
    })
}

struct RecordedFrame {
    elapsed_us: u128,
    position_before: i32,
    position_after: i32,
    matched_offset: Option<usize>,
    during_native_tracking: bool,
    pixels: Option<Vec<u8>>,
}

struct Recording {
    frames: Vec<RecordedFrame>,
    positions: HashSet<i32>,
    changing_frames: usize,
    matched_offsets: HashSet<usize>,
    changing_matched_frames: usize,
    tracking_frames: usize,
    tracking_matched_offsets: HashSet<usize>,
    tracking_changing_frames: usize,
    invalid_frames: usize,
    thumb_messages: u32,
    thumb_positions: Vec<i32>,
    excluded_right_strip_px: i32,
    scrollbar_bad_frames: usize,
    scrollbar_first_bad: Option<Vec<u8>>,
    scrollbar_rect: RECT,
}

fn record_screen(
    viewport_value: usize,
    rect: RECT,
    atlas: Arc<Atlas>,
    stop: Arc<AtomicBool>,
    native_tracking: Arc<AtomicBool>,
    theme: ThemeMode,
    save_frames: bool,
) -> Recording {
    let _dpi = ScopedPmv2::enter().unwrap();
    let viewport = HWND(viewport_value as *mut _);
    let started = Instant::now();
    let mut bar = SCROLLBARINFO {
        cbSize: std::mem::size_of::<SCROLLBARINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: the recording fixture retains this viewport across the read-only
    // native geometry query; no drawing message is sent from this thread.
    unsafe { GetScrollBarInfo(viewport, OBJID_VSCROLL, &mut bar).unwrap() };
    let track_color = match theme {
        ThemeMode::Light => [0xfa, 0xf7, 0xf5],
        ThemeMode::Dark => [0x27, 0x18, 0x11],
    };
    let mut report = Recording {
        frames: vec![],
        positions: HashSet::new(),
        changing_frames: 0,
        matched_offsets: HashSet::new(),
        changing_matched_frames: 0,
        tracking_frames: 0,
        tracking_matched_offsets: HashSet::new(),
        tracking_changing_frames: 0,
        invalid_frames: 0,
        thumb_messages: 0,
        thumb_positions: vec![],
        excluded_right_strip_px: 0,
        scrollbar_bad_frames: 0,
        scrollbar_first_bad: None,
        scrollbar_rect: bar.rcScrollBar,
    };
    let mut previous = None;
    let mut previous_matched = None;
    let mut previous_tracking_match = None;
    let mut last_saved = Duration::ZERO;
    let mut saved = 0;
    while !stop.load(Ordering::Acquire) {
        let tracking_before = native_tracking.load(Ordering::Acquire);
        let before = scroll_info(viewport).nPos;
        let pixels = capture_viewport(rect);
        let scrollbar_pixels = capture_viewport(bar.rcScrollBar);
        let bar_width = (bar.rcScrollBar.right - bar.rcScrollBar.left) as usize;
        if scrollbar_pixels
            .chunks_exact(bar_width * 4)
            .any(|row| row[4..7] != track_color)
        {
            report.scrollbar_bad_frames += 1;
            if report.scrollbar_first_bad.is_none() {
                report.scrollbar_first_bad = Some(scrollbar_pixels);
            }
        }
        let after = scroll_info(viewport).nPos;
        // Count only samples bracketed by active scrollbar capture. Production
        // commits its document movement synchronously on each captured move.
        let during_native_tracking = tracking_before && native_tracking.load(Ordering::Acquire);
        if during_native_tracking {
            report.tracking_frames += 1;
        } else {
            // Changes between separate drags must not stand in for progress
            // within a drag whose screen presentation is actually frozen.
            previous_tracking_match = None;
        }
        let matched_offset = atlas.matching_offset(&pixels);
        let elapsed = started.elapsed();
        if previous.is_some_and(|position| position != after) || before != after {
            report.changing_frames += 1;
        }
        previous = Some(after);
        report.positions.insert(after);
        // This independent visual progress check cannot be satisfied by a
        // changing scrollbar while the screen remains frozen at one clean slice.
        if let Some(offset) = matched_offset {
            if previous_matched.is_some_and(|previous| previous != offset) {
                report.changing_matched_frames += 1;
            }
            previous_matched = Some(offset);
            report.matched_offsets.insert(offset);
            if during_native_tracking {
                if previous_tracking_match.is_some_and(|previous| previous != offset) {
                    report.tracking_changing_frames += 1;
                }
                previous_tracking_match = Some(offset);
                report.tracking_matched_offsets.insert(offset);
            }
        }
        if matched_offset.is_none() {
            report.invalid_frames += 1;
        }
        let retain = (save_frames
            && elapsed.saturating_sub(last_saved) >= Duration::from_millis(100)
            && saved < 300)
            || (matched_offset.is_none() && report.invalid_frames <= 12);
        if retain {
            last_saved = elapsed;
            saved += 1;
        }
        report.frames.push(RecordedFrame {
            elapsed_us: elapsed.as_micros(),
            position_before: before,
            position_after: after,
            matched_offset,
            during_native_tracking,
            pixels: retain.then_some(pixels),
        });
        thread::sleep(Duration::from_millis(8));
    }
    report
}

fn capture_viewport(viewport: RECT) -> Vec<u8> {
    let width = (viewport.right - viewport.left) as usize;
    let height = (viewport.bottom - viewport.top) as usize;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut surface = ScreenSurface::default();
    let mut bits = std::ptr::null_mut();
    // SAFETY: All DCs/bitmap are owned by the local guard, the initialized DIB
    // descriptor covers exactly width*height BGRX pixels, and one BitBlt copies
    // the sampled rectangle in one full-size screen-sampling operation. No
    // window drawing message is sent and no screen/DC lease crosses a frame.
    let mut pixels = unsafe {
        surface.screen = GetDC(None);
        assert!(!surface.screen.is_invalid());
        surface.memory = CreateCompatibleDC(Some(surface.screen));
        assert!(!surface.memory.is_invalid());
        surface.bitmap = CreateDIBSection(
            Some(surface.screen),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
        .unwrap();
        surface.previous = SelectObject(surface.memory, surface.bitmap.into());
        assert!(!surface.previous.is_invalid() && surface.previous.0 as isize != -1);
        BitBlt(
            surface.memory,
            0,
            0,
            width as i32,
            height as i32,
            Some(surface.screen),
            viewport.left,
            viewport.top,
            SRCCOPY,
        )
        .unwrap();
        assert!(GdiFlush().as_bool());
        std::slice::from_raw_parts(bits.cast::<u8>(), width * height * 4).to_vec()
    };
    drop(surface);
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 0;
    }
    pixels
}

#[derive(Default)]
struct ScreenSurface {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Drop for ScreenSurface {
    fn drop(&mut self) {
        // SAFETY: These are exactly the resources created by capture_viewport;
        // restore the prior selection before releasing the bitmap and DCs.
        unsafe {
            if !self.previous.is_invalid() && self.previous.0 as isize != -1 {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.memory.is_invalid() {
                let _ = DeleteDC(self.memory);
            }
            if !self.screen.is_invalid() {
                ReleaseDC(None, self.screen);
            }
        }
    }
}

fn write_recording(output: &Path, report: &Recording, width: usize, height: usize) {
    fs::create_dir_all(output).unwrap();
    if let Some(pixels) = &report.scrollbar_first_bad {
        write_bmp(
            &output.join("scrollbar-first-stock-frame.bmp"),
            (report.scrollbar_rect.right - report.scrollbar_rect.left) as usize,
            (report.scrollbar_rect.bottom - report.scrollbar_rect.top) as usize,
            pixels,
        );
    }
    let mut matched_offsets: Vec<_> = report.matched_offsets.iter().copied().collect();
    matched_offsets.sort_unstable();
    let mut tracking_matched_offsets: Vec<_> =
        report.tracking_matched_offsets.iter().copied().collect();
    tracking_matched_offsets.sort_unstable();
    let mut csv = String::from(
        "frame,elapsed_us,position_before,position_after,matching_document_offset,saved_image,during_native_tracking\n",
    );
    let mut html = format!(
        "<!doctype html><meta charset=utf-8><title>Continuous settings scroll capture</title><style>body{{font:14px system-ui;background:#eee}}main{{display:grid;grid-template-columns:repeat(4,1fr);gap:12px}}figure{{margin:0;background:white;padding:8px}}img{{width:100%}}</style><h1>Continuous settings scroll screen sequence</h1><p>Actual GetDC/BitBlt samples from an independent thread. Automatic inputs are test-local wheel messages; RECORD_READY permits external native scrollbar dragging. Saved images are spaced about 100 ms apart; CSV lists every sampled frame. Pixel checks do not substitute for visual review.</p><p>Sampled width: {width} px; explicitly excluded pointer-feedback strip on the right: {} px. Every pixel in the remaining sampled rectangle is compared strictly; a cropped recording does not verify the full viewport width.</p><main>",
        report.excluded_right_strip_px,
    );
    for (index, frame) in report.frames.iter().enumerate() {
        let image = if let Some(pixels) = &frame.pixels {
            let name = format!("frame-{index:05}.bmp");
            write_bmp(&output.join(&name), width, height, pixels);
            html.push_str(&format!("<figure><a href='{name}'><img src='{name}'></a><figcaption>{:.1} ms · pos {}→{} · clean offset {:?} · native tracking {}</figcaption></figure>", frame.elapsed_us as f64/1000.0, frame.position_before, frame.position_after, frame.matched_offset, frame.during_native_tracking));
            name
        } else {
            String::new()
        };
        csv.push_str(&format!(
            "{index},{},{},{},{},{},{}\n",
            frame.elapsed_us,
            frame.position_before,
            frame.position_after,
            frame
                .matched_offset
                .map(|value| value.to_string())
                .unwrap_or_default(),
            image,
            frame.during_native_tracking
        ));
    }
    html.push_str("</main>");
    fs::write(output.join("frames.csv"), csv).unwrap();
    fs::write(output.join("contact-sheet.html"), html).unwrap();
    fs::write(output.join("summary.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "sampled_frames": report.frames.len(), "changing_position_frames": report.changing_frames,
        "distinct_positions": report.positions.len(), "invalid_frames": report.invalid_frames,
        "changing_matched_frames": report.changing_matched_frames,
        "distinct_matched_offsets": report.matched_offsets.len(),
        "matched_offsets": matched_offsets,
        "native_tracking_sampled_frames": report.tracking_frames,
        "native_tracking_changing_frames": report.tracking_changing_frames,
        "native_tracking_distinct_matched_offsets": report.tracking_matched_offsets.len(),
        "native_tracking_matched_offsets": tracking_matched_offsets,
        "width": width, "height": height,
        "source_viewport_width": width + report.excluded_right_strip_px as usize,
        "excluded_right_strip_px": report.excluded_right_strip_px,
        "full_viewport_width_verified": report.excluded_right_strip_px == 0,
        "invalid_scrollbar_track_frames": report.scrollbar_bad_frames,
            "native_thumb_messages": report.thumb_messages,
            "native_thumb_positions": report.thumb_positions,
    })).unwrap()).unwrap();
}

fn write_bmp(path: &Path, width: usize, height: usize, pixels: &[u8]) {
    let mut file = fs::File::create(path).unwrap();
    file.write_all(b"BM").unwrap();
    file.write_all(&((54 + pixels.len()) as u32).to_le_bytes())
        .unwrap();
    file.write_all(&[0; 4]).unwrap();
    file.write_all(&54_u32.to_le_bytes()).unwrap();
    file.write_all(&40_u32.to_le_bytes()).unwrap();
    file.write_all(&(width as i32).to_le_bytes()).unwrap();
    file.write_all(&(-(height as i32)).to_le_bytes()).unwrap();
    file.write_all(&1_u16.to_le_bytes()).unwrap();
    file.write_all(&32_u16.to_le_bytes()).unwrap();
    file.write_all(&[0; 24]).unwrap();
    file.write_all(pixels).unwrap();
}

fn child(parent: HWND, id: i32) -> HWND {
    // SAFETY: The fixture owns this live parent and its child window tree.
    unsafe { GetDlgItem(Some(parent), id) }.unwrap()
}

fn window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: The fixture retains this HWND while the local RECT is filled.
    unsafe { GetWindowRect(hwnd, &mut rect).unwrap() };
    rect
}

fn client_on_screen(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    let mut origin = POINT::default();
    // SAFETY: This owned live HWND is queried synchronously into local outputs.
    unsafe {
        GetClientRect(hwnd, &mut rect).unwrap();
        assert!(ClientToScreen(hwnd, &mut origin).as_bool());
    }
    RECT {
        left: origin.x,
        top: origin.y,
        right: origin.x + rect.right,
        bottom: origin.y + rect.bottom,
    }
}

fn local_rect(document: HWND, hwnd: HWND) -> RECT {
    let bounds = window_rect(hwnd);
    let mut origin = POINT {
        x: bounds.left,
        y: bounds.top,
    };
    // SAFETY: Convert only scalar coordinates for this test-owned document.
    unsafe { assert!(ScreenToClient(document, &mut origin).as_bool()) };
    RECT {
        left: origin.x,
        top: origin.y,
        right: origin.x + bounds.right - bounds.left,
        bottom: origin.y + bounds.bottom - bounds.top,
    }
}

fn assert_local_positions(document: HWND, before: &[(i32, RECT)]) {
    for &(id, expected) in before {
        assert_eq!(
            local_rect(document, child(document, id)),
            expected,
            "control {id} must keep document-local geometry while scrolling"
        );
    }
}

fn scroll_info(viewport: HWND) -> SCROLLINFO {
    let mut info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        ..Default::default()
    };
    // SAFETY: The fixture retains this HWND across the sample; this query fills
    // correctly sized local storage and sends no drawing messages.
    unsafe { GetScrollInfo(viewport, SB_VERT, &mut info).unwrap() };
    info
}

fn scroll_command(viewport: HWND, command: SCROLLBAR_COMMAND) {
    // SAFETY: This scalar command targets only the fixture's native viewport.
    unsafe { SendMessageW(viewport, WM_VSCROLL, Some(WPARAM(command.0 as usize)), None) };
}

fn assert_owned_visible(root: HWND, rect: RECT) {
    // SAFETY: Query ownership at sample locations by handle value only.
    unsafe {
        assert!(IsWindowVisible(root).as_bool());
        for y in [rect.top + 2, (rect.top + rect.bottom) / 2, rect.bottom - 2] {
            for x in [rect.left + 2, (rect.left + rect.right) / 2, rect.right - 2] {
                let hit = WindowFromPoint(POINT { x, y });
                assert!(
                    hit == root || IsChild(root, hit).as_bool(),
                    "fixture viewport is obscured"
                );
            }
        }
    }
}

#[derive(Default)]
struct NativeScrollEvents {
    tracking: Arc<AtomicBool>,
    thumb_messages: Cell<u32>,
    thumb_positions: RefCell<HashSet<i32>>,
}

struct NativeScrollObserver {
    viewport: HWND,
    events: Box<NativeScrollEvents>,
}

impl NativeScrollObserver {
    fn new(viewport: HWND) -> Self {
        let observer = Self {
            viewport,
            events: Box::default(),
        };
        // SAFETY: The observer retains its stable Box and the fixture retains
        // the viewport until this subclass is removed on the same UI thread.
        unsafe {
            assert!(
                SetWindowSubclass(
                    viewport,
                    Some(observe_native_scroll),
                    78,
                    observer.events.as_ref() as *const NativeScrollEvents as usize
                )
                .as_bool()
            )
        };
        observer
    }
}

impl Drop for NativeScrollObserver {
    fn drop(&mut self) {
        // SAFETY: The viewport still belongs to the live fixture, and callback
        // removal precedes releasing the Box it borrows.
        unsafe {
            assert!(RemoveWindowSubclass(self.viewport, Some(observe_native_scroll), 78).as_bool())
        };
    }
}

unsafe extern "system" fn observe_native_scroll(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: The stable observer Box outlives the viewport subclass. Only the
    // atomic flag is shared with the independent screen-capture thread.
    let events = unsafe { &*(data as *const NativeScrollEvents) };
    // SAFETY: observe this thread's current capture before production dispatch.
    let dragging = unsafe { GetCapture() } == hwnd;
    // SAFETY: Preserve the production message path; the observer only records
    // state after the real scrollbar handler has moved its document.
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    // SAFETY: scalar capture query on the owning test/UI thread.
    events
        .tracking
        .store(unsafe { GetCapture() } == hwnd, Ordering::Release);
    if message == WM_MOUSEMOVE && dragging {
        // SAFETY: This Box remains alive until the observer removes this
        // subclass; cells are accessed only on the fixture's UI thread.
        events
            .thumb_messages
            .set(events.thumb_messages.get().saturating_add(1));
        // This records progress on each captured move, before mouse-up.
        let position = scroll_info(hwnd).nPos;
        events.thumb_positions.borrow_mut().insert(position);
    }
    result
}

struct ChildObserver {
    handles: Vec<HWND>,
    counts: Box<[Cell<u32>; 2]>,
}

impl ChildObserver {
    fn new(document: HWND) -> Self {
        let observer = Self {
            handles: CONTROL_IDS.iter().map(|&id| child(document, id)).collect(),
            counts: Box::new([Cell::new(0), Cell::new(0)]),
        };
        for &hwnd in &observer.handles {
            // SAFETY: The counts Box stays stable until all owned subclasses are
            // removed. Each native callback borrows it only synchronously.
            unsafe {
                assert!(
                    SetWindowSubclass(
                        hwnd,
                        Some(observe_child),
                        77,
                        observer.counts.as_ref() as *const [Cell<u32>; 2] as usize
                    )
                    .as_bool()
                )
            };
        }
        observer
    }
    fn counts(&self) -> [u32; 2] {
        [self.counts[0].get(), self.counts[1].get()]
    }
    fn reset(&self) {
        for count in self.counts.iter() {
            count.set(0);
        }
    }
}

impl Drop for ChildObserver {
    fn drop(&mut self) {
        for &hwnd in &self.handles {
            // SAFETY: These test-owned children remain live until this observer
            // has removed every callback borrowing its Box.
            unsafe { assert!(RemoveWindowSubclass(hwnd, Some(observe_child), 77).as_bool()) };
        }
    }
}

unsafe extern "system" fn observe_child(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    if let Some(index) = match message {
        WM_SETFONT => Some(0),
        WM_WINDOWPOSCHANGED => Some(1),
        _ => None,
    } {
        // SAFETY: The observer's stable Box remains alive until this subclass is
        // removed; all reads/writes occur on the creating UI thread.
        let counts = unsafe { &*(data as *const [Cell<u32>; 2]) };
        counts[index].set(counts[index].get().saturating_add(1));
    }
    // SAFETY: Preserve the original native subclass chain and payload lifetime.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

struct Owner(HWND);
impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: The settings window has already released this fixture owner.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
