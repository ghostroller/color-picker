use super::color::{Hsl, Rgb8};

/// UI display and clipboard output must share this formatter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorFormat {
    #[default]
    Hex,
    Rgb,
    CssRgb,
    Hsl,
}

impl ColorFormat {
    pub const ALL: [Self; 4] = [Self::Hex, Self::Rgb, Self::CssRgb, Self::Hsl];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Hex => "HEX",
            Self::Rgb => "RGB",
            Self::CssRgb => "CSS RGB",
            Self::Hsl => "HSL",
        }
    }
}

pub fn format_color(rgb: Rgb8, format: ColorFormat) -> String {
    let Rgb8 { r, g, b } = rgb;
    match format {
        ColorFormat::Hex => format!("#{r:02X}{g:02X}{b:02X}"),
        ColorFormat::Rgb => format!("{r}, {g}, {b}"),
        ColorFormat::CssRgb => format!("rgb({r} {g} {b})"),
        ColorFormat::Hsl => format_hsl(rgb.to_hsl()),
    }
}

fn format_hsl(hsl: Hsl) -> String {
    // Normalize after rounding as well: 359.96 degrees displays as 0, not 360.
    let hue = ((hsl.hue * 10.0).round() / 10.0).rem_euclid(360.0);
    format!(
        "hsl({} {}% {}%)",
        decimal(hue),
        decimal(hsl.saturation * 100.0),
        decimal(hsl.lightness * 100.0),
    )
}

fn decimal(value: f64) -> String {
    let formatted = format!("{value:.1}");
    formatted
        .strip_suffix(".0")
        .unwrap_or(&formatted)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_rounding_wraps_full_turn_to_zero() {
        assert_eq!(
            format_hsl(Hsl {
                hue: 359.96,
                saturation: 1.0,
                lightness: 0.5,
            }),
            "hsl(0 100% 50%)"
        );
    }
}
