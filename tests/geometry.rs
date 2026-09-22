use color_picker::core::geometry::{
    ScreenPointPx, ScreenRectPx, freeze_rect, freeze_rect_for_view,
};

#[test]
fn rectangles_are_half_open_even_at_negative_coordinates() {
    let rect = ScreenRectPx {
        left: -1920,
        top: -200,
        right: 0,
        bottom: 880,
    };
    assert!(rect.contains(ScreenPointPx { x: -1920, y: -200 }));
    assert!(rect.contains(ScreenPointPx { x: -1, y: 879 }));
    assert!(!rect.contains(ScreenPointPx { x: 0, y: 0 }));
    assert!(!rect.contains(ScreenPointPx { x: -1, y: 880 }));
    assert!(!rect.contains(ScreenPointPx { x: -1921, y: 0 }));
    assert!(!rect.contains(ScreenPointPx { x: -1, y: -201 }));
    assert_eq!(rect.width(), 1920);
    assert_eq!(rect.height(), 1080);
}

#[test]
fn intersection_rejects_touching_edges_and_inverted_rectangles() {
    let rect = ScreenRectPx {
        left: -10,
        top: -10,
        right: 10,
        bottom: 10,
    };
    let overlapping = ScreenRectPx {
        left: 0,
        top: 0,
        right: 20,
        bottom: 20,
    };
    assert_eq!(
        rect.intersection(overlapping),
        Some(ScreenRectPx {
            left: 0,
            top: 0,
            right: 10,
            bottom: 10,
        })
    );
    assert_eq!(
        rect.intersection(ScreenRectPx {
            left: 10,
            ..overlapping
        }),
        None
    );
    let inverted = ScreenRectPx { left: 30, ..rect };
    assert!(inverted.is_empty());
    assert_eq!(inverted.area(), 0);
    assert_eq!(inverted.intersection(rect), None);
}

#[test]
fn extreme_dimensions_use_wide_intermediates() {
    let rect = ScreenRectPx {
        left: i32::MIN,
        top: i32::MIN,
        right: i32::MAX,
        bottom: i32::MAX,
    };
    assert_eq!(rect.width(), u32::MAX);
    assert_eq!(rect.height(), u32::MAX);
    assert_eq!(rect.area(), u64::from(u32::MAX).pow(2));
    assert_eq!(rect.intersection(rect), Some(rect));
}

#[test]
fn freezing_is_centered_then_clipped_to_one_monitor() {
    let monitor = ScreenRectPx {
        left: -1920,
        top: -100,
        right: 0,
        bottom: 980,
    };
    assert_eq!(
        freeze_rect(ScreenPointPx { x: -200, y: 200 }, monitor),
        Some(ScreenRectPx {
            left: -232,
            top: 168,
            right: -167,
            bottom: 233,
        })
    );
    let corner = freeze_rect(ScreenPointPx { x: -1920, y: -100 }, monitor).unwrap();
    assert_eq!(corner.width(), 33);
    assert_eq!(corner.height(), 33);
    assert_eq!(corner.left, -1920);
    assert_eq!(corner.top, -100);
    let last_pixel = freeze_rect(ScreenPointPx { x: -1, y: 979 }, monitor).unwrap();
    assert_eq!(last_pixel.width(), 33);
    assert_eq!(last_pixel.height(), 33);
    assert_eq!(last_pixel.right, 0);
    assert_eq!(last_pixel.bottom, 980);
    assert_eq!(freeze_rect(ScreenPointPx { x: 0, y: 200 }, monitor), None);
}

#[test]
fn freezing_near_integer_limits_never_wraps() {
    let monitor = ScreenRectPx {
        left: i32::MIN,
        top: i32::MIN,
        right: i32::MAX,
        bottom: i32::MAX,
    };
    let low = freeze_rect(
        ScreenPointPx {
            x: i32::MIN,
            y: i32::MIN,
        },
        monitor,
    )
    .unwrap();
    assert_eq!((low.width(), low.height()), (33, 33));
    let high = freeze_rect(
        ScreenPointPx {
            x: i32::MAX - 1,
            y: i32::MAX - 1,
        },
        monitor,
    )
    .unwrap();
    assert_eq!((high.width(), high.height()), (33, 33));
}

