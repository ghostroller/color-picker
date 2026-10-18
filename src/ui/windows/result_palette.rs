//! Native palette buttons keep button accessibility and activation semantics,
//! while sharing exact physical-pixel boundaries with their owner-drawn fills.

use super::*;
use windows::Win32::UI::{
    Controls::{ODS_FOCUS, ODS_NOFOCUSRECT, ODT_BUTTON, WM_MOUSELEAVE},
    Input::KeyboardAndMouse::{
        TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_LEFT, VK_RETURN, VK_RIGHT,
        VK_SPACE, VK_UP,
    },
};

/// Split the complete physical-pixel rectangle rather than rounding each cell
/// width separately. Adjacent cells therefore share the same boundary.
pub(super) fn cell_rect(index: usize, width: i32, top: i32, height: i32) -> RECT {
    assert!(index < ROWS * COLUMNS);
    let column = index % COLUMNS;
    let row = index / COLUMNS;
    let width = i64::from(width.max(0));
    let height = i64::from(height.max(0));
    let top = i64::from(top);
    RECT {
        left: (width * column as i64 / COLUMNS as i64) as i32,
        right: (width * (column + 1) as i64 / COLUMNS as i64) as i32,
        top: (top + height * row as i64 / ROWS as i64) as i32,
        bottom: (top + height * (row + 1) as i64 / ROWS as i64) as i32,
    }
}

/// # Safety
/// Must receive native WM_DRAWITEM storage delivered to this parent. The
/// DRAWITEMSTRUCT, child HWND and borrowed HDC must survive this callback.
pub(super) unsafe fn draw(parent: HWND, payload: LPARAM, state: &CallbackState) -> Option<LRESULT> {
    // SAFETY: The caller supplies the native, aligned WM_DRAWITEM payload.
    let draw = unsafe { (payload.0 as *const DRAWITEMSTRUCT).as_ref() }?;
    let id = draw.CtlID as usize;
    if draw.CtlType != ODT_BUTTON || !(PALETTE_CELL..PALETTE_CELL + ROWS * COLUMNS).contains(&id)
        // SAFETY: The synchronous native payload identifies its still-live child.
        || unsafe { GetParent(draw.hwndItem) }.ok() != Some(parent)
        // SAFETY: Verify the native child's identity as well as the payload ID.
        || unsafe { GetDlgCtrlID(draw.hwndItem) } as usize != id
    {
        return None;
    }
    let index = id - PALETTE_CELL;
    let row = index / COLUMNS;
    let column = index % COLUMNS;
    // Copy the model values before entering GDI: owner drawing must never retain
    // a RefCell borrow across a native call that can synchronously dispatch.
    let (rgb, selected) = {
        let palette = state.palette.borrow();
        let rgb = palette.candidate(row, column)?;
        let selected_column = CENTER as i64 - i64::from(palette.offsets()[row]);
        let selected = state.palette_selected.get().is_some_and(|selected| {
            selected / COLUMNS == row
                && column as i64 == selected_column
                && rgb == palette.current()
        });
        (rgb, selected)
    };
    let color = COLORREF(u32::from(rgb.r) | (u32::from(rgb.g) << 8) | (u32::from(rgb.b) << 16));
    // SAFETY: The validated sender remains live throughout this native draw.
    let dpi = unsafe { GetDpiForWindow(draw.hwndItem) }.max(96);
    let focused = draw.itemState.0 & ODS_FOCUS.0 != 0 && draw.itemState.0 & ODS_NOFOCUSRECT.0 == 0;
    let hovered = state.palette_hover.get() == Some(index);
    // Create owned pens before the saved-DC guard, so restoration precedes their
    // destruction on every ordinary return or unwind path.
    let mut outlines = Vec::new();
    if selected {
        for (inset, ink) in [(2, COLORREF(0)), (3, COLORREF(0x00ff_ffff))] {
            if let Ok(pen) = OwnedPen::new(PS_SOLID, dip(1, dpi).max(1), ink) {
                outlines.push((dip(inset, dpi).max(1), pen));
            }
        }
    }
    if hovered && let Ok(pen) = OwnedPen::new(PS_SOLID, dip(1, dpi).max(1), caption_ink(color)) {
        outlines.push((dip(if selected { 5 } else { 2 }, dpi).max(1), pen));
    }
    if focused && let Ok(pen) = OwnedPen::new(PS_DOT, 1, caption_ink(color)) {
        outlines.push((
            dip(if selected || hovered { 7 } else { 4 }, dpi).max(1),
            pen,
        ));
    }
    // SAFETY: The owner-draw notification lends this DC until this callback returns.
    let Ok(saved) = (unsafe { SavedDc::new(draw.hDC) }) else {
        return Some(LRESULT(0));
    };
    // SAFETY: The validated owner-draw record supplies its live DC and local
    // rectangle. The stock brush is borrowed, and all owned pens outlive restore.
    unsafe {
        SetDCBrushColor(draw.hDC, color);
        FillRect(draw.hDC, &draw.rcItem, HBRUSH(GetStockObject(DC_BRUSH).0));
        SelectObject(draw.hDC, GetStockObject(NULL_BRUSH));
        for (inset, pen) in &outlines {
            let rect = RECT {
                left: draw.rcItem.left + inset,
                top: draw.rcItem.top + inset,
                right: draw.rcItem.right - inset,
                bottom: draw.rcItem.bottom - inset,
            };
            if rect.right > rect.left
                && rect.bottom > rect.top
                && !crate::platform::windows::gdi::invalid_selection(SelectObject(
                    draw.hDC,
                    pen.raw().into(),
                ))
            {
                let _ = Rectangle(draw.hDC, rect.left, rect.top, rect.right, rect.bottom);
            }
        }
    }
    if let Err(error) = saved.restore() {
        for (_, pen) in outlines {
            pen.retain();
        }
        crate::app::diagnostics::event(format_args!("result.palette_restore_failed {error}"));
    }
    Some(LRESULT(1))
}

