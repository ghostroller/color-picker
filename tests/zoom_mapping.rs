use color_picker::core::color::Rgb8;
use color_picker::core::geometry::{ScreenPointPx, ScreenRectPx, freeze_rect_for_view};
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

fn planned_view(point: ScreenPointPx, monitor: ScreenRectPx, viewport: ScreenRectPx) -> ZoomView {
    let capture = freeze_rect_for_view(point, monitor, viewport, 4).unwrap();
    let mut image = fixture(capture.width(), capture.height());
    image.origin = ScreenPointPx {
        x: capture.left,
        y: capture.top,
    };
    let focus = CachePoint {
        x: (i64::from(point.x) - i64::from(capture.left)) as u32,
        y: (i64::from(point.y) - i64::from(capture.top)) as u32,
    };
    ZoomView::new_anchored(image, viewport, ZoomScale::X4, focus, point).unwrap()
}

#[test]
fn initial_left_edge_freeze_keeps_source_x20_under_the_cursor() {
    let monitor = ScreenRectPx {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };
    let viewport = ScreenRectPx {
        left: 0,
        top: 380,
        right: 240,
        bottom: 620,
    };
    let point = ScreenPointPx { x: 20, y: 500 };
    let mut view = planned_view(point, monitor, viewport);
    let hit = view.hit_test(point).unwrap();
    assert_eq!(hit.source, point);
    assert_eq!(hit.cache, view.selected());
    assert_eq!(view.image().width, 65);
    assert_eq!(view.source_view().width, 60);
    assert_eq!(view.select_at(point), Some(hit));
    // The first zoom uses that same selected source pixel.
    view.change_scale(ZoomScale::X8, point);
    assert_eq!(view.hit_test(point), Some(hit));
}

#[test]
fn initial_freeze_keeps_original_pixels_at_all_edges_across_dpi_sizes() {
    for monitor in [
        ScreenRectPx {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        },
        ScreenRectPx {
            left: -1920,
            top: -1080,
            right: 0,
            bottom: 0,
        },
        ScreenRectPx {
            left: -1920,
            top: -100,
            right: 0,
            bottom: 980,
        },
    ] {
        for dpi in [96, 120, 144, 168, 192, 240, 288] {
            let size = (240 * dpi + 48) / 96;
            for x in [
                monitor.left,
                monitor.left + 1,
                monitor.left + 20,
                monitor.left + 960,
                monitor.right - 21,
                monitor.right - 2,
                monitor.right - 1,
            ] {
                for y in [
                    monitor.top,
                    monitor.top + 1,
                    monitor.top + 20,
                    monitor.top + 540,
                    monitor.bottom - 21,
                    monitor.bottom - 2,
                    monitor.bottom - 1,
                ] {
                    let point = ScreenPointPx { x, y };
                    let left = (x - size / 2).clamp(monitor.left, monitor.right - size);
                    let top = (y - size / 2).clamp(monitor.top, monitor.bottom - size);
                    let viewport = ScreenRectPx {
                        left,
                        top,
                        right: left + size,
                        bottom: top + size,
                    };
                    let view = planned_view(point, monitor, viewport);
                    assert_eq!(
                        view.hit_test(point).unwrap().source,
                        point,
                        "{dpi} DPI: {point:?}"
                    );
                    let capture_size = (size as u32 / 4).max(65);
                    assert_eq!(
                        (view.image().width, view.image().height),
                        (capture_size, capture_size)
                    );
                    assert_eq!(view.drawn_rect(), viewport);
                    assert_eq!(view.hit_test(point).unwrap().cache, view.selected());
                }
            }
        }
    }
}

#[test]
fn capture_planning_matches_centered_cells_for_fractional_viewport_sizes() {
    let monitor = ScreenRectPx {
        left: -1000,
        top: -1000,
        right: 1000,
        bottom: 1000,
    };
    for size in [239, 241, 242, 243, 259, 261, 300, 419, 420, 421, 480] {
        let viewport = ScreenRectPx {
            left: -100,
            top: -100,
            right: -100 + size,
            bottom: -100 + size,
        };
        let padding = (size % 4) / 2;
        for point in [
            ScreenPointPx {
                x: -100 + padding,
                y: -100 + padding,
            },
            ScreenPointPx {
                x: -100 + size / 2,
                y: -100 + size / 2,
            },
            ScreenPointPx {
                x: -100 + size - padding - 2,
                y: -100 + size - padding - 2,
            },
        ] {
            let view = planned_view(point, monitor, viewport);
            assert_eq!(
                view.hit_test(point).unwrap().source,
                point,
                "{size}: {point:?}"
            );
        }
    }
}