#[test]
fn planned_freeze_caches_the_source_view_needed_at_the_left_edge() {
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
    let capture = freeze_rect_for_view(point, monitor, viewport, 4).unwrap();
    assert_eq!((capture.width(), capture.height()), (65, 65));
    // The cursor is over cell 5, so source x=15 must start the 60-cell view.
    assert!(capture.left <= 15);
    assert!(capture.right >= 75);
    assert!(capture.contains(point));
    assert_eq!(capture.intersection(monitor), Some(capture));
}

#[test]
fn planned_freeze_keeps_full_size_at_all_negative_monitor_edges() {
    let monitor = ScreenRectPx {
        left: -1920,
        top: -1080,
        right: 0,
        bottom: 0,
    };
    for x in [-1920, -1919, -1900, -960, -21, -2, -1] {
        for y in [-1080, -1079, -1060, -540, -21, -2, -1] {
            let point = ScreenPointPx { x, y };
            let left = (x - 120).clamp(monitor.left, monitor.right - 240);
            let top = (y - 120).clamp(monitor.top, monitor.bottom - 240);
            let viewport = ScreenRectPx {
                left,
                top,
                right: left + 240,
                bottom: top + 240,
            };
            let capture = freeze_rect_for_view(point, monitor, viewport, 4).unwrap();
            assert_eq!((capture.width(), capture.height()), (65, 65));
            assert!(capture.contains(point));
            assert_eq!(capture.intersection(monitor), Some(capture));
            let source_left = x - (x - left) / 4;
            let source_top = y - (y - top) / 4;
            assert!(capture.left <= source_left && capture.right >= source_left + 60);
            assert!(capture.top <= source_top && capture.bottom >= source_top + 60);
        }
    }
}

#[test]
fn planned_freeze_falls_back_around_a_cursor_outside_the_drawn_area() {
    let monitor = ScreenRectPx {
        left: -1920,
        top: -1080,
        right: 0,
        bottom: 0,
    };
    let viewport = ScreenRectPx {
        left: -300,
        top: -300,
        right: 0,
        bottom: 0,
    };
    for point in [
        ScreenPointPx { x: -1, y: -1 }, // Inside viewport, outside centered cells.
        ScreenPointPx { x: -900, y: -500 },
        ScreenPointPx { x: -1920, y: -1080 },
    ] {
        let capture = freeze_rect_for_view(point, monitor, viewport, 4).unwrap();
        assert_eq!((capture.width(), capture.height()), (65, 65));
        assert!(capture.contains(point));
        assert_eq!(capture.intersection(monitor), Some(capture));
    }
}

#[test]
fn planned_freeze_handles_tiny_monitors_and_integer_limits() {
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
            right: i32::MIN + 1,
            bottom: i32::MIN + 1,
        },
        ScreenRectPx {
            left: i32::MAX - 1,
            top: i32::MAX - 1,
            right: i32::MAX,
            bottom: i32::MAX,
        },
        ScreenRectPx {
            left: i32::MIN,
            top: i32::MIN,
            right: i32::MAX,
            bottom: i32::MAX,
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
            let capture = freeze_rect_for_view(point, monitor, monitor, 4).unwrap();
            assert_eq!(capture.width(), monitor.width().min(65));
            assert_eq!(capture.height(), monitor.height().min(65));
            assert!(capture.contains(point));
            assert_eq!(capture.intersection(monitor), Some(capture));
        }
    }
}

#[test]
fn planned_freeze_rejects_invalid_point_viewport_and_scale() {
    let monitor = ScreenRectPx {
        left: 0,
        top: 0,
        right: 100,
        bottom: 100,
    };
    let point = ScreenPointPx { x: 20, y: 20 };
    assert_eq!(freeze_rect_for_view(point, monitor, monitor, 0), None);
    assert_eq!(
        freeze_rect_for_view(
            point,
            monitor,
            ScreenRectPx {
                right: 0,
                ..monitor
            },
            4
        ),
        None
    );
    assert_eq!(
        freeze_rect_for_view(ScreenPointPx { x: 100, y: 20 }, monitor, monitor, 4),
        None
    );
}
