use color_picker::core::{
    geometry::{ScreenPointPx, ScreenRectPx},
    placement::place_preview,
};

#[test]
fn preview_prefers_below_right_with_a_physical_pixel_gap() {
    let work = ScreenRectPx {
        left: 0,
        top: 0,
        right: 200,
        bottom: 200,
    };
    assert_eq!(
        place_preview(ScreenPointPx { x: 50, y: 50 }, work, 40, 20, 8),
        Some(ScreenRectPx {
            left: 59,
            top: 59,
            right: 99,
            bottom: 79
        }),
    );
    assert_eq!(
        place_preview(ScreenPointPx { x: 195, y: 50 }, work, 40, 20, 8),
        Some(ScreenRectPx {
            left: 147,
            top: 59,
            right: 187,
            bottom: 79
        }),
    );
    assert_eq!(
        place_preview(ScreenPointPx { x: 195, y: 195 }, work, 40, 20, 8),
        Some(ScreenRectPx {
            left: 147,
            top: 167,
            right: 187,
            bottom: 187
        }),
    );
}

#[test]
fn clamping_keeps_the_preview_complete_and_avoids_the_sample() {
    let work = ScreenRectPx {
        left: 0,
        top: 0,
        right: 100,
        bottom: 100,
    };
    let point = ScreenPointPx { x: 50, y: 50 };
    assert_eq!(
        place_preview(point, work, 60, 20, 10),
        Some(ScreenRectPx {
            left: 40,
            top: 61,
            right: 100,
            bottom: 81
        }),
    );
    assert_eq!(place_preview(point, work, 60, 60, 10), None);
    assert_eq!(place_preview(point, work, 100, 100, 0), None);
}

#[test]
fn zero_gap_still_keeps_the_sampled_pixel_outside_the_preview() {
    let work = ScreenRectPx {
        left: 0,
        top: 0,
        right: 10,
        bottom: 10,
    };
    assert_eq!(
        place_preview(ScreenPointPx { x: 0, y: 0 }, work, 1, 1, 0),
        Some(ScreenRectPx {
            left: 1,
            top: 1,
            right: 2,
            bottom: 2
        }),
    );
    assert_eq!(
        place_preview(ScreenPointPx { x: 9, y: 9 }, work, 1, 1, 0),
        Some(ScreenRectPx {
            left: 8,
            top: 8,
            right: 9,
            bottom: 9
        }),
    );
}

#[test]
fn negative_monitor_and_taskbar_coordinates_keep_previews_in_work_area() {
    let work = ScreenRectPx {
        left: -1920,
        top: -200,
        right: 0,
        bottom: 840,
    };
    for point in [
        ScreenPointPx { x: -1920, y: -200 },
        ScreenPointPx { x: -1, y: 839 },
        ScreenPointPx { x: -400, y: 870 },
        ScreenPointPx { x: -1930, y: 300 },
    ] {
        let rect = place_preview(point, work, 200, 80, 12).unwrap();
        assert_eq!(rect.intersection(work), Some(rect));
        assert!(!rect.contains(point));
        assert_eq!((rect.width(), rect.height()), (200, 80));
    }
}

#[test]
fn empty_and_too_small_work_areas_do_not_produce_clipped_windows() {
    let work = ScreenRectPx {
        left: 0,
        top: 0,
        right: 10,
        bottom: 10,
    };
    let point = ScreenPointPx { x: 0, y: 0 };
    for (width, height) in [(0, 1), (1, 0), (11, 1), (1, 11), (u32::MAX, 1)] {
        assert_eq!(place_preview(point, work, width, height, 0), None);
    }
    assert_eq!(
        place_preview(point, ScreenRectPx { right: 0, ..work }, 1, 1, 0),
        None
    );
    assert_eq!(
        place_preview(point, ScreenRectPx { top: 20, ..work }, 1, 1, 0),
        None
    );
}

#[test]
fn integer_extremes_and_maximum_gaps_never_wrap_coordinates() {
    let work = ScreenRectPx {
        left: i32::MIN,
        top: i32::MIN,
        right: i32::MAX,
        bottom: i32::MAX,
    };
    for point in [
        ScreenPointPx {
            x: i32::MIN,
            y: i32::MIN,
        },
        ScreenPointPx {
            x: i32::MAX - 1,
            y: i32::MAX - 1,
        },
        ScreenPointPx {
            x: i32::MIN,
            y: i32::MAX - 1,
        },
        ScreenPointPx { x: 0, y: 0 },
    ] {
        for (width, height) in [(40, 20), (u32::MAX, 1), (1, u32::MAX)] {
            let rect = place_preview(point, work, width, height, u32::MAX).unwrap();
            assert_eq!(rect.intersection(work), Some(rect));
            assert_eq!((rect.width(), rect.height()), (width, height));
            assert!(!rect.contains(point));
        }
    }
    assert_eq!(
        place_preview(
            ScreenPointPx { x: 0, y: 0 },
            work,
            u32::MAX,
            u32::MAX,
            u32::MAX
        ),
        None
    );
}

#[test]
fn exhaustive_small_layouts_find_a_position_exactly_when_one_exists() {
    let work = ScreenRectPx {
        left: -3,
        top: -2,
        right: 5,
        bottom: 4,
    };
    for width in 0..=9 {
        for height in 0..=7 {
            for x in -4..=5 {
                for y in -3..=4 {
                    let point = ScreenPointPx { x, y };
                    let possible = width > 0
                        && height > 0
                        && (work.left..work.right).any(|left| {
                            (work.top..work.bottom).any(|top| {
                                let rect = ScreenRectPx {
                                    left,
                                    top,
                                    right: left + width as i32,
                                    bottom: top + height as i32,
                                };
                                rect.intersection(work) == Some(rect) && !rect.contains(point)
                            })
                        });
                    for gap in [0, 1, 20] {
                        let placed = place_preview(point, work, width, height, gap);
                        assert_eq!(
                            placed.is_some(),
                            possible,
                            "point={point:?} width={width} height={height} gap={gap}"
                        );
                        if let Some(rect) = placed {
                            assert_eq!(rect.intersection(work), Some(rect));
                            assert_eq!((rect.width(), rect.height()), (width, height));
                            assert!(!rect.contains(point));
                        }
                    }
                }
            }
        }
    }
}
