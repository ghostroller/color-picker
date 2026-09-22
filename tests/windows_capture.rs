#![cfg(windows)]

#[path = "support/pixel_fixture.rs"]
mod pixel_fixture;

use color_picker::{
    core::{
        color::Rgb8,
        geometry::{MAX_FREEZE_SIDE_PX, ScreenRectPx},
    },
    platform::windows::capture::{CaptureError, GdiSampler},
};
use pixel_fixture::{PixelFixture, STRIPE_TOP, ScopedPmv2, gdi_objects};

#[test]
#[ignore = "requires an unlocked interactive SDR desktop; displays its own small fixture window"]
fn screen_sampler_matches_known_pixels_and_releases_gdi_objects() {
    let _dpi = ScopedPmv2::enter().expect("PMv2 test thread context");
    let fixture = PixelFixture::new().expect("known physical pixel fixture");
    println!("fixture physical origin: {:?}", fixture.origin().unwrap());
    let mut sampler = GdiSampler::new().expect("GDI sampler");
    for (x, y) in [
        (0, 0),
        (1, 1),
        (2, 7),
        (64, 97),
        (127, 128),
        (255, 239),
        (256, 17),
        (383, 239),
    ] {
        let point = fixture.screen_point(x, y).unwrap();
        let expected = Rgb8::new((x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8);
        assert_eq!(
            sampler.sample_pixel(point).unwrap(),
            expected,
            "pattern at ({x}, {y}) / {point:?}"
        );
    }
    for (x, expected) in [
        (0, Rgb8::new(255, 0, 0)),
        (1, Rgb8::new(0, 255, 0)),
        (2, Rgb8::new(0, 0, 255)),
    ] {
        assert_eq!(
            sampler
                .sample_pixel(fixture.screen_point(x, STRIPE_TOP + 5).unwrap())
                .unwrap(),
            expected,
            "one-pixel RGB stripe {x}"
        );
    }

    let stationary = fixture.screen_point(101, 73).unwrap();
    let original = sampler.sample_pixel(stationary).unwrap();
    let origin = fixture.screen_point(90, 60).unwrap();
    let frozen = sampler
        .capture_rect(ScreenRectPx {
            left: origin.x,
            top: origin.y,
            right: origin.x + 65,
            bottom: origin.y + 65,
        })
        .unwrap();
    assert_eq!(
        (frozen.width, frozen.height, frozen.stride_bytes),
        (65, 65, 65 * 4)
    );
    for y in 0..65 {
        for x in 0..65 {
            assert_eq!(
                frozen.pixel_at(x, y),
                Some(Rgb8::new(
                    (90 + x) as u8,
                    (60 + y) as u8,
                    ((90 + x) ^ (60 + y)) as u8
                ))
            );
        }
    }
    for (width, height) in [(75, 75), (105, 105), (120, 120)] {
        let enlarged = sampler
            .capture_rect(ScreenRectPx {
                left: origin.x,
                top: origin.y,
                right: origin.x + width,
                bottom: origin.y + height,
            })
            .unwrap();
        assert_eq!(enlarged.width, width as u32);
        assert_eq!(enlarged.height, height as u32);
        assert_eq!(enlarged.stride_bytes, width as usize * 4);
        for y in 0..height as u32 {
            for x in 0..width as u32 {
                assert_eq!(
                    enlarged.pixel_at(x, y),
                    Some(Rgb8::new(
                        (90 + x) as u8,
                        (60 + y) as u8,
                        ((90 + x) ^ (60 + y)) as u8,
                    ))
                );
            }
        }
    }
    for (width, height) in [
        (MAX_FREEZE_SIDE_PX + 1, 1),
        (1, MAX_FREEZE_SIDE_PX + 1),
        (0, 1),
    ] {
        assert!(matches!(
            sampler.capture_rect(ScreenRectPx {
                left: origin.x,
                top: origin.y,
                right: origin.x + width as i32,
                bottom: origin.y + height as i32,
            }),
            Err(CaptureError::InvalidRectangle)
        ));
    }
    let updated = Rgb8::new(23, 211, 84);
    assert_ne!(original, updated);
    fixture.change_pixel(101, 73, updated).unwrap();
    assert_eq!(
        sampler.sample_pixel(stationary).unwrap(),
        updated,
        "sampling must refresh a fixed physical point after its content changes"
    );
    assert_eq!(
        frozen.pixel_at(11, 13),
        Some(original),
        "a frozen pixel must retain its value after the desktop changes"
    );
    drop(sampler);

    for _ in 0..20 {
        let mut sampler = GdiSampler::new().unwrap();
        assert_eq!(sampler.sample_pixel(stationary).unwrap(), updated);
    }
    let baseline = gdi_objects();
    assert!(
        baseline > 0,
        "GetGuiResources must report the fixture's GDI resources"
    );
    for _ in 0..100 {
        let mut sampler = GdiSampler::new().unwrap();
        assert_eq!(sampler.sample_pixel(stationary).unwrap(), updated);
    }
    let after = gdi_objects();
    println!("GDI objects after warmup={baseline}, after 100 sampler cycles={after}");
    assert!(
        after <= baseline,
        "GDI objects grew from {baseline} to {after}"
    );
}
