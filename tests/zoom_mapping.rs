use color_picker::core::color::Rgb8;
use color_picker::core::geometry::{ScreenPointPx, ScreenRectPx};
use color_picker::core::zoom::{
    CachePoint, FrozenImage, ImageError, ZoomError, ZoomScale, ZoomView,
};

const SCALES: [ZoomScale; 4] = [ZoomScale::X4, ZoomScale::X8, ZoomScale::X16, ZoomScale::X32];

fn fixture(width: u32, height: u32) -> FrozenImage {
    let stride_bytes = width as usize * 4 + 8;
    let mut bgrx = vec![0xff; stride_bytes * height as usize];
    for y in 0..height {
        for x in 0..width {
            let offset = y as usize * stride_bytes + x as usize * 4;
            bgrx[offset..offset + 4].copy_from_slice(&[(x ^ y) as u8, y as u8, x as u8, 0]);
        }
    }
    FrozenImage {
        origin: ScreenPointPx { x: -150, y: -250 },
        width,
        height,
        stride_bytes,
        bgrx,
    }
}

fn viewport() -> ScreenRectPx {
    ScreenRectPx {
        left: -800,
        top: -600,
        right: -477,
        bottom: -275,
    }
}

#[test]
fn bgrx_uses_rgb_channels_top_down_rows_and_stride() {
    let image = fixture(3, 2);
    assert_eq!(image.validate(), Ok(()));
    assert_eq!(image.pixel_at(2, 0), Some(Rgb8 { r: 2, g: 0, b: 2 }));
    assert_eq!(image.pixel_at(0, 1), Some(Rgb8 { r: 0, g: 1, b: 1 }));
    assert_eq!(image.pixel_at(2, 1), Some(Rgb8 { r: 2, g: 1, b: 3 }));
    assert_eq!(image.pixel_at(3, 0), None);
    assert_eq!(image.pixel_at(0, 2), None);
    assert_eq!(image.pixel_at(u32::MAX, u32::MAX), None);
}

#[test]
fn malformed_or_overflowing_buffers_are_rejected() {
    let mut image = fixture(3, 2);
    image.stride_bytes = 11;
    assert_eq!(image.validate(), Err(ImageError::InvalidStride));
    assert_eq!(image.pixel_at(0, 0), None);
    image.stride_bytes = usize::MAX;
    assert_eq!(image.validate(), Err(ImageError::TruncatedBuffer));
    assert_eq!(image.pixel_at(0, 0), None);
    image.stride_bytes = 20;
    image.bgrx.pop();
    assert_eq!(image.validate(), Err(ImageError::TruncatedBuffer));
    image.width = 0;
    assert_eq!(image.validate(), Err(ImageError::EmptyImage));
    let mut image = fixture(3, 2);
    image.origin.x = i32::MAX - 1;
    assert_eq!(image.validate(), Err(ImageError::CoordinateOverflow));
}

