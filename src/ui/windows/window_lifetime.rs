//! The Rust owner outlives the complete native window tree, including subclasses.
use std::{cell::Cell, ffi::c_void, io::Write, marker::PhantomData, rc::Rc};

use windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, GetLastError, HWND, SetLastError},
        UI::WindowsAndMessaging::{DestroyWindow, GWLP_USERDATA, SetWindowLongPtrW},
    },
    core::Result,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    NotAttached,
    Alive(HWND),
    Destroying(HWND),
    Destroyed,
}

#[derive(Default)]
pub(super) struct WindowLifetime {
    phase: Cell<Phase>,
    _thread: PhantomData<Rc<()>>,
}

#[derive(Clone, Copy)]
pub(super) enum WindowRole {
    Root,
    Content,
}

/// Only borrowed during synchronous CreateWindowExW / WM_NCCREATE dispatch.
pub(super) struct WindowInit {
    pub state: *const c_void,
    pub role: WindowRole,
}

#[cfg(test)]
thread_local! { static HIDDEN_FIXTURES: Cell<bool> = const { Cell::new(false) }; }

/// Test-only desktop isolation; release builds always retain ordinary visibility.
pub(super) fn show_native_windows() -> bool {
    #[cfg(test)]
    {
        !HIDDEN_FIXTURES.with(Cell::get)
    }
    #[cfg(not(test))]
    {
        true
    }
}

#[cfg(test)]
pub(super) fn with_hidden_windows<R>(run: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            HIDDEN_FIXTURES.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(HIDDEN_FIXTURES.with(|s| s.replace(true)));
    run()
}

/// Production dispatch is inlined; the thread-local failure switch exists only
/// in tests and fails before the chosen native creation call.
#[inline]
pub(super) fn create_window(create: impl FnOnce() -> Result<HWND>) -> Result<HWND> {
    #[cfg(test)]
    if TEST_CREATION.with(|counter| {
        let (remaining, attached, terminated) = counter.get();
        counter.set((remaining.saturating_sub(1), attached, terminated));
        remaining == 1
    }) {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "injected window creation failure",
        ));
    }
    create()
}

#[cfg(test)]
thread_local! {
    static TEST_CREATION: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
}

#[cfg(test)]
pub(super) fn creation_failure_test(nth: usize, run: impl FnOnce()) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_CREATION.with(|s| s.set((0, 0, 0)));
        }
    }
    let _reset = Reset;
    TEST_CREATION.with(|s| s.set((nth, 0, 0)));
    run();
    TEST_CREATION.with(|s| {
        let (remaining, attached, terminated) = s.get();
        assert_eq!(remaining, 0, "injected creation point was not reached");
        assert_eq!(
            attached, terminated,
            "every attached root must finish before state can drop"
        );
        assert_eq!(attached, usize::from(nth > 1));
    });
}

impl WindowLifetime {
    /// # Safety
    /// Called once by WM_NCCREATE with a stable callback allocation owned by the
    /// creating UI thread. Content windows must belong to this root's tree.
    pub unsafe fn attach(&self, hwnd: HWND, init: &WindowInit) -> bool {
        // SAFETY: hwnd is in synchronous creation; only the stable state pointer
        // (never the stack WindowInit) is retained until WM_NCDESTROY.
        unsafe {
            SetLastError(ERROR_SUCCESS);
            let previous = SetWindowLongPtrW(hwnd, GWLP_USERDATA, init.state as isize);
            if previous == 0 && GetLastError() != ERROR_SUCCESS {
                return false;
            }
        }
        if matches!(init.role, WindowRole::Root) {
            if self.phase.get() != Phase::NotAttached {
                fatal("attach", self.phase.get(), false);
            }
            self.phase.set(Phase::Alive(hwnd));
            #[cfg(test)]
            TEST_CREATION.with(|s| {
                let (n, a, t) = s.get();
                s.set((n, a + 1, t));
            });
        }
        true
    }

