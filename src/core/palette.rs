//! Three independent, scrollable OKLCH neighbourhoods around the current color.
//! Browsing never changes the current color; choosing a candidate starts a new
//! neighbourhood with the exact chosen RGB value at every row's centre.

use super::color::Rgb8;

pub const ROWS: usize = 3;
pub const COLUMNS: usize = 7;
pub const CENTER: usize = COLUMNS / 2;
pub const LIGHTNESS_ROW: usize = 0;
pub const HUE_ROW: usize = 1;
pub const CHROMA_ROW: usize = 2;

const LIGHTNESS_STEP: f64 = 0.03;
const HUE_STEP: f64 = 5.0;
const HUE_STEPS: i32 = 72;
// Hue becomes unstable close to the neutral axis. Do not amplify tiny RGB
// quantization differences into an arbitrary saturated hue when browsing.
const NEUTRAL_CHROMA: f64 = 0.005;
const GAMUT_EPSILON: f64 = 2.0e-7;

#[derive(Clone, Debug)]
pub struct Palette {
    current: Rgb8,
    anchor: Oklch,
    offsets: [i32; ROWS],
    candidates: [[Rgb8; COLUMNS]; ROWS],
    maximum_chroma: f64,
    chroma_step: f64,
}

impl Palette {
    pub fn new(color: Rgb8) -> Self {
        let anchor = Oklch::from_rgb(color);
        let maximum_chroma = maximum_chroma(anchor.lightness, anchor.hue).max(anchor.chroma);
        let mut palette = Self {
            current: color,
            anchor,
            offsets: [0; ROWS],
            candidates: [[color; COLUMNS]; ROWS],
            maximum_chroma,
            chroma_step: (maximum_chroma / 12.0).max(0.002),
        };
        for row in 0..ROWS {
            palette.rebuild(row);
        }
        palette
    }

    pub fn reset(&mut self, color: Rgb8) {
        *self = Self::new(color);
    }

    pub fn current(&self) -> Rgb8 {
        self.current
    }

    pub fn candidates(&self) -> &[[Rgb8; COLUMNS]; ROWS] {
        &self.candidates
    }

    pub fn candidate(&self, row: usize, column: usize) -> Option<Rgb8> {
        self.candidates.get(row)?.get(column).copied()
    }

    pub fn offsets(&self) -> [i32; ROWS] {
        self.offsets
    }

    /// Move only one row's candidate window. Positive steps increase that
    /// row's coordinate. Returns whether its visible RGB candidates changed.
    /// Lightness/chroma stop at their limits; hue wraps after a full turn.
    pub fn scroll(&mut self, row: usize, steps: i32) -> bool {
        if row >= ROWS || steps == 0 || (row != LIGHTNESS_ROW && self.is_neutral()) {
            return false;
        }
        let requested = i64::from(self.offsets[row]) + i64::from(steps);
        let next = match row {
            LIGHTNESS_ROW => requested.clamp(
                -(self.anchor.lightness / LIGHTNESS_STEP).ceil() as i64,
                ((1.0 - self.anchor.lightness) / LIGHTNESS_STEP).ceil() as i64,
            ) as i32,
            HUE_ROW => {
                ((requested + i64::from(HUE_STEPS / 2)).rem_euclid(i64::from(HUE_STEPS))
                    - i64::from(HUE_STEPS / 2)) as i32
            }
            CHROMA_ROW => requested.clamp(
                -(self.anchor.chroma / self.chroma_step).ceil() as i64,
                ((self.maximum_chroma - self.anchor.chroma) / self.chroma_step).ceil() as i64,
            ) as i32,
            _ => unreachable!(),
        };
        if next == self.offsets[row] {
            return false;
        }
        let previous = self.candidates[row];
        self.offsets[row] = next;
        self.rebuild(row);
        self.candidates[row] != previous
    }

    /// Select an existing candidate, then rebase every row on its exact RGB.
    /// Invalid cell indices have no effect.
    pub fn select(&mut self, row: usize, column: usize) -> Option<Rgb8> {
        let color = self.candidate(row, column)?;
        self.reset(color);
        Some(color)
    }

    fn is_neutral(&self) -> bool {
        self.anchor.chroma < NEUTRAL_CHROMA
    }

    fn rebuild(&mut self, row: usize) {
        self.candidates[row] = std::array::from_fn(|column| {
            let step = self.offsets[row] + column as i32 - CENTER as i32;
            // The reference cell must remain bit-exact rather than relying on
            // floating point conversion and gamut mapping to round-trip it.
            if step == 0 || (row != LIGHTNESS_ROW && self.is_neutral()) {
                return self.current;
            }
            let mut color = self.anchor;
            match row {
                LIGHTNESS_ROW => {
                    color.lightness =
                        (color.lightness + f64::from(step) * LIGHTNESS_STEP).clamp(0.0, 1.0);
                }
                HUE_ROW => color.hue = (color.hue + f64::from(step) * HUE_STEP).rem_euclid(360.0),
                CHROMA_ROW => {
                    color.chroma = (color.chroma + f64::from(step) * self.chroma_step)
                        .clamp(0.0, self.maximum_chroma);
                }
                _ => unreachable!(),
            }
            color.to_rgb()
        });
    }
}

