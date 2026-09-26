//! Native message payloads are borrowed only during their synchronous callback.
//! Null checks are defensive; they never establish pointer validity by themselves.
use super::window_lifetime::WindowInit;
use windows::Win32::{
    Foundation::{HWND, LPARAM, RECT},
    UI::{
        Controls::{DRAWITEMSTRUCT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR, ODT_STATIC},
        WindowsAndMessaging::{
            BS_DEFPUSHBUTTON, BS_PUSHBUTTON, BS_TYPEMASK, CREATESTRUCTW, GWL_STYLE, GetClassNameW,
            GetParent, GetWindowLongW,
        },
    },
};

/// # Safety
/// Must be a native WM_NCCREATE from this project's CreateWindowExW call. Its
/// CREATESTRUCTW and WindowInit must stay valid for the synchronous closure.
pub(super) unsafe fn with_window_init<R>(
    payload: LPARAM,
    f: impl FnOnce(&WindowInit) -> R,
) -> Option<R> {
    // SAFETY: caller guarantees the native CREATESTRUCTW allocation and alignment.
    let create = unsafe { (payload.0 as *const CREATESTRUCTW).as_ref() }?;
    // SAFETY: the caller supplied our WindowInit as lpCreateParams for this call.
    let init = unsafe { (create.lpCreateParams as *const WindowInit).as_ref() }?;
    if init.state.is_null() {
        return None;
    }
    Some(f(init))
}

/// # Safety
/// Must be the LPARAM of native WM_DPICHANGED; any non-null RECT is readable
/// and aligned until this callback returns. Return a copy, never a borrowed view.
pub(super) unsafe fn dpi_rect(payload: LPARAM) -> Option<RECT> {
    // SAFETY: the native message contract supplies readable RECT storage.
    unsafe { (payload.0 as *const RECT).as_ref().copied() }
}

/// # Safety
/// Must be native WM_DRAWITEM for this parent during synchronous dispatch.
/// The payload and its HDC remain valid only until the closure returns.
pub(super) unsafe fn with_static_draw<R>(
    parent: HWND,
    payload: LPARAM,
    id: usize,
    f: impl FnOnce(&DRAWITEMSTRUCT) -> R,
) -> Option<R> {
    // SAFETY: native WM_DRAWITEM supplies an initialized DRAWITEMSTRUCT.
    let draw = unsafe { (payload.0 as *const DRAWITEMSTRUCT).as_ref() }?;
    // SAFETY: hwndItem comes from the live native draw notification.
    if draw.CtlType != ODT_STATIC || draw.CtlID as usize != id
        // SAFETY: hwndItem is the live owner-draw sender from the native payload.
        || unsafe { GetParent(draw.hwndItem) }.ok() != Some(parent)
    {
        return None;
    }
    Some(f(draw))
}