    /// Called only at the end of the matching WM_NCDESTROY, after the default
    /// procedure and userdata clearing. Child termination cannot finish a root.
    pub fn terminated(&self, hwnd: HWND) {
        if matches!(self.phase.get(), Phase::Alive(root) | Phase::Destroying(root) if root == hwnd)
        {
            self.phase.set(Phase::Destroyed);
            #[cfg(test)]
            TEST_CREATION.with(|s| {
                let (n, a, t) = s.get();
                s.set((n, a, t + 1));
            });
        }
    }

    /// Creation can fail after WM_NCCREATE attached the stable callback. Resolve
    /// that native lifetime while the allocation and every dependency still exist.
    pub fn creation_result(&self, kind: &'static str, result: Result<HWND>) -> Result<HWND> {
        if result.is_err() {
            self.destroy(kind);
        }
        result
    }

    pub fn is_alive(&self) -> bool {
        matches!(self.phase.get(), Phase::Alive(_))
    }

    /// Complete teardown before the caller lets callback-dependent fields drop.
    /// No RefCell borrow may cross this call; DestroyWindow reenters callbacks.
    pub fn destroy(&self, kind: &'static str) {
        self.destroy_with(kind, |hwnd| {
            // SAFETY: only the recorded root on its creating thread is used;
            // the Rust owner keeps callback, fonts and icons alive through return.
            unsafe { DestroyWindow(hwnd) }
        });
    }

    fn destroy_with(&self, kind: &'static str, destroy: impl FnOnce(HWND) -> Result<()>) {
        match self.phase.get() {
            Phase::NotAttached | Phase::Destroyed => (),
            Phase::Alive(hwnd) => {
                self.phase.set(Phase::Destroying(hwnd));
                let success = destroy(hwnd).is_ok();
                if self.phase.get() != Phase::Destroyed {
                    fatal(kind, self.phase.get(), success);
                }
            }
            Phase::Destroying(_) => fatal(kind, self.phase.get(), false),
        }
    }
}

