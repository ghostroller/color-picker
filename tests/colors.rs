use color_picker::core::{
    color::Rgb8,
    format::{ColorFormat, format_color},
};

#[test]
fn canonical_colors_have_exact_formats() {
    let cases = [
        (Rgb8::new(0, 0, 0), "#000000", "hsl(0 0% 0%)"),
        (Rgb8::new(255, 255, 255), "#FFFFFF", "hsl(0 0% 100%)"),
        (Rgb8::new(255, 0, 0), "#FF0000", "hsl(0 100% 50%)"),
        (Rgb8::new(0, 255, 0), "#00FF00", "hsl(120 100% 50%)"),
        (Rgb8::new(0, 0, 255), "#0000FF", "hsl(240 100% 50%)"),
        (Rgb8::new(128, 128, 128), "#808080", "hsl(0 0% 50.2%)"),
    ];
    for (rgb, hex, hsl) in cases {
        assert_eq!(format_color(rgb, ColorFormat::Hex), hex);
        assert_eq!(format_color(rgb, ColorFormat::Hsl), hsl);
    }
}

#[test]
fn hex_pads_every_channel_and_uses_uppercase() {
    assert_eq!(
        format_color(Rgb8::new(1, 10, 15), ColorFormat::Hex),
        "#010A0F"
    );
}

#[test]
fn rgb_formats_use_the_contractual_spacing() {
    let rgb = Rgb8::new(64, 158, 255);
    assert_eq!(format_color(rgb, ColorFormat::Rgb), "64, 158, 255");
    assert_eq!(format_color(rgb, ColorFormat::CssRgb), "rgb(64 158 255)");
    assert_eq!(format_color(rgb, ColorFormat::Hsl), "hsl(210.5 100% 62.5%)");
}

#[test]
fn gray_hue_and_saturation_are_always_zero() {
    for channel in 0..=255 {
        let hsl = Rgb8::new(channel, channel, channel).to_hsl();
        assert_eq!(hsl.hue, 0.0);
        assert_eq!(hsl.saturation, 0.0);
        assert!(hsl.lightness.is_finite());
    }
}

#[test]
fn secondary_hues_and_negative_red_sector_are_correct() {
    for (rgb, expected_hue) in [
        (Rgb8::new(255, 255, 0), 60.0),
        (Rgb8::new(0, 255, 255), 180.0),
        (Rgb8::new(255, 0, 255), 300.0),
        (Rgb8::new(255, 0, 128), 329.882_352_941_176_46),
    ] {
        assert!((rgb.to_hsl().hue - expected_hue).abs() < 1e-10);
    }
}

#[test]
fn hsl_rounding_is_limited_to_one_decimal() {
    assert_eq!(
        format_color(Rgb8::new(1, 2, 3), ColorFormat::Hsl),
        "hsl(210 50% 0.8%)"
    );
    assert_eq!(
        format_color(Rgb8::new(255, 0, 1), ColorFormat::Hsl),
        "hsl(359.8 100% 50%)"
    );
}

#[test]
fn conversion_stays_finite_and_in_range_at_channel_edges() {
    for r in [0, 1, 17, 127, 128, 254, 255] {
        for g in [0, 1, 17, 127, 128, 254, 255] {
            for b in [0, 1, 17, 127, 128, 254, 255] {
                let hsl = Rgb8::new(r, g, b).to_hsl();
                assert!((0.0..360.0).contains(&hsl.hue));
                assert!((0.0..=1.0).contains(&hsl.saturation));
                assert!((0.0..=1.0).contains(&hsl.lightness));
            }
        }
    }
}
