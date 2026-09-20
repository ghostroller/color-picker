//! Immutable BGRX snapshots and integer-pixel magnifier mapping.

use crate::core::color::Rgb8;
use crate::core::geometry::{ScreenPointPx, ScreenRectPx};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenImage {
    pub origin: ScreenPointPx,
    pub width: u32,
    pub height: u32,
    pub stride_bytes: usize,
    /// Top-down BGRX rows. The fourth byte is unused, not alpha.
    pub bgrx: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    EmptyImage,
    CoordinateOverflow,
    InvalidStride,
    TruncatedBuffer,
}

impl FrozenImage {
    pub fn validate(&self) -> Result<(), ImageError> {
        if self.width == 0 || self.height == 0 {
            return Err(ImageError::EmptyImage);
        }
        if i64::from(self.origin.x) + i64::from(self.width) - 1 > i64::from(i32::MAX)
            || i64::from(self.origin.y) + i64::from(self.height) - 1 > i64::from(i32::MAX)
        {
            return Err(ImageError::CoordinateOverflow);
        }
        let row_bytes = usize::try_from(self.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or(ImageError::InvalidStride)?;
        if self.stride_bytes < row_bytes {
            return Err(ImageError::InvalidStride);
        }
        let required = usize::try_from(self.height)
            .ok()
            .and_then(|height| self.stride_bytes.checked_mul(height))
            .ok_or(ImageError::TruncatedBuffer)?;
        if self.bgrx.len() < required {
            return Err(ImageError::TruncatedBuffer);
        }
        Ok(())
    }

    pub fn pixel_at(&self, x: u32, y: u32) -> Option<Rgb8> {
        self.validate().ok()?;
        if x >= self.width || y >= self.height {
            return None;
        }
        let column = usize::try_from(x).ok()?.checked_mul(4)?;
        let offset = usize::try_from(y)
            .ok()?
            .checked_mul(self.stride_bytes)?
            .checked_add(column)?;
        let pixel = self.bgrx.get(offset..offset.checked_add(4)?)?;
        Some(Rgb8 {
            r: pixel[2],
            g: pixel[1],
            b: pixel[0],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePoint {
    pub x: u32,
    pub y: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceView {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePixel {
    pub cache: CachePoint,
    pub source: ScreenPointPx,
    pub rgb: Rgb8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomScale {
    X4,
    X8,
    X16,
    X32,
}

impl ZoomScale {
    pub const fn factor(self) -> u32 {
        match self {
            Self::X4 => 4,
            Self::X8 => 8,
            Self::X16 => 16,
            Self::X32 => 32,
        }
    }

    pub const fn increase(self) -> Self {
        match self {
            Self::X4 => Self::X8,
            Self::X8 => Self::X16,
            Self::X16 | Self::X32 => Self::X32,
        }
    }

    /// `None` means the caller should discard the snapshot and return to Live.
    pub const fn decrease(self) -> Option<Self> {
        match self {
            Self::X4 => None,
            Self::X8 => Some(Self::X4),
            Self::X16 => Some(Self::X8),
            Self::X32 => Some(Self::X16),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomError {
    Image(ImageError),
    EmptyViewport,
    InvalidFocus,
}

#[derive(Debug)]
pub struct ZoomView {
    image: FrozenImage,
    viewport: ScreenRectPx,
    scale: ZoomScale,
    source_view: SourceView,
    drawn_rect: ScreenRectPx,
    selected: CachePoint,
}

impl ZoomView {
    /// `viewport` is only the image area; borders and the text bar are excluded.
    pub fn new(
        image: FrozenImage,
        viewport: ScreenRectPx,
        scale: ZoomScale,
        focus: CachePoint,
    ) -> Result<Self, ZoomError> {
        image.validate().map_err(ZoomError::Image)?;
        if viewport.is_empty() {
            return Err(ZoomError::EmptyViewport);
        }
        if focus.x >= image.width || focus.y >= image.height {
            return Err(ZoomError::InvalidFocus);
        }
        let (drawn_rect, width, height) = layout(&image, viewport, scale);
        Ok(Self {
            source_view: SourceView {
                x: centered_origin(focus.x, width, image.width),
                y: centered_origin(focus.y, height, image.height),
                width,
                height,
            },
            image,
            viewport,
            scale,
            drawn_rect,
            selected: focus,
        })
    }

    pub fn image(&self) -> &FrozenImage {
        &self.image
    }

    pub fn viewport(&self) -> ScreenRectPx {
        self.viewport
    }

    pub fn scale(&self) -> ZoomScale {
        self.scale
    }

    pub fn source_view(&self) -> SourceView {
        self.source_view
    }

    pub fn drawn_rect(&self) -> ScreenRectPx {
        self.drawn_rect
    }

    pub fn selected(&self) -> CachePoint {
        self.selected
    }

    /// The same mapping serves hover, confirmation and displayed source data.
    pub fn hit_test(&self, mouse_screen: ScreenPointPx) -> Option<SourcePixel> {
        if !self.drawn_rect.contains(mouse_screen) {
            return None;
        }
        let k = i64::from(self.scale.factor());
        let x = i64::from(self.source_view.x)
            + (i64::from(mouse_screen.x) - i64::from(self.drawn_rect.left)) / k;
        let y = i64::from(self.source_view.y)
            + (i64::from(mouse_screen.y) - i64::from(self.drawn_rect.top)) / k;
        let cache = CachePoint {
            x: u32::try_from(x).ok()?,
            y: u32::try_from(y).ok()?,
        };
        Some(SourcePixel {
            cache,
            source: ScreenPointPx {
                x: i32::try_from(i64::from(self.image.origin.x) + x).ok()?,
                y: i32::try_from(i64::from(self.image.origin.y) + y).ok()?,
            },
            rgb: self.image.pixel_at(cache.x, cache.y)?,
        })
    }

    /// Invalid movement preserves the last valid pixel for subsequent zooms.
    pub fn select_at(&mut self, mouse_screen: ScreenPointPx) -> Option<SourcePixel> {
        let pixel = self.hit_test(mouse_screen)?;
        self.selected = pixel.cache;
        Some(pixel)
    }

    /// Keep the cursor's source pixel under it when possible. Image bounds and
    /// centered whitespace take precedence when an exact anchor is impossible.
    /// The viewport and image allocation remain unchanged.
    pub fn change_scale(&mut self, next_scale: ZoomScale, mouse_screen: ScreenPointPx) {
        let anchor_screen = if let Some(pixel) = self.select_at(mouse_screen) {
            self.selected = pixel.cache;
            mouse_screen
        } else {
            self.cell_center(self.selected).unwrap_or(ScreenPointPx {
                x: (i64::from(self.viewport.left) + i64::from(self.viewport.width()) / 2) as i32,
                y: (i64::from(self.viewport.top) + i64::from(self.viewport.height()) / 2) as i32,
            })
        };
        let (drawn_rect, width, height) = layout(&self.image, self.viewport, next_scale);
        self.source_view = SourceView {
            x: anchored_origin(
                self.selected.x,
                anchor_screen.x,
                drawn_rect.left,
                width,
                self.image.width,
                next_scale,
            ),
            y: anchored_origin(
                self.selected.y,
                anchor_screen.y,
                drawn_rect.top,
                height,
                self.image.height,
                next_scale,
            ),
            width,
            height,
        };
        self.drawn_rect = drawn_rect;
        self.scale = next_scale;
    }

    /// Physical center of a visible cache cell, useful for selection borders.
    pub fn cell_center(&self, cache: CachePoint) -> Option<ScreenPointPx> {
        let x = cache.x.checked_sub(self.source_view.x)?;
        let y = cache.y.checked_sub(self.source_view.y)?;
        if x >= self.source_view.width || y >= self.source_view.height {
            return None;
        }
        let k = i64::from(self.scale.factor());
        Some(ScreenPointPx {
            x: i32::try_from(i64::from(self.drawn_rect.left) + i64::from(x) * k + k / 2).ok()?,
            y: i32::try_from(i64::from(self.drawn_rect.top) + i64::from(y) * k + k / 2).ok()?,
        })
    }
}

fn centered_origin(focus: u32, visible: u32, total: u32) -> u32 {
    focus.saturating_sub(visible / 2).min(total - visible)
}

fn anchored_origin(
    cache: u32,
    screen: i32,
    start: i32,
    visible: u32,
    total: u32,
    scale: ZoomScale,
) -> u32 {
    if visible == 0 {
        return cache;
    }
    let cell = ((i64::from(screen) - i64::from(start)) / i64::from(scale.factor()))
        .clamp(0, i64::from(visible) - 1);
    (i64::from(cache) - cell).clamp(0, i64::from(total - visible)) as u32
}

fn layout(
    image: &FrozenImage,
    viewport: ScreenRectPx,
    scale: ZoomScale,
) -> (ScreenRectPx, u32, u32) {
    let k = scale.factor();
    let width = image.width.min(viewport.width() / k);
    let height = image.height.min(viewport.height() / k);
    let drawn_width = width * k;
    let drawn_height = height * k;
    let left = i64::from(viewport.left) + i64::from((viewport.width() - drawn_width) / 2);
    let top = i64::from(viewport.top) + i64::from((viewport.height() - drawn_height) / 2);
    (
        ScreenRectPx {
            left: left as i32,
            top: top as i32,
            right: (left + i64::from(drawn_width)) as i32,
            bottom: (top + i64::from(drawn_height)) as i32,
        },
        width,
        height,
    )
}
