//! Position a physical-pixel preview without covering the sampled pixel.

use super::geometry::{ScreenPointPx, ScreenRectPx};

/// Prefer below/right, below/left, above/right, then above/left of `point`.
/// `gap` counts empty physical pixels between the sampled pixel and the preview.
/// If no preferred rectangle fits, clamp the same candidates to the work area;
/// this may reduce the gap, but never covers `point` or clips the preview.
/// A point in a taskbar can be outside the work area. Zero-sized previews,
/// undersized work areas, and layouts that must cover the point return `None`.
pub fn place_preview(
    point: ScreenPointPx,
    work_area: ScreenRectPx,
    width: u32,
    height: u32,
    gap: u32,
) -> Option<ScreenRectPx> {
    if width == 0
        || height == 0
        || work_area.is_empty()
        || width > work_area.width()
        || height > work_area.height()
    {
        return None;
    }

    let width = i64::from(width);
    let height = i64::from(height);
    let gap = i64::from(gap);
    let x = i64::from(point.x);
    let y = i64::from(point.y);
    let right = x + 1 + gap;
    let left = x - gap - width;
    let below = y + 1 + gap;
    let above = y - gap - height;
    let candidates = [(right, below), (left, below), (right, above), (left, above)];
    let minimum_x = i64::from(work_area.left);
    let maximum_x = i64::from(work_area.right) - width;
    let minimum_y = i64::from(work_area.top);
    let maximum_y = i64::from(work_area.bottom) - height;

    // Try every complete quadrant before relaxing the preferred cursor gap.
    for (x, y) in candidates {
        if x >= minimum_x && x <= maximum_x && y >= minimum_y && y <= maximum_y {
            return rectangle(x, y, width, height);
        }
    }
    for (x, y) in candidates {
        let rect = rectangle(
            x.clamp(minimum_x, maximum_x),
            y.clamp(minimum_y, maximum_y),
            width,
            height,
        )?;
        if !rect.contains(point) {
            return Some(rect);
        }
    }
    None
}

fn rectangle(left: i64, top: i64, width: i64, height: i64) -> Option<ScreenRectPx> {
    Some(ScreenRectPx {
        left: i32::try_from(left).ok()?,
        top: i32::try_from(top).ok()?,
        right: i32::try_from(left + width).ok()?,
        bottom: i32::try_from(top + height).ok()?,
    })
}
