use color_picker::core::geometry::{ScreenPointPx, ScreenRectPx, freeze_rect};

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