/// # Safety
/// Must receive native WM_DRAWITEM storage delivered to this parent. Its
/// DRAWITEMSTRUCT, original-color button and borrowed HDC outlive this callback.
pub(super) unsafe fn draw_restore(
    parent: HWND,
    payload: LPARAM,
    state: &CallbackState,
) -> Option<LRESULT> {
    // SAFETY: The caller supplies the native, aligned WM_DRAWITEM payload.
    let draw = unsafe { (payload.0 as *const DRAWITEMSTRUCT).as_ref() }?;
    if draw.CtlType != ODT_BUTTON || draw.CtlID as usize != RESTORE_ORIGINAL
        // SAFETY: This native draw notification identifies its still-live child.
        || unsafe { GetParent(draw.hwndItem) }.ok() != Some(parent)
        // SAFETY: Validate the actual owned button's scalar ID before drawing.
        || unsafe { GetDlgCtrlID(draw.hwndItem) } as usize != RESTORE_ORIGINAL
    {
        return None;
    }
    let original = colorref(state.original_color);
    let result = draw_swatch(draw, original, original);
    let hovered =
        state.restore_hover.get() && state.palette.borrow().current() != state.original_color;
    let focused = draw.itemState.0 & ODS_FOCUS.0 != 0 && draw.itemState.0 & ODS_NOFOCUSRECT.0 == 0;
    if result.0 == 0 || (!hovered && !focused) {
        return Some(result);
    }
    // SAFETY: The validated owner-draw sender remains alive for this DPI query.
    let dpi = unsafe { GetDpiForWindow(draw.hwndItem) }.max(96);
    // Keep the same thin, contrast-aware inner outline as the palette cells.
    let hover_pen = hovered
        .then(|| OwnedPen::new(PS_SOLID, dip(1, dpi).max(1), caption_ink(original)))
        .transpose()
        .ok()
        .flatten();
    // SAFETY: The callback lends the DC until return; the pen outlives restore.
    let Ok(saved) = (unsafe { SavedDc::new(draw.hDC) }) else {
        return Some(result);
    };
    if let Some(pen) = &hover_pen {
        let inset = dip(2, dpi).max(1);
        // SAFETY: The owned pen and borrowed stock brush remain valid until DC
        // restoration; the rectangle is inset and never changes control bounds.
        unsafe {
            SelectObject(draw.hDC, GetStockObject(NULL_BRUSH));
            if !crate::platform::windows::gdi::invalid_selection(SelectObject(
                draw.hDC,
                pen.raw().into(),
            )) {
                let _ = Rectangle(
                    draw.hDC,
                    draw.rcItem.left + inset,
                    draw.rcItem.top + inset,
                    draw.rcItem.right - inset,
                    draw.rcItem.bottom - inset,
                );
            }
        }
    }
    let inset = dip(if hovered { 5 } else { 3 }, dpi).max(1);
    let focus = RECT {
        left: draw.rcItem.left + inset,
        top: draw.rcItem.top + inset,
        right: draw.rcItem.right - inset,
        bottom: draw.rcItem.bottom - inset,
    };
    if focused && focus.right > focus.left && focus.bottom > focus.top {
        // SAFETY: Only this DC's attributes and the inset local RECT are used;
        // the entire original-color fill remains unchanged outside that RECT.
        unsafe {
            SetTextColor(draw.hDC, COLORREF(0));
            SetBkColor(draw.hDC, COLORREF(0x00ff_ffff));
            let _ = DrawFocusRect(draw.hDC, &focus);
        }
    }
    if let Err(error) = saved.restore() {
        if let Some(pen) = hover_pen {
            pen.retain();
        }
        crate::app::diagnostics::event(format_args!("result.restore_focus_restore_failed {error}"));
    }
    Some(result)
}

