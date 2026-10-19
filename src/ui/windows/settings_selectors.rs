//! Paint settings selectors while USER32 retains their selection, popup,
//! keyboard and accessibility behavior, including their string-based CB_* API.

use super::{CallbackState, FORMAT, LANGUAGE, THEME, dip};
use crate::platform::windows::gdi::{PaintSession, SavedDc};
use std::{
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
};
use windows::{
    Win32::{
        Foundation::{COLORREF, E_FAIL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::*,
        UI::{
            Controls::{
                DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_COMBOBOXEDIT, ODS_DISABLED, ODS_SELECTED,
                ODT_COMBOBOX, WM_MOUSELEAVE,
            },
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                GetFocus, IsWindowEnabled, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::{Error, Result},
};

pub(super) const SELECTION_HEIGHT: i32 = 30;
pub(super) const ITEM_HEIGHT: i32 = 34;
const SUBCLASS: usize = 3;

pub(super) fn install(hwnd: HWND, state: &CallbackState) -> Result<()> {
    // SAFETY: Each control belongs to the root that retains this stable callback
    // Box until all child WM_NCDESTROY notifications have returned.
    if unsafe {
        SetWindowSubclass(
            hwnd,
            Some(selector_proc),
            SUBCLASS,
            state as *const _ as usize,
        )
    }
    .as_bool()
    {
        Ok(())
    } else {
        Err(Error::new(E_FAIL, "Could not prepare settings selector"))
    }
}

/// # Safety
/// Only call for a native WM_MEASUREITEM whose payload lives for this dispatch.
/// Creation can deliver this message before CreateWindowExW returns the control.
pub(super) unsafe fn measure(parent: HWND, payload: LPARAM) -> Option<LRESULT> {
    // SAFETY: The native message supplies writable, aligned MEASUREITEMSTRUCT storage.
    let item = unsafe { (payload.0 as *mut MEASUREITEMSTRUCT).as_mut() }?;
    if item.CtlType != ODT_COMBOBOX || !matches!(item.CtlID as usize, LANGUAGE | THEME | FORMAT) {
        return None;
    }
    // SAFETY: Only the existing parent is queried; the new combo need not yet
    // have a returned HWND or an installed subclass when initial sizing occurs.
    item.itemHeight = dip(ITEM_HEIGHT, unsafe { GetDpiForWindow(parent) }.max(96)) as u32;
    Some(LRESULT(1))
}

/// # Safety
/// Only call for native WM_DRAWITEM; its sender and HDC live until return.
pub(super) unsafe fn draw(parent: HWND, payload: LPARAM, state: &CallbackState) -> Option<LRESULT> {
    // SAFETY: WM_DRAWITEM provides this initialized, aligned structure.
    let item = unsafe { (payload.0 as *const DRAWITEMSTRUCT).as_ref() }?;
    if item.CtlType != ODT_COMBOBOX
        || !matches!(item.CtlID as usize, LANGUAGE | THEME | FORMAT)
        // SAFETY: hwndItem is the live native owner-draw sender.
        || unsafe { GetParent(item.hwndItem) }.ok() != Some(parent)
    {
        return None;
    }
    let palette = state.theme.palette();
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
    let collapsed = item.itemState.0 & ODS_COMBOBOXEDIT.0 != 0;
    let fill = if collapsed {
        palette.field
    } else if selected {
        blend(
            palette.panel,
            palette.accent,
            if state.theme.is_dark() { 20 } else { 12 },
        )
    } else {
        palette.panel
    };
    // SAFETY: The draw message keeps this DC and its control font alive.
    let saved = unsafe { SavedDc::new(item.hDC) }.ok()?;
    fill_rect(item.hDC, &item.rcItem, fill);
    let mut text_rect = item.rcItem;
    // SAFETY: This is the validated, existing owner-draw control.
    let dpi = unsafe { GetDpiForWindow(item.hwndItem) }.max(96);
    text_rect.left += dip(12, dpi);
    text_rect.right -= dip(12, dpi);
    if item.itemID != u32::MAX {
        draw_text(
            item.hwndItem,
            item.hDC,
            item.itemID as usize,
            text_rect,
            if disabled { palette.muted } else { palette.ink },
        );
    }
    if let Err(error) = saved.restore() {
        state.theme.control_restore_failed(&error);
    }
    Some(LRESULT(1))
}

unsafe extern "system" fn selector_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    // SAFETY: This installed subclass receives native message payloads and its
    // root-owned stable callback pointer. Rust cannot unwind across USER32.
    catch_unwind(AssertUnwindSafe(|| unsafe {
        dispatch(hwnd, message, wparam, lparam, subclass, reference)
    }))
    .unwrap_or_else(|_| std::process::abort())
}

unsafe fn dispatch(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    // SAFETY: Root teardown keeps this installed callback Box alive through
    // child WM_NCDESTROY; no mutable borrow spans any native dispatch.
    let state = unsafe { &*(reference as *const CallbackState) };
    match message {
        WM_PAINT => {
            // SAFETY: This live HWND is handling its own WM_PAINT.
            if let Ok(paint) = unsafe { PaintSession::begin(hwnd) } {
                paint_selection(hwnd, paint.dc, state);
            }
            LRESULT(0)
        }
        WM_PRINTCLIENT => {
            paint_selection(hwnd, HDC(wparam.0 as *mut _), state);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // All chrome is inside the rounded client surface; do not draw the
        // classic nonclient border over it when visual styles are disabled.
        WM_NCPAINT => LRESULT(0),
        WM_NCDESTROY => {
            // SAFETY: Remove precisely this live subclass before default native
            // teardown; its borrowed root state remains alive throughout.
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(selector_proc), subclass);
                DefSubclassProc(hwnd, message, wparam, lparam)
            }
        }
        _ => {
            if message == WM_MOUSEMOVE {
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    ..Default::default()
                };
                // SAFETY: This local descriptor belongs to the live control;
                // TrackMouseEvent copies it and retains no Rust storage.
                let _ = unsafe { TrackMouseEvent(&mut tracking) };
            }
            // SAFETY: All input and CB_* messages remain native so arrow keys,
            // type-ahead, F4, selection notifications and accessibility survive.
            let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
            if matches!(
                message,
                WM_MOUSEMOVE
                    | WM_MOUSELEAVE
                    | WM_LBUTTONDOWN
                    | WM_LBUTTONUP
                    | WM_SETFOCUS
                    | WM_KILLFOCUS
                    | WM_ENABLE
                    | WM_THEMECHANGED
                    | WM_KEYDOWN
                    | WM_SYSKEYDOWN
                    | CB_SETCURSEL
                    | CB_SHOWDROPDOWN
            ) {
                // SAFETY: Repaint only this live selector after native state
                // changes; the renderer queries its new value synchronously.
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            result
        }
    }
}

fn paint_selection(hwnd: HWND, hdc: HDC, state: &CallbackState) {
    // SAFETY: The paint/print caller borrows a live DC and HWND synchronously.
    let Ok(saved) = (unsafe { SavedDc::new(hdc) }) else {
        return;
    };
    let mut rect = RECT::default();
    let mut pointer = POINT::default();
    // SAFETY: All output buffers are initialized local storage and the control
    // remains alive throughout this synchronous paint.
    let (dpi, enabled, focus, open, hot) = unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
        let pointer_ok =
            GetCursorPos(&mut pointer).is_ok() && ScreenToClient(hwnd, &mut pointer).as_bool();
        (
            GetDpiForWindow(hwnd).max(96),
            IsWindowEnabled(hwnd).as_bool(),
            GetFocus() == hwnd,
            SendMessageW(hwnd, CB_GETDROPPEDSTATE, None, None).0 != 0,
            pointer_ok
                && pointer.x >= 0
                && pointer.x < rect.right
                && pointer.y >= 0
                && pointer.y < rect.bottom,
        )
    };
    let palette = state.theme.palette();
    let fill = if !enabled {
        palette.panel
    } else if hot || open {
        blend(palette.field, palette.accent, 7)
    } else {
        palette.field
    };
    let border = if enabled && (hot || focus || open) {
        palette.accent
    } else {
        palette.border
    };
    let ink = if enabled { palette.ink } else { palette.muted };
    fill_rect(hdc, &rect, palette.panel);
    // SAFETY: SavedDc scopes all borrowed stock-object selections.
    unsafe {
        SelectObject(hdc, GetStockObject(DC_BRUSH));
        SelectObject(hdc, GetStockObject(DC_PEN));
        SetDCBrushColor(hdc, fill);
        SetDCPenColor(hdc, border);
        let _ = RoundRect(
            hdc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            dip(10, dpi),
            dip(10, dpi),
        );
    }
    let mut text_rect = rect;
    text_rect.left += dip(12, dpi);
    text_rect.right -= dip(36, dpi);
    // SAFETY: This native string combo is alive and returns a scalar item index.
    let selected = unsafe { SendMessageW(hwnd, CB_GETCURSEL, None, None) }.0;
    if selected >= 0 {
        draw_text(hwnd, hdc, selected as usize, text_rect, ink);
    }
    let center = POINT {
        x: rect.right - dip(18, dpi),
        y: (rect.top + rect.bottom) / 2,
    };
    let width = dip(4, dpi);
    let rise = dip(2, dpi);
    let thickness = dip(1, dpi).max(1);
    // A filled chevron has a DPI-scaled stroke without allocating a GDI pen.
    let points = [
        POINT {
            x: center.x - width,
            y: center.y - rise,
        },
        POINT {
            x: center.x,
            y: center.y + rise,
        },
        POINT {
            x: center.x + width,
            y: center.y - rise,
        },
        POINT {
            x: center.x + width,
            y: center.y - rise + thickness,
        },
        POINT {
            x: center.x,
            y: center.y + rise + thickness,
        },
        POINT {
            x: center.x - width,
            y: center.y - rise + thickness,
        },
    ];
    // SAFETY: The active DC and the local POINT array live throughout drawing.
    unsafe {
        SelectObject(hdc, GetStockObject(NULL_PEN));
        SetDCBrushColor(
            hdc,
            if enabled {
                palette.muted
            } else {
                palette.border
            },
        );
        let _ = Polygon(hdc, &points);
    }
    if let Err(error) = saved.restore() {
        state.theme.control_restore_failed(&error);
    }
}

