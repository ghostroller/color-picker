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