/// Consume wheel input according to the row beneath the pointer, even when the
/// native keyboard focus remains on a button in a different row.
pub(super) fn wheel(root: HWND, state: &CallbackState, wparam: WPARAM, lparam: LPARAM) -> bool {
    let bounds = state.palette_rect.get();
    if bounds.right <= bounds.left || bounds.bottom <= bounds.top {
        return false;
    }
    let mut point = POINT {
        x: (lparam.0 as u16 as i16) as i32,
        y: ((lparam.0 >> 16) as u16 as i16) as i32,
    };
    // SAFETY: The root is retained by this callback's owner. The initialized
    // stack POINT is writable only for this synchronous coordinate conversion.
    if !unsafe { ScreenToClient(root, &mut point) }.as_bool()
        || point.x < bounds.left
        || point.x >= bounds.right
        || point.y < bounds.top
        || point.y >= bounds.bottom
    {
        return false;
    }
    let height = i64::from(bounds.bottom) - i64::from(bounds.top);
    // Compare against the same accumulated boundaries used by cell_rect;
    // dividing y*ROWS/height would misclassify a boundary when height is odd.
    let row = (0..ROWS)
        .find(|row| {
            i64::from(point.y) < i64::from(bounds.top) + height * (row + 1) as i64 / ROWS as i64
        })
        .expect("an interior palette point belongs to a row");
    let delta = ((wparam.0 >> 16) as u16 as i16) as i32;
    state.scroll_palette(row, delta);
    true
}

fn invalidate_cell(root: HWND, index: usize) {
    if index >= ROWS * COLUMNS {
        return;
    }
    // SAFETY: This callback retains the owned root and its child button tree;
    // the lookup returns a borrowed child without transferring ownership.
    if let Ok(button) = unsafe { GetDlgItem(Some(root), (PALETTE_CELL + index) as i32) } {
        // SAFETY: Invalidation borrows no rectangle and requests later painting
        // only for the still-live native child returned by the owned root.
        let _ = unsafe { InvalidateRect(Some(button), None, false) };
    }
}

fn set_hover(root: HWND, state: &CallbackState, hovered: Option<usize>) {
    let previous = state.palette_hover.replace(hovered);
    if previous != hovered {
        for index in [previous, hovered].into_iter().flatten() {
            invalidate_cell(root, index);
        }
    }
}