fn fatal(kind: &str, phase: Phase, api_success: bool) -> ! {
    // Ignore diagnostic I/O failure; no UI, logger locks or unwinding may bypass abort.
    let _ = writeln!(
        std::io::stderr().lock(),
        "window.teardown_fatal kind={kind} phase={phase:?} api_success={api_success}"
    );
    std::process::abort()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use windows::{
        Win32::{
            Foundation::{ERROR_CLASS_ALREADY_EXISTS, LPARAM, LRESULT, WPARAM},
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
                WindowsAndMessaging::*,
            },
        },
        core::w,
    };

    struct ProtectedFont {
        owner: Option<crate::platform::windows::gdi::OwnedFont>,
        events: Rc<RefCell<Vec<&'static str>>>,
    }
    impl Drop for ProtectedFont {
        fn drop(&mut self) {
            drop(self.owner.take());
            self.events.borrow_mut().push("font freed");
        }
    }
    struct NativeData {
        lifetime: WindowLifetime,
        font: ProtectedFont,
        freed_marker: Option<std::path::PathBuf>,
        events: Rc<RefCell<Vec<&'static str>>>,
        reject_create: bool,
    }
    impl Drop for NativeData {
        fn drop(&mut self) {
            self.events.borrow_mut().push("callback freed");
            if let Some(path) = &self.freed_marker {
                std::fs::write(path, b"freed").unwrap();
            }
        }
    }
    struct NativeFixture {
        root: HWND,
        content: HWND,
        data: Box<NativeData>,
    }
    impl Drop for NativeFixture {
        fn drop(&mut self) {
            self.data.lifetime.destroy("native fixture");
        }
    }

    /// # Safety
    /// Registered only for the hidden fixture class; lparam and userdata refer
    /// to its stable NativeData until the root NCDESTROY returns.
    unsafe extern "system" fn fixture_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if message == WM_NCCREATE {
                // SAFETY: synchronous native creation carries this fixture's WindowInit.
                return unsafe { super::super::messages::with_window_init(lparam, |init| {
                    let data = &*(init.state as *const NativeData);
                    LRESULT(isize::from(data.lifetime.attach(hwnd, init)))
                }) }.unwrap_or(LRESULT(0));
            }
            // SAFETY: only the NativeFixture owner installs userdata for this class.
            let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const NativeData;
            // SAFETY: owner retains this stable Box through synchronous teardown.
            if let Some(data) = unsafe { pointer.as_ref() } {
                if message == WM_CREATE && data.reject_create { return LRESULT(-1); }
                if message == WM_NCDESTROY {
                    let root = matches!(data.lifetime.phase.get(), Phase::Alive(root) | Phase::Destroying(root) if root == hwnd);
                    // SAFETY: finish the real native callback chain with live state.
                    let result = unsafe {
                        let result = DefWindowProcW(hwnd, message, wparam, lparam);
                        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                        result
                    };
                    data.events.borrow_mut().push(if root { "root ended" } else { "content ended" });
                    data.lifetime.terminated(hwnd);
                    return result;
                }
            }
            // SAFETY: forward the native message without retaining any payload.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        })).unwrap_or_else(|_| std::process::abort())
    }

    /// # Safety
    /// Native descendant subclass dispatch supplies its live HWND and original
    /// message payload; reference is the root-owned NativeData allocation.
    unsafe extern "system" fn fixture_subclass(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        reference: usize,
    ) -> LRESULT {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if message == WM_NCDESTROY {
                // SAFETY: reference was installed from the stable fixture Box and
                // this descendant terminates before its root's final callback.
                let data = unsafe { &*(reference as *const NativeData) };
                data.events.borrow_mut().push("subclass ended");
                // SAFETY: remove exactly this installed subclass on its owning thread.
                let _ = unsafe { RemoveWindowSubclass(hwnd, Some(fixture_subclass), id) };
            }
            // SAFETY: preserve the native subclass chain for the same message.
            unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
        }))
        .unwrap_or_else(|_| std::process::abort())
    }

    impl NativeFixture {
        fn new(reject_create: bool, events: Rc<RefCell<Vec<&'static str>>>) -> Result<Self> {
            // SAFETY: process module is borrowed; no ownership is taken.
            let instance = unsafe { GetModuleHandleW(None)? }.into();
            let class = WNDCLASSW {
                lpfnWndProc: Some(fixture_proc),
                hInstance: instance,
                lpszClassName: w!("ColorPicker.LifetimeTest"),
                ..Default::default()
            };
            // SAFETY: class and callback are valid for the process lifetime.
            if unsafe { RegisterClassW(&class) } == 0
                // SAFETY: inspect only the preceding registration failure.
                && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
            {
                return Err(windows::core::Error::from_thread());
            }
            use windows::Win32::Graphics::Gdi::*;
            // SAFETY: fresh font creation transfers unique ownership; the root's
            // descendants release their WM_SETFONT borrow before this owner drops.
            let font = unsafe {
                crate::platform::windows::gdi::OwnedFont::from_raw(CreateFontW(
                    -12,
                    0,
                    0,
                    0,
                    400,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    0,
                    w!("Segoe UI"),
                ))
            }?;
            let data = Box::new(NativeData {
                lifetime: WindowLifetime::default(),
                font: ProtectedFont {
                    owner: Some(font),
                    events: events.clone(),
                },
                freed_marker: None,
                events,
                reject_create,
            });
            let init = WindowInit {
                state: (&*data as *const NativeData).cast(),
                role: WindowRole::Root,
            };
            // SAFETY: this hidden root borrows the stack init only during creation;
            // its stable callback Box lives through creation_result and the owner.
            let root = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class.lpszClassName,
                    w!(""),
                    WS_OVERLAPPED,
                    0,
                    0,
                    32,
                    32,
                    None,
                    None,
                    Some(instance),
                    Some((&init as *const WindowInit).cast()),
                )
            };
            let root = data.lifetime.creation_result("fixture creation", root)?;
            let mut fixture = Self {
                root,
                content: HWND::default(),
                data,
            };
            let init = WindowInit {
                state: (&*fixture.data as *const NativeData).cast(),
                role: WindowRole::Content,
            };
            // SAFETY: content belongs to the live root and shares its retained Box.
            fixture.content = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class.lpszClassName,
                    w!(""),
                    WS_CHILD,
                    0,
                    0,
                    16,
                    16,
                    Some(root),
                    None,
                    Some(instance),
                    Some((&init as *const WindowInit).cast()),
                )
            }?;
            // SAFETY: hidden native control is a descendant and borrows no stack data.
            let child = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!(""),
                    WS_CHILD,
                    0,
                    0,
                    8,
                    8,
                    Some(fixture.content),
                    None,
                    Some(instance),
                    None,
                )
            }?;
            // SAFETY: the font owner remains in the root's stable Box until all
            // native descendants, including this static control, are destroyed.
            unsafe {
                SendMessageW(
                    child,
                    WM_SETFONT,
                    Some(WPARAM(
                        fixture.data.font.owner.as_ref().unwrap().raw().0 as usize,
                    )),
                    Some(LPARAM(0)),
                );
            }
            // SAFETY: the subclass reference is the root-owned stable allocation;
            // descendants remove it during native destruction, before Box release.
            if !unsafe {
                SetWindowSubclass(
                    child,
                    Some(fixture_subclass),
                    1,
                    &*fixture.data as *const NativeData as usize,
                )
            }
            .as_bool()
            {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "fixture subclass failure",
                ));
            }
            Ok(fixture)
        }
    }

    #[test]
    fn real_native_tree_ends_before_callback_allocation_drops() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let fixture = NativeFixture::new(false, events.clone()).unwrap();
        drop(fixture);
        assert_eq!(
            &*events.borrow(),
            &[
                "subclass ended",
                "content ended",
                "root ended",
                "callback freed",
                "font freed"
            ]
        );
    }

    #[test]
    fn real_native_owner_destruction_is_not_repeated_by_rust_drop() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let fixture = NativeFixture::new(false, events.clone()).unwrap();
        // SAFETY: the root is live and owned on this thread; its Box remains alive.
        unsafe { DestroyWindow(fixture.root) }.unwrap();
        assert!(!fixture.data.lifetime.is_alive());
        drop(fixture);
        assert_eq!(
            &*events.borrow(),
            &[
                "subclass ended",
                "content ended",
                "root ended",
                "callback freed",
                "font freed"
            ]
        );
    }

    #[test]
    fn native_create_rejection_after_attachment_is_terminal_before_free() {
        let events = Rc::new(RefCell::new(Vec::new()));
        assert!(NativeFixture::new(true, events.clone()).is_err());
        assert_eq!(
            &*events.borrow(),
            &["root ended", "callback freed", "font freed"]
        );
    }

    #[test]
    fn teardown_child_process() {
        let Ok(directory) = std::env::var("COLOR_PICKER_TEST_TEARDOWN") else {
            return;
        };
        let directory = std::path::PathBuf::from(directory);
        let mut fixture = NativeFixture::new(false, Rc::new(RefCell::new(Vec::new()))).unwrap();
        fixture.data.freed_marker = Some(directory.join("freed"));
        fixture.data.lifetime.destroy_with("injected", |root| {
            std::fs::write(directory.join("ready"), b"ready").unwrap();
            if directory.join("normal").exists() {
                // SAFETY: the real fixture root and its callback allocation are live.
                unsafe { DestroyWindow(root) }
            } else {
                Err(windows::core::Error::empty())
            }
        });
        drop(fixture);
    }

    #[test]
    fn failed_destroy_aborts_before_protected_fields_drop() {
        for normal in [true, false] {
            let directory = std::env::temp_dir().join(format!(
                "color-picker-teardown-{}-{normal}",
                std::process::id()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            for name in ["normal", "ready", "freed"] {
                let _ = std::fs::remove_file(directory.join(name));
            }
            if normal {
                std::fs::write(directory.join("normal"), b"normal").unwrap();
            }
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ui::windows::window_lifetime::tests::teardown_child_process",
                    "--nocapture",
                ])
                .env("COLOR_PICKER_TEST_TEARDOWN", &directory)
                .status()
                .unwrap();
            assert_eq!(status.success(), normal);
            assert!(directory.join("ready").exists());
            assert_eq!(directory.join("freed").exists(), normal);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}