#[test]
fn every_source_pixel_maps_back_at_every_scale_including_cell_edges() {
    for scale in SCALES {
        let view =
            ZoomView::new(fixture(7, 5), viewport(), scale, CachePoint { x: 3, y: 2 }).unwrap();
        assert_eq!(
            (view.source_view().width, view.source_view().height),
            (7, 5)
        );
        let drawn = view.drawn_rect();
        let k = scale.factor() as i32;
        for y in 0..5 {
            for x in 0..7 {
                // Every physical pixel inside every cell, not only its center.
                for dy in 0..k {
                    for dx in 0..k {
                        let hit = view
                            .hit_test(ScreenPointPx {
                                x: drawn.left + x * k + dx,
                                y: drawn.top + y * k + dy,
                            })
                            .unwrap();
                        assert_eq!(
                            hit.cache,
                            CachePoint {
                                x: x as u32,
                                y: y as u32
                            }
                        );
                        assert_eq!(
                            hit.source,
                            ScreenPointPx {
                                x: -150 + x,
                                y: -250 + y
                            }
                        );
                        assert_eq!(
                            hit.rgb,
                            Rgb8 {
                                r: x as u8,
                                g: y as u8,
                                b: (x ^ y) as u8
                            }
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn cropped_large_image_maps_the_source_view_instead_of_the_window() {
    for scale in SCALES {
        let view = ZoomView::new(
            fixture(65, 65),
            viewport(),
            scale,
            CachePoint { x: 40, y: 45 },
        )
        .unwrap();
        let source = view.source_view();
        for y in source.y..source.y + source.height {
            for x in source.x..source.x + source.width {
                let cache = CachePoint { x, y };
                let center = view.cell_center(cache).unwrap();
                let hit = view.hit_test(center).unwrap();
                assert_eq!(hit.cache, cache);
                assert_eq!(
                    hit.source,
                    ScreenPointPx {
                        x: -150 + x as i32,
                        y: -250 + y as i32
                    }
                );
                assert_eq!(hit.rgb, view.image().pixel_at(x, y).unwrap());
            }
        }
        assert_eq!(view.drawn_rect().width(), source.width * scale.factor());
        assert_eq!(view.drawn_rect().height(), source.height * scale.factor());
        assert_eq!(
            view.drawn_rect().intersection(viewport()),
            Some(view.drawn_rect())
        );
    }
}

#[test]
fn whitespace_borders_and_text_bar_never_map_to_pixels() {
    for scale in SCALES {
        let view =
            ZoomView::new(fixture(3, 2), viewport(), scale, CachePoint { x: 1, y: 1 }).unwrap();
        let drawn = view.drawn_rect();
        assert_eq!(
            (viewport().width() - drawn.width()) / 2,
            (drawn.left - viewport().left) as u32
        );
        for point in [
            ScreenPointPx {
                x: drawn.left - 1,
                y: drawn.top,
            },
            ScreenPointPx {
                x: drawn.left,
                y: drawn.top - 1,
            },
            ScreenPointPx {
                x: drawn.right,
                y: drawn.top,
            },
            ScreenPointPx {
                x: drawn.left,
                y: drawn.bottom,
            },
            ScreenPointPx {
                x: viewport().left,
                y: viewport().top,
            },
            ScreenPointPx {
                x: drawn.left,
                y: viewport().bottom + 20,
            },
        ] {
            assert_eq!(view.hit_test(point), None, "{scale:?}: {point:?}");
        }
    }
}

#[test]
fn scale_changes_preserve_cursor_anchor_and_image_allocation() {
    let mut view = ZoomView::new(
        fixture(65, 65),
        viewport(),
        ZoomScale::X4,
        CachePoint { x: 32, y: 32 },
    )
    .unwrap();
    let anchor = ScreenPointPx { x: -650, y: -440 };
    let expected = view.hit_test(anchor).unwrap();
    let allocation = view.image().bgrx.as_ptr();
    for next in [
        ZoomScale::X8,
        ZoomScale::X16,
        ZoomScale::X32,
        ZoomScale::X16,
        ZoomScale::X8,
        ZoomScale::X4,
    ] {
        view.change_scale(next, anchor);
        assert_eq!(view.hit_test(anchor), Some(expected));
        assert_eq!(view.image().bgrx.as_ptr(), allocation);
        assert_eq!(view.viewport(), viewport());
    }
}

#[test]
fn invalid_cursor_anchors_to_the_last_valid_selection() {
    let mut view = ZoomView::new(
        fixture(65, 65),
        viewport(),
        ZoomScale::X8,
        CachePoint { x: 32, y: 32 },
    )
    .unwrap();
    let point = ScreenPointPx { x: -640, y: -430 };
    let selected = view.select_at(point).unwrap().cache;
    let old_center = view.cell_center(selected).unwrap();
    view.change_scale(ZoomScale::X16, ScreenPointPx { x: 900, y: 900 });
    assert_eq!(view.selected(), selected);
    assert_eq!(view.hit_test(old_center).unwrap().cache, selected);
    assert_eq!(view.select_at(ScreenPointPx { x: 900, y: 900 }), None);
    assert_eq!(view.selected(), selected);
}

#[test]
fn scale_changes_at_image_edges_keep_the_view_inside_the_cache() {
    for focus in [CachePoint { x: 0, y: 0 }, CachePoint { x: 64, y: 64 }] {
        let mut view = ZoomView::new(fixture(65, 65), viewport(), ZoomScale::X32, focus).unwrap();
        for scale in SCALES {
            view.change_scale(
                scale,
                ScreenPointPx {
                    x: i32::MIN,
                    y: i32::MIN,
                },
            );
            let source = view.source_view();
            assert!(source.x + source.width <= 65);
            assert!(source.y + source.height <= 65);
            assert_eq!(view.selected(), focus);
            assert_eq!(
                view.hit_test(view.cell_center(focus).unwrap())
                    .unwrap()
                    .cache,
                focus
            );
        }
    }
}

#[test]
fn a_viewport_smaller_than_one_cell_does_not_show_partial_pixels() {
    let tiny = ScreenRectPx {
        left: -1,
        top: -1,
        right: 2,
        bottom: 2,
    };
    let view = ZoomView::new(
        fixture(3, 2),
        tiny,
        ZoomScale::X4,
        CachePoint { x: 1, y: 1 },
    )
    .unwrap();
    assert!(view.drawn_rect().is_empty());
    assert_eq!(view.hit_test(ScreenPointPx { x: 0, y: 0 }), None);
    assert_eq!(view.cell_center(CachePoint { x: 1, y: 1 }), None);
}

#[test]
fn empty_viewports_and_invalid_focus_are_rejected() {
    for right in [-1, -2] {
        let empty = ScreenRectPx {
            left: -1,
            top: -1,
            right,
            bottom: 10,
        };
        assert!(matches!(
            ZoomView::new(
                fixture(3, 2),
                empty,
                ZoomScale::X4,
                CachePoint { x: 1, y: 1 }
            ),
            Err(ZoomError::EmptyViewport)
        ));
    }
    assert!(matches!(
        ZoomView::new(
            fixture(3, 2),
            viewport(),
            ZoomScale::X4,
            CachePoint { x: 3, y: 1 }
        ),
        Err(ZoomError::InvalidFocus)
    ));
}

#[test]
fn a_single_axis_without_a_complete_cell_has_no_hits() {
    for small in [
        ScreenRectPx {
            left: 0,
            top: 0,
            right: 3,
            bottom: 100,
        },
        ScreenRectPx {
            left: 0,
            top: 0,
            right: 100,
            bottom: 3,
        },
    ] {
        let view = ZoomView::new(
            fixture(3, 2),
            small,
            ZoomScale::X4,
            CachePoint { x: 1, y: 1 },
        )
        .unwrap();
        assert!(view.drawn_rect().is_empty());
        assert_eq!(view.hit_test(ScreenPointPx { x: 1, y: 1 }), None);
        assert_eq!(view.cell_center(CachePoint { x: 1, y: 1 }), None);
    }
}

#[test]
fn viewport_near_integer_limits_maps_without_overflow() {
    let huge = ScreenRectPx {
        left: i32::MIN,
        top: i32::MIN,
        right: i32::MAX,
        bottom: i32::MAX,
    };
    let view = ZoomView::new(
        fixture(3, 2),
        huge,
        ZoomScale::X32,
        CachePoint { x: 1, y: 1 },
    )
    .unwrap();
    let cache = CachePoint { x: 2, y: 1 };
    assert_eq!(
        view.hit_test(view.cell_center(cache).unwrap())
            .unwrap()
            .cache,
        cache
    );
}

#[test]
fn zoom_steps_are_bounded_and_lowest_step_exits() {
    assert_eq!(ZoomScale::X4.decrease(), None);
    assert_eq!(ZoomScale::X8.decrease(), Some(ZoomScale::X4));
    assert_eq!(ZoomScale::X16.decrease(), Some(ZoomScale::X8));
    assert_eq!(ZoomScale::X32.decrease(), Some(ZoomScale::X16));
    assert_eq!(ZoomScale::X4.increase(), ZoomScale::X8);
    assert_eq!(ZoomScale::X8.increase(), ZoomScale::X16);
    assert_eq!(ZoomScale::X16.increase(), ZoomScale::X32);
    assert_eq!(ZoomScale::X32.increase(), ZoomScale::X32);
}