/// # Safety
/// Installed only on owned palette BUTTON controls. reference must be the
/// stable CallbackState allocation retained until every child WM_NCDESTROY.
pub(super) unsafe extern "system" fn cell_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The installation contract retains this stable callback Box
        // until this child's native subclass has been removed during teardown.
        let state = unsafe { &*(reference as *const CallbackState) };
        // SAFETY: This live native button supplies only its scalar control ID.
        let id = unsafe { GetDlgCtrlID(hwnd) } as usize;
        let Some(index) = id
            .checked_sub(PALETTE_CELL)
            .filter(|index| *index < ROWS * COLUMNS)
        else {
            // SAFETY: The unchanged native message remains valid during this callback.
            return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        };
        // SAFETY: This owned child remains attached to its retained root until
        // its WM_NCDESTROY callback has returned.
        let root = unsafe { GetParent(hwnd) }.ok();
        let key = wparam.0 as u32;
        match message {
            WM_NCDESTROY => {
                if state.palette_hover.get() == Some(index) {
                    state.palette_hover.set(None);
                }
                // SAFETY: Remove precisely this installed subclass while the
                // parent still retains the callback allocation it borrows.
                let _ = unsafe { RemoveWindowSubclass(hwnd, Some(cell_proc), subclass) };
            }
            WM_MOUSEMOVE => {
                if let Some(root) = root {
                    let mut tracking = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        ..Default::default()
                    };
                    // SAFETY: USER32 copies this correctly sized stack tracking
                    // descriptor; its child HWND remains owned by this root.
                    let hovered = unsafe { TrackMouseEvent(&mut tracking) }
                        .is_ok()
                        .then_some(index);
                    set_hover(root, state, hovered);
                }
            }
            WM_MOUSELEAVE if state.palette_hover.get() == Some(index) => {
                if let Some(root) = root {
                    set_hover(root, state, None);
                }
            }
            WM_MOUSEWHEEL if root.is_some_and(|root| wheel(root, state, wparam, lparam)) => {
                return LRESULT(0);
            }
            WM_GETDLGCODE => {
                // SAFETY: Preserve the native button's dialog code while its
                // original synchronous query payload remains valid.
                let native = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
                let activation = key == u32::from(VK_RETURN.0) || key == u32::from(VK_SPACE.0);
                return LRESULT(
                    native.0
                        | DLGC_WANTARROWS as isize
                        | if activation {
                            DLGC_WANTALLKEYS as isize
                        } else {
                            0
                        },
                );
            }
            WM_KEYDOWN => {
                let row = index / COLUMNS;
                let column = index % COLUMNS;
                let destination = match key {
                    key if key == u32::from(VK_LEFT.0) => {
                        Some(row * COLUMNS + column.saturating_sub(1))
                    }
                    key if key == u32::from(VK_RIGHT.0) => {
                        Some(row * COLUMNS + (column + 1).min(COLUMNS - 1))
                    }
                    key if key == u32::from(VK_UP.0) => {
                        Some(row.saturating_sub(1) * COLUMNS + column)
                    }
                    key if key == u32::from(VK_DOWN.0) => {
                        Some((row + 1).min(ROWS - 1) * COLUMNS + column)
                    }
                    _ => None,
                };
                if let Some(destination) = destination {
                    if let Some(root) = root {
                        // SAFETY: All palette buttons are retained siblings of
                        // this focused button, with stable IDs in the owned root.
                        if let Ok(button) =
                            // SAFETY: This live root owns every destination palette button.
                            unsafe {
                                GetDlgItem(Some(root), (PALETTE_CELL + destination) as i32)
                            }
                        {
                            // SAFETY: Focus stays inside the owned native palette;
                            // no model selection changes during arrow navigation.
                            let _ = unsafe { SetFocus(Some(button)) };
                        }
                    }
                    return LRESULT(0);
                }
                if key == u32::from(VK_RETURN.0) || key == u32::from(VK_SPACE.0) {
                    if lparam.0 & (1 << 30) == 0 {
                        state.select_palette(index);
                    }
                    return LRESULT(0);
                }
            }
            WM_KEYUP | WM_CHAR if key == u32::from(VK_RETURN.0) || key == u32::from(VK_SPACE.0) => {
                // Activation already ran once on the initial keydown; prevent
                // BUTTON's Space handling or the dialog default from copying.
                return LRESULT(0);
            }
            _ => {}
        }
        // SAFETY: This preserves ordinary mouse activation/BN_CLICKED and native
        // accessibility. The original message remains valid throughout dispatch.
        let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        if matches!(message, WM_SETFOCUS | WM_KILLFOCUS) {
            // SAFETY: The child is still live after a focus notification, and
            // invalidation requests its internal focus outline without erasing.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
        }
        result
    }))
    .unwrap_or_else(|_| std::process::abort())
}