/// # Safety
/// Must be a native WM_NOTIFY delivered to this parent. Read the NMHDR first;
/// only a push-button NM_CUSTOMDRAW has the NMCUSTOMDRAW layout consumed here.
/// The closure cannot retain a reference beyond the synchronous notification.
pub(super) unsafe fn with_button_custom_draw<R>(
    parent: HWND,
    payload: LPARAM,
    f: impl FnOnce(&NMCUSTOMDRAW) -> R,
) -> Option<R> {
    // SAFETY: every native WM_NOTIFY begins with a readable, aligned NMHDR.
    let header = unsafe { (payload.0 as *const NMHDR).as_ref() }?;
    if header.code != NM_CUSTOMDRAW {
        return None;
    }
    // SAFETY: hwndFrom is the live sender of this synchronous notification.
    if unsafe { GetParent(header.hwndFrom) }.ok() != Some(parent) {
        return None;
    }
    let mut class = [0_u16; 32];
    // SAFETY: the output buffer is writable and hwndFrom belongs to the notification.
    let length = unsafe { GetClassNameW(header.hwndFrom, &mut class) } as usize;
    if !String::from_utf16_lossy(&class[..length]).eq_ignore_ascii_case("button") {
        return None;
    }
    // SAFETY: querying the live sender's button style does not retain a pointer.
    let kind = unsafe { GetWindowLongW(header.hwndFrom, GWL_STYLE) } & BS_TYPEMASK;
    if kind != BS_PUSHBUTTON && kind != BS_DEFPUSHBUTTON {
        return None;
    }
    // SAFETY: the verified sender class and notification code establish this layout.
    let draw = unsafe { &*(payload.0 as *const NMCUSTOMDRAW) };
    Some(f(draw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        Win32::{
            Graphics::Gdi::{GetDC, ReleaseDC},
            UI::{
                Controls::{CDDS_PREPAINT, ODT_STATIC},
                WindowsAndMessaging::*,
            },
        },
        core::w,
    };
    struct Parent(HWND);
    impl Drop for Parent {
        fn drop(&mut self) {
            // SAFETY: this hidden fixture owns the complete native-only tree;
            // no Rust callback or paint/DC reference survives this scope.
            unsafe { DestroyWindow(self.0) }.unwrap();
        }
    }

    #[test]
    fn typed_payloads_validate_sender_code_and_copy_dpi_rect() {
        // SAFETY: built-in classes retain no stack pointers and all windows stay hidden.
        let parent = Parent(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!(""),
                    WS_OVERLAPPED,
                    0,
                    0,
                    20,
                    20,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .unwrap(),
        );
        // SAFETY: these native controls belong to the live parent fixture.
        let button = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                w!(""),
                WS_CHILD | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                0,
                0,
                10,
                10,
                Some(parent.0),
                Some(HMENU(7_usize as *mut _)),
                None,
                None,
            )
        }
        .unwrap();
        // SAFETY: the static control is destroyed with its parent on this thread.
        let static_control = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!(""),
                WS_CHILD,
                0,
                0,
                10,
                10,
                Some(parent.0),
                Some(HMENU(8_usize as *mut _)),
                None,
                None,
            )
        }
        .unwrap();
        // SAFETY: borrow the live control's DC only within this test and release below.
        let dc = unsafe { GetDC(Some(button)) };
        assert!(!dc.is_invalid());
        let draw = NMCUSTOMDRAW {
            hdr: NMHDR {
                hwndFrom: button,
                idFrom: 7,
                code: NM_CUSTOMDRAW,
            },
            dwDrawStage: CDDS_PREPAINT,
            hdc: dc,
            ..Default::default()
        };
        let calls = std::cell::Cell::new(0);
        // SAFETY: this compliant notification fixture is aligned, initialized and
        // live for the synchronous decoder closure, exactly like WM_NOTIFY storage.
        let decoded = unsafe {
            with_button_custom_draw(parent.0, LPARAM(&draw as *const _ as isize), |value| {
                calls.set(calls.get() + 1);
                assert_eq!(value.hdr.idFrom, 7);
                true
            })
        };
        assert_eq!(decoded, Some(true));
        assert_eq!(calls.get(), 1);
        for header in [
            NMHDR {
                hwndFrom: button,
                idFrom: 7,
                code: 12345,
            },
            NMHDR {
                hwndFrom: static_control,
                idFrom: 8,
                code: NM_CUSTOMDRAW,
            },
        ] {
            // SAFETY: only NMHDR is allocated here. Unsupported code/class must
            // return before attempting to view the larger NMCUSTOMDRAW layout.
            assert!(
                // SAFETY: this valid NMHDR must be rejected before extension access.
                unsafe {
                    with_button_custom_draw(parent.0, LPARAM(&header as *const _ as isize), |_| ())
                }
                .is_none()
            );
        }
        let owner_draw = DRAWITEMSTRUCT {
            CtlType: ODT_STATIC,
            CtlID: 8,
            hwndItem: static_control,
            hDC: dc,
            ..Default::default()
        };
        // SAFETY: compliant owner-draw stack fixture and its handles outlive closure.
        assert_eq!(
            // SAFETY: initialized owner-draw fixture and handles outlive the closure.
            unsafe {
                with_static_draw(
                    parent.0,
                    LPARAM(&owner_draw as *const _ as isize),
                    8,
                    |value| value.CtlID,
                )
            },
            Some(8)
        );
        // SAFETY: same valid payload, with a nonmatching requested control ID.
        assert!(
            // SAFETY: initialized fixture is valid; ID mismatch must return None.
            unsafe {
                with_static_draw(
                    parent.0,
                    LPARAM(&owner_draw as *const _ as isize),
                    9,
                    |_| (),
                )
            }
            .is_none()
        );
        let rectangle = RECT {
            left: -20,
            top: -10,
            right: 120,
            bottom: 80,
        };
        // SAFETY: initialized, aligned RECT fixture remains valid for the copy.
        assert_eq!(
            // SAFETY: the aligned stack RECT remains alive for this value copy.
            unsafe { dpi_rect(LPARAM(&rectangle as *const _ as isize)) },
            Some(rectangle)
        );
        // SAFETY: pair exactly the borrowed window/DC before destroying the tree.
        assert_ne!(unsafe { ReleaseDC(Some(button), dc) }, 0);
    }
}
