/// An opaque, eight-bit RGB color sampled from the composed SDR desktop.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// HSL values before display rounding. Hue is in degrees; saturation and
/// lightness are fractions in the inclusive range 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hsl {
    pub hue: f64,
    pub saturation: f64,
    pub lightness: f64,
}

impl Rgb8 {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn to_hsl(self) -> Hsl {
        let r = f64::from(self.r) / 255.0;
        let g = f64::from(self.g) / 255.0;
        let b = f64::from(self.b) / 255.0;
        let maximum = r.max(g).max(b);
        let minimum = r.min(g).min(b);
        let delta = maximum - minimum;
        let lightness = (maximum + minimum) / 2.0;

        if delta == 0.0 {
            return Hsl {
                hue: 0.0,
                saturation: 0.0,
                lightness,
            };
        }

        let hue_sector = if maximum == r {
            ((g - b) / delta).rem_euclid(6.0)
        } else if maximum == g {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };

        Hsl {
            hue: (hue_sector * 60.0).rem_euclid(360.0),
            // Floating-point cancellation near black/white can otherwise put
            // a fully saturated color a few ulps above the mathematical 1.
            saturation: (delta / (1.0 - (2.0 * lightness - 1.0).abs())).clamp(0.0, 1.0),
            lightness,
        }
    }
}