/// # Safety
/// Installed only on the owned original-color BUTTON. reference is the stable
/// CallbackState allocation retained until this child's WM_NCDESTROY returns.
pub(super) unsafe extern "system" fn restore_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The subclass installation retains this stable callback Box
        // until its owned original-color button has finished native teardown.
        let state = unsafe { &*(reference as *const CallbackState) };
        let key = wparam.0 as u32;
        let activation = key == u32::from(VK_RETURN.0) || key == u32::from(VK_SPACE.0);
        match message {
            WM_NCDESTROY => {
                state.restore_hover.set(false);
                // SAFETY: Remove this exact installed subclass while its root
                // still retains the callback allocation it borrows.
                let _ = unsafe { RemoveWindowSubclass(hwnd, Some(restore_proc), subclass) };
            }
            WM_MOUSEMOVE => {
                let mut bounds = RECT::default();
                let x = i32::from(lparam.0 as u16 as i16);
                let y = i32::from((lparam.0 >> 16) as u16 as i16);
                // A pressed button can capture moves beyond its client area.
                // SAFETY: The subclass retains this button and a writable local RECT.
                let inside = unsafe { GetClientRect(hwnd, &mut bounds) }.is_ok()
                    && x >= bounds.left
                    && x < bounds.right
                    && y >= bounds.top
                    && y < bounds.bottom;
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    ..Default::default()
                };
                // SAFETY: USER32 copies this stack descriptor for the owned button.
                let hovered = unsafe { TrackMouseEvent(&mut tracking) }.is_ok() && inside;
                if state.restore_hover.replace(hovered) != hovered {
                    // SAFETY: The button is live and is repainted after this callback.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
            }
            WM_MOUSELEAVE if state.restore_hover.replace(false) => {
                // SAFETY: This native leave message targets the still-owned button.
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            WM_GETDLGCODE => {
                // SAFETY: Preserve the native button code while the original
                // query payload remains valid for synchronous default dispatch.
                let native = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
                return LRESULT(
                    native.0
                        | if activation {
                            DLGC_WANTALLKEYS as isize
                        } else {
                            0
                        },
                );
            }
            WM_KEYDOWN if activation => {
                if lparam.0 & (1 << 30) == 0 {
                    state.restore_original();
                }
                return LRESULT(0);
            }
            WM_KEYUP | WM_CHAR if activation => {
                // Restore already ran on the first keydown. Suppress native
                // Space activation and any default-copy handling on release.
                return LRESULT(0);
            }
            WM_SETCURSOR if lparam.0 as u16 as u32 == HTCLIENT => {
                let cursor = if state.palette.borrow().current() != state.original_color {
                    IDC_HAND
                } else {
                    IDC_ARROW
                };
                // SAFETY: The system cursor is shared; the control
                // borrows it and must never destroy it as an owned resource.
                if let Ok(cursor) = unsafe { LoadCursorW(None, cursor) } {
                    // SAFETY: This shared cursor remains valid after the call;
                    // only the current UI thread's pointer presentation changes.
                    unsafe { SetCursor(Some(cursor)) };
                    return LRESULT(1);
                }
            }
            _ => {}
        }
        // SAFETY: Preserve native mouse BN_CLICKED and button accessibility;
        // the original message remains valid for the complete default dispatch.
        let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        if matches!(message, WM_SETFOCUS | WM_KILLFOCUS) {
            // SAFETY: This owned button remains live after a focus notification;
            // request its internal focus rectangle without erasing the swatch.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
        }
        result
    }))
    .unwrap_or_else(|_| std::process::abort())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_cells_tile_complete_physical_rectangles_without_gaps() {
        // These are physical-pixel geometry checks at representative DPI
        // scales, independent from the test monitor's actual system DPI.
        for dpi in [96, 120, 144, 168, 192, 240, 288] {
            for width in [1, 6, 7, 8, 239, 419, 420, 421] {
                for height in [1, 2, 3, 4, 54, 63, 96] {
                    assert_complete_tiling(dip(width, dpi), dip(56, dpi), dip(height, dpi));
                }
            }
        }
    }

    fn assert_complete_tiling(width: i32, top: i32, height: i32) {
        let mut area = 0;
        for row in 0..ROWS {
            for column in 0..COLUMNS {
                let rect = cell_rect(row * COLUMNS + column, width, top, height);
                assert!(rect.left >= 0 && rect.right <= width);
                assert!(rect.top >= top && rect.bottom <= top + height);
                if column == 0 {
                    assert_eq!(rect.left, 0);
                } else {
                    assert_eq!(
                        cell_rect(row * COLUMNS + column - 1, width, top, height).right,
                        rect.left,
                    );
                }
                if row > 0 {
                    assert_eq!(
                        cell_rect((row - 1) * COLUMNS + column, width, top, height).bottom,
                        rect.top,
                    );
                }
                if column + 1 == COLUMNS {
                    assert_eq!(rect.right, width);
                }
                if row + 1 == ROWS {
                    assert_eq!(rect.bottom, top + height);
                }
                area += (rect.right - rect.left) * (rect.bottom - rect.top);
            }
        }
        assert_eq!(area, width * height);
    }
}