#[derive(Clone, Copy, Debug)]
struct Oklch {
    lightness: f64,
    chroma: f64,
    hue: f64,
}

impl Oklch {
    fn from_rgb(rgb: Rgb8) -> Self {
        let [r, g, b] = [rgb.r, rgb.g, rgb.b].map(decode_srgb);
        // Björn Ottosson's 2021 linear-sRGB / Oklab matrices:
        // https://bottosson.github.io/posts/oklab/#converting-from-linear-srgb-to-oklab
        let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
        let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
        let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
        let lightness =
            (0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s).clamp(0.0, 1.0);
        let a = 1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s;
        let b = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s;
        let neutral = rgb.r == rgb.g && rgb.g == rgb.b;
        Self {
            lightness,
            chroma: if neutral { 0.0 } else { a.hypot(b) },
            hue: if neutral {
                0.0
            } else {
                b.atan2(a).to_degrees().rem_euclid(360.0)
            },
        }
    }

    fn linear_rgb(self) -> [f64; 3] {
        let hue = self.hue.to_radians();
        let a = self.chroma * hue.cos();
        let b = self.chroma * hue.sin();
        let l = (self.lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
        let m = (self.lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
        let s = (self.lightness - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
        [
            4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
            -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
            -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
        ]
    }

    fn to_rgb(self) -> Rgb8 {
        if self.lightness <= 0.0 {
            return Rgb8::new(0, 0, 0);
        }
        if self.lightness >= 1.0 {
            return Rgb8::new(255, 255, 255);
        }
        let [r, g, b] = self.gamut_mapped().linear_rgb().map(encode_srgb);
        Rgb8::new(r, g, b)
    }

    fn gamut_mapped(mut self) -> Self {
        if !in_gamut(self.linear_rgb()) {
            // Reduce chroma along a constant-L / constant-h ray. Clipping RGB
            // channels directly would introduce a hue/lightness shift instead.
            let mut lower = 0.0;
            let mut upper = self.chroma;
            for _ in 0..28 {
                let middle = (lower + upper) * 0.5;
                if in_gamut(
                    Self {
                        chroma: middle,
                        ..self
                    }
                    .linear_rgb(),
                ) {
                    lower = middle;
                } else {
                    upper = middle;
                }
            }
            self.chroma = lower;
        }
        self
    }
}

fn maximum_chroma(lightness: f64, hue: f64) -> f64 {
    if lightness <= 0.0 || lightness >= 1.0 {
        return 0.0;
    }
    // Every sRGB color lies below C=0.5 in OKLCH. Search its fixed-L/h slice.
    let mut lower = 0.0;
    let mut upper = 0.5;
    for _ in 0..28 {
        let middle = (lower + upper) * 0.5;
        if in_gamut(
            Oklch {
                lightness,
                chroma: middle,
                hue,
            }
            .linear_rgb(),
        ) {
            lower = middle;
        } else {
            upper = middle;
        }
    }
    lower
}

fn in_gamut(rgb: [f64; 3]) -> bool {
    rgb.into_iter().all(|channel| {
        channel.is_finite() && (-GAMUT_EPSILON..=1.0 + GAMUT_EPSILON).contains(&channel)
    })
}

fn decode_srgb(channel: u8) -> f64 {
    let value = f64::from(channel) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn encode_srgb(channel: f64) -> u8 {
    (encoded_srgb(channel) * 255.0).round() as u8
}

fn encoded_srgb(channel: f64) -> f64 {
    // Only numerical epsilon should remain outside the gamut after mapping.
    let value = channel.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_rgb_values_round_trip_through_oklch() {
        for r in (0..=255).step_by(17) {
            for g in (0..=255).step_by(17) {
                for b in (0..=255).step_by(17) {
                    let rgb = Rgb8::new(r, g, b);
                    assert_eq!(Oklch::from_rgb(rgb).to_rgb(), rgb, "{rgb:?}");
                }
            }
        }
        let red = Oklch::from_rgb(Rgb8::new(255, 0, 0));
        assert!((red.lightness - 0.627_955).abs() < 0.000_001);
        assert!((red.chroma - 0.257_683).abs() < 0.000_001);
        assert!((red.hue - 29.233_885).abs() < 0.000_001);
    }

    #[test]
    fn browsing_moves_only_the_requested_row_without_selecting() {
        let original = Rgb8::new(143, 185, 158);
        let mut palette = Palette::new(original);
        let before = *palette.candidates();
        assert!(palette.scroll(HUE_ROW, 4));
        assert_eq!(palette.current(), original);
        assert_eq!(palette.offsets(), [0, 4, 0]);
        assert_eq!(palette.candidates()[LIGHTNESS_ROW], before[LIGHTNESS_ROW]);
        assert_eq!(palette.candidates()[CHROMA_ROW], before[CHROMA_ROW]);
        assert_ne!(palette.candidates()[HUE_ROW], before[HUE_ROW]);
        assert!(palette.scroll(HUE_ROW, -4));
        assert_eq!(*palette.candidates(), before);
    }

    #[test]
    fn selection_rebases_every_row_and_retains_the_exact_selected_rgb() {
        let mut palette = Palette::new(Rgb8::new(143, 185, 158));
        palette.scroll(LIGHTNESS_ROW, 4);
        palette.scroll(HUE_ROW, -7);
        palette.scroll(CHROMA_ROW, 2);
        let selected = palette.candidate(HUE_ROW, 5).unwrap();
        assert_eq!(palette.select(HUE_ROW, 5), Some(selected));
        assert_eq!(palette.current(), selected);
        assert_eq!(palette.offsets(), [0; ROWS]);
        for row in 0..ROWS {
            assert_eq!(palette.candidate(row, CENTER), Some(selected));
        }
        let expected = Palette::new(selected);
        assert_eq!(palette.candidates(), expected.candidates());
        assert_eq!(palette.select(ROWS, 0), None);
        assert_eq!(palette.select(0, COLUMNS), None);
        assert!(!palette.scroll(ROWS, 1));
        assert_eq!(palette.current(), selected);
    }

    #[test]
    fn neutral_colors_never_gain_an_arbitrary_hue_or_chroma() {
        for neutral in (0..=255)
            .map(|gray| Rgb8::new(gray, gray, gray))
            .chain([Rgb8::new(128, 129, 128), Rgb8::new(240, 239, 240)])
        {
            let mut palette = Palette::new(neutral);
            for row in [HUE_ROW, CHROMA_ROW] {
                assert!(!palette.scroll(row, i32::MAX));
                assert_eq!(palette.candidates()[row], [neutral; COLUMNS]);
            }
            assert_eq!(palette.current(), neutral);
        }
        for gray in [0, 64, 128, 192, 255] {
            for color in Palette::new(Rgb8::new(gray, gray, gray)).candidates()[LIGHTNESS_ROW] {
                assert_eq!(color.r, color.g);
                assert_eq!(color.g, color.b);
            }
        }
    }

    #[test]
    fn extreme_wheel_deltas_are_bounded_and_hue_wraps() {
        for original in [
            Rgb8::new(0, 0, 0),
            Rgb8::new(255, 255, 255),
            Rgb8::new(255, 0, 0),
            Rgb8::new(0, 255, 0),
            Rgb8::new(0, 0, 255),
            Rgb8::new(1, 0, 0),
            Rgb8::new(254, 255, 254),
        ] {
            let mut palette = Palette::new(original);
            palette.scroll(LIGHTNESS_ROW, i32::MAX);
            assert_eq!(
                palette.candidate(LIGHTNESS_ROW, CENTER),
                Some(Rgb8::new(255, 255, 255))
            );
            assert!(!palette.scroll(LIGHTNESS_ROW, i32::MAX));
            palette.scroll(LIGHTNESS_ROW, i32::MIN);
            assert_eq!(
                palette.candidate(LIGHTNESS_ROW, CENTER),
                Some(Rgb8::new(0, 0, 0))
            );
            for row in [HUE_ROW, CHROMA_ROW] {
                palette.scroll(row, i32::MAX);
                palette.scroll(row, i32::MIN);
            }
            assert_eq!(palette.current(), original);
            palette.reset(original);
            let before = *palette.candidates();
            assert!(!palette.scroll(HUE_ROW, HUE_STEPS));
            assert_eq!(*palette.candidates(), before);
            assert_eq!(palette.offsets(), [0; ROWS]);
        }
    }

    #[test]
    fn gamut_mapping_keeps_lightness_and_hue_while_reducing_chroma() {
        for hue in (0..360).step_by(15) {
            for lightness in [0.1, 0.3, 0.5, 0.7, 0.9] {
                let target = Oklch {
                    lightness,
                    chroma: 0.5,
                    hue: f64::from(hue),
                };
                let mapped = target.gamut_mapped();
                assert_eq!(mapped.lightness, target.lightness);
                assert_eq!(mapped.hue, target.hue);
                assert!(in_gamut(mapped.linear_rgb()));
                assert!(mapped.chroma < target.chroma);
                // Quantization can rotate the measured hue substantially close
                // to black or the neutral axis. Verify the actual 8-bit rounding
                // contract instead: every stored channel is within half a code
                // value of the mapped floating-point encoded sRGB channel.
                let rgb = target.to_rgb();
                for (quantized, linear) in
                    [rgb.r, rgb.g, rgb.b].into_iter().zip(mapped.linear_rgb())
                {
                    let error = (f64::from(quantized) - encoded_srgb(linear) * 255.0).abs();
                    assert!(error <= 0.5 + 1.0e-9, "{target:?}: error {error}");
                }
            }
        }
    }
}