fn fill_rect(hdc: HDC, rect: &RECT, color: COLORREF) {
    // SAFETY: Callers hold a SavedDc guard and keep this borrowed DC alive.
    unsafe {
        SelectObject(hdc, GetStockObject(DC_BRUSH));
        SetDCBrushColor(hdc, color);
        FillRect(hdc, rect, HBRUSH(GetStockObject(DC_BRUSH).0));
    }
}

fn draw_text(hwnd: HWND, hdc: HDC, index: usize, mut rect: RECT, color: COLORREF) {
    // SAFETY: CBS_HASSTRINGS guarantees these documented CB_* calls retrieve
    // text, including when CBS_OWNERDRAWFIXED is present.
    let length = unsafe { SendMessageW(hwnd, CB_GETLBTEXTLEN, Some(WPARAM(index)), None) }.0;
    if !(0..=4096).contains(&length) {
        return;
    }
    let mut label = vec![0_u16; length as usize + 1];
    // SAFETY: This UI-thread combo cannot mutate between these synchronous
    // queries; the correctly sized local UTF-16 buffer lives until return.
    let copied = unsafe {
        SendMessageW(
            hwnd,
            CB_GETLBTEXT,
            Some(WPARAM(index)),
            Some(LPARAM(label.as_mut_ptr() as isize)),
        )
    }
    .0;
    if copied < 0 || copied as usize >= label.len() {
        return;
    }
    // SAFETY: The enclosing SavedDc guard restores the borrowed control font;
    // the bounded text slice remains alive for DrawTextW.
    unsafe {
        let font = SendMessageW(hwnd, WM_GETFONT, None, None);
        if font.0 != 0 {
            SelectObject(hdc, HGDIOBJ(font.0 as *mut _));
        }
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        DrawTextW(
            hdc,
            &mut label[..copied as usize],
            &mut rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    }
}

fn blend(background: COLORREF, foreground: COLORREF, percent: u32) -> COLORREF {
    let channel = |shift: u32| {
        let a = (background.0 >> shift) & 0xff_u32;
        let b = (foreground.0 >> shift) & 0xff_u32;
        ((a * (100 - percent) + b * percent) / 100) << shift
    };
    COLORREF(channel(0) | channel(8) | channel(16))
}