#[test]
fn enlarged_dpi_views_keep_integer_zoom_and_the_original_cursor_anchor() {
    let monitor = ScreenRectPx {
        left: -3840,
        top: -2160,
        right: 0,
        bottom: 0,
    };
    for size in [300, 420, 480] {
        for point in [
            ScreenPointPx { x: -3820, y: -2140 },
            ScreenPointPx { x: -1920, y: -1080 },
            ScreenPointPx { x: -21, y: -21 },
        ] {
            let left = (point.x - size / 2).clamp(monitor.left, monitor.right - size);
            let top = (point.y - size / 2).clamp(monitor.top, monitor.bottom - size);
            let viewport = ScreenRectPx {
                left,
                top,
                right: left + size,
                bottom: top + size,
            };
            let mut view = planned_view(point, monitor, viewport);
            let original = view.hit_test(point).unwrap();
            let allocation = view.image().bgrx.as_ptr();
            assert_eq!(original.source, point);
            assert_eq!(view.drawn_rect(), viewport);
            for scale in [
                ZoomScale::X8,
                ZoomScale::X16,
                ZoomScale::X32,
                ZoomScale::X16,
                ZoomScale::X8,
                ZoomScale::X4,
            ] {
                view.change_scale(scale, point);
                assert_eq!(view.hit_test(point), Some(original), "{size}px {scale:?}");
                assert_eq!(view.image().bgrx.as_ptr(), allocation);
                assert_eq!(view.viewport(), viewport);
                let drawn = view.drawn_rect();
                assert_eq!(drawn.width() % scale.factor(), 0);
                assert_eq!(drawn.height() % scale.factor(), 0);
                assert_eq!(drawn.intersection(viewport), Some(drawn));
                let source = view.source_view();
                assert!(source.x + source.width <= view.image().width);
                assert!(source.y + source.height <= view.image().height);
                for cache in [
                    CachePoint {
                        x: source.x,
                        y: source.y,
                    },
                    CachePoint {
                        x: source.x + source.width - 1,
                        y: source.y + source.height - 1,
                    },
                ] {
                    let pixel = view.hit_test(view.cell_center(cache).unwrap()).unwrap();
                    assert_eq!(pixel.cache, cache);
                    assert_eq!(pixel.rgb, view.image().pixel_at(cache.x, cache.y).unwrap());
                }
            }
        }
    }
}

#[test]
fn anchored_initialization_clamps_small_caches_without_reselecting_focus() {
    for (width, height) in [(1, 1), (7, 5), (33, 65), (65, 33), (65, 65)] {
        for focus in [
            CachePoint { x: 0, y: 0 },
            CachePoint {
                x: width - 1,
                y: height - 1,
            },
        ] {
            for anchor in [
                ScreenPointPx { x: -800, y: -600 },
                ScreenPointPx { x: -640, y: -440 },
                ScreenPointPx {
                    x: i32::MIN,
                    y: i32::MAX,
                },
            ] {
                let view = ZoomView::new_anchored(
                    fixture(width, height),
                    viewport(),
                    ZoomScale::X4,
                    focus,
                    anchor,
                )
                .unwrap();
                let source = view.source_view();
                assert!(source.x + source.width <= width);
                assert!(source.y + source.height <= height);
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
}

#[test]
fn anchored_initialization_outside_drawn_area_uses_centered_fallback() {
    let focus = CachePoint { x: 20, y: 45 };
    let centered = ZoomView::new(fixture(65, 65), viewport(), ZoomScale::X8, focus).unwrap();
    let view = ZoomView::new_anchored(
        fixture(65, 65),
        viewport(),
        ZoomScale::X8,
        focus,
        ScreenPointPx {
            x: viewport().left - 1,
            y: viewport().top,
        },
    )
    .unwrap();
    assert_eq!(view.source_view(), centered.source_view());
    assert_eq!(view.drawn_rect(), centered.drawn_rect());
    assert_eq!(view.selected(), focus);
}

#[test]
fn planned_anchored_views_remain_valid_for_tiny_monitors_and_integer_limits() {
    for monitor in [
        ScreenRectPx {
            left: -7,
            top: -3,
            right: 5,
            bottom: 6,
        },
        ScreenRectPx {
            left: i32::MIN,
            top: i32::MIN,
            right: i32::MIN + 240,
            bottom: i32::MIN + 240,
        },
        ScreenRectPx {
            left: i32::MAX - 240,
            top: i32::MAX - 240,
            right: i32::MAX,
            bottom: i32::MAX,
        },
        ScreenRectPx {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        },
    ] {
        for point in [
            ScreenPointPx {
                x: monitor.left,
                y: monitor.top,
            },
            ScreenPointPx {
                x: monitor.right - 1,
                y: monitor.bottom - 1,
            },
        ] {
            let view = planned_view(point, monitor, monitor);
            let source = view.source_view();
            assert!(source.x + source.width <= view.image().width);
            assert!(source.y + source.height <= view.image().height);
            assert_eq!(view.image().validate(), Ok(()));
            if let Some(hit) = view.hit_test(point) {
                assert!(monitor.contains(hit.source));
            }
        }
    }
}
