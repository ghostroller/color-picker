//! Screen coordinates are physical pixels; rectangles are left/top inclusive.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenPointPx {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRectPx {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ScreenRectPx {
    pub fn is_empty(self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    pub fn width(self) -> u32 {
        (i64::from(self.right) - i64::from(self.left)).max(0) as u32
    }

    pub fn height(self) -> u32 {
        (i64::from(self.bottom) - i64::from(self.top)).max(0) as u32
    }

    pub fn area(self) -> u64 {
        u64::from(self.width()) * u64::from(self.height())
    }

    pub fn contains(self, point: ScreenPointPx) -> bool {
        point.x >= self.left && point.x < self.right && point.y >= self.top && point.y < self.bottom
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let rect = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (!rect.is_empty()).then_some(rect)
    }
}

/// Clip the 65 × 65 freeze area to the one monitor containing `point`.
///
/// Performing the crop before narrowing to i32 also handles screen coordinates
/// near either integer limit. A point in a gap between monitors is not valid.
pub fn freeze_rect(point: ScreenPointPx, monitor: ScreenRectPx) -> Option<ScreenRectPx> {
    if !monitor.contains(point) {
        return None;
    }
    Some(ScreenRectPx {
        left: (i64::from(point.x) - 32).max(i64::from(monitor.left)) as i32,
        top: (i64::from(point.y) - 32).max(i64::from(monitor.top)) as i32,
        right: (i64::from(point.x) + 33).min(i64::from(monitor.right)) as i32,
        bottom: (i64::from(point.y) + 33).min(i64::from(monitor.bottom)) as i32,
    })
}

/// Plan a snapshot after the image viewport has been placed on screen.
///
/// Keep the 65 × 65 capture size at monitor edges by moving it inward. When
/// `point` is in the integer-pixel drawing area, also include the source view
/// that puts that original screen pixel under the cursor. If monitor bounds
/// prevent an exact anchor, the source view is clamped to the monitor first.
/// A cursor outside the drawing area falls back to a capture around the point.
pub fn freeze_rect_for_view(
    point: ScreenPointPx,
    monitor: ScreenRectPx,
    viewport: ScreenRectPx,
    scale: u32,
) -> Option<ScreenRectPx> {
    if !monitor.contains(point) || viewport.is_empty() || scale == 0 {
        return None;
    }
    let width = monitor.width().min(65);
    let height = monitor.height().min(65);
    let (drawn_left, visible_width) =
        image_axis_layout(viewport.left, viewport.width(), width, scale);
    let (drawn_top, visible_height) =
        image_axis_layout(viewport.top, viewport.height(), height, scale);
    let drawn = ScreenRectPx {
        left: drawn_left,
        top: drawn_top,
        right: (i64::from(drawn_left) + i64::from(visible_width) * i64::from(scale)) as i32,
        bottom: (i64::from(drawn_top) + i64::from(visible_height) * i64::from(scale)) as i32,
    };
    let anchor = drawn.contains(point);
    let left = capture_axis_origin(
        point.x,
        monitor.left,
        monitor.right,
        width,
        anchor.then_some((drawn_left, visible_width, scale)),
    );
    let top = capture_axis_origin(
        point.y,
        monitor.top,
        monitor.bottom,
        height,
        anchor.then_some((drawn_top, visible_height, scale)),
    );
    Some(ScreenRectPx {
        left,
        top,
        right: (i64::from(left) + i64::from(width)) as i32,
        bottom: (i64::from(top) + i64::from(height)) as i32,
    })
}

/// Shared with ZoomView so capture planning uses exactly the drawn cell grid.
pub(crate) fn image_axis_layout(
    viewport_start: i32,
    viewport_size: u32,
    image_size: u32,
    scale: u32,
) -> (i32, u32) {
    let visible = image_size.min(viewport_size / scale);
    let padding = (viewport_size - visible * scale) / 2;
    let drawn_start = (i64::from(viewport_start) + i64::from(padding)) as i32;
    (drawn_start, visible)
}

fn capture_axis_origin(
    point: i32,
    monitor_start: i32,
    monitor_end: i32,
    capture_size: u32,
    anchor: Option<(i32, u32, u32)>,
) -> i32 {
    let point = i64::from(point);
    let monitor_start = i64::from(monitor_start);
    let monitor_end = i64::from(monitor_end);
    let capture_size = i64::from(capture_size);
    let centered = (point - capture_size / 2).clamp(monitor_start, monitor_end - capture_size);
    let Some((drawn_start, visible, scale)) = anchor else {
        return centered as i32;
    };
    let visible = i64::from(visible);
    let cell = (point - i64::from(drawn_start)) / i64::from(scale);
    let source_start = (point - cell).clamp(monitor_start, monitor_end - visible);
    // These bounds guarantee that the complete initial source view is cached.
    let earliest = monitor_start.max(source_start + visible - capture_size);
    let latest = (monitor_end - capture_size).min(source_start);
    centered.clamp(earliest, latest) as i32
}
