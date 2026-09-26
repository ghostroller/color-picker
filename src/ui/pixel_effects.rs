//! Integer BGRX backdrop effects, independent of native drawing and allocation.
#![forbid(unsafe_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PixelEffectError {
    EmptyImage,
    InvalidLength,
    InvalidRadius,
    InvalidTransparency,
}

/// Apply the existing two-pass box filter and dark tint, leaving X untouched.
/// Both slices must exactly cover width * height BGRX pixels. Scratch is reused.
pub(crate) fn blur_and_tint(
    pixels: &mut [u8],
    scratch: &mut [u8],
    width: usize,
    height: usize,
    radius: usize,
    transparency_percent: u8,
) -> Result<(), PixelEffectError> {
    if width == 0 || height == 0 {
        return Err(PixelEffectError::EmptyImage);
    }
    let len = width
        .checked_mul(height)
        .and_then(|len| len.checked_mul(4))
        .ok_or(PixelEffectError::InvalidLength)?;
    if pixels.len() != len || scratch.len() != len {
        return Err(PixelEffectError::InvalidLength);
    }
    let count = radius
        .checked_mul(2)
        .and_then(|value| value.checked_add(1))
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| value.checked_mul(255).is_some())
        .ok_or(PixelEffectError::InvalidRadius)?;
    if width.checked_add(radius).is_none() || height.checked_add(radius).is_none() {
        return Err(PixelEffectError::InvalidRadius);
    }
    if transparency_percent > 100 {
        return Err(PixelEffectError::InvalidTransparency);
    }

    let backdrop_opacity = (u32::from(transparency_percent) * 255 + 50) / 100;
    let tint_opacity = 255 - backdrop_opacity;
    // Sliding box filters preserve the original integer division at each pass.
    for y in 0..height {
        for channel in 0..3 {
            let at = |x: usize| pixels[(y * width + x) * 4 + channel] as u32;
            let mut sum = (radius as u32 + 1) * at(0);
            for x in 1..=radius {
                sum += at(x.min(width - 1));
            }
            for x in 0..width {
                scratch[(y * width + x) * 4 + channel] = (sum / count) as u8;
                sum -= at(x.saturating_sub(radius));
                sum += at((x + radius + 1).min(width - 1));
            }
        }
    }
    for x in 0..width {
        for (channel, tint) in [51_u32, 32, 23].into_iter().enumerate() {
            let at = |y: usize| scratch[(y * width + x) * 4 + channel] as u32;
            let mut sum = (radius as u32 + 1) * at(0);
            for y in 1..=radius {
                sum += at(y.min(height - 1));
            }
            for y in 0..height {
                let value = ((sum / count) * backdrop_opacity + tint * tint_opacity + 127) / 255;
                pixels[(y * width + x) * 4 + channel] = value as u8;
                sum -= at(y.saturating_sub(radius));
                sum += at((y + radius + 1).min(height - 1));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim algorithm from pre-refactor HEAD, retained only as a migration oracle.
    fn baseline_blur_and_tint(
        pixels: &mut [u8],
        scratch: &mut [u8],
        width: usize,
        height: usize,
        radius: usize,
        transparency_percent: u8,
    ) {
        let backdrop_opacity = (u32::from(transparency_percent) * 255 + 50) / 100;
        let tint_opacity = 255 - backdrop_opacity;
        // Sliding box filters keep the cost linear in this small information strip.
        let count = (2 * radius + 1) as u32;
        for y in 0..height {
            for channel in 0..3 {
                let at = |x: usize| pixels[(y * width + x) * 4 + channel] as u32;
                let mut sum = (radius as u32 + 1) * at(0);
                for x in 1..=radius {
                    sum += at(x.min(width - 1));
                }
                for x in 0..width {
                    scratch[(y * width + x) * 4 + channel] = (sum / count) as u8;
                    sum -= at(x.saturating_sub(radius));
                    sum += at((x + radius + 1).min(width - 1));
                }
            }
        }
        for x in 0..width {
            for (channel, tint) in [51_u32, 32, 23].into_iter().enumerate() {
                let at = |y: usize| scratch[(y * width + x) * 4 + channel] as u32;
                let mut sum = (radius as u32 + 1) * at(0);
                for y in 1..=radius {
                    sum += at(y.min(height - 1));
                }
                for y in 0..height {
                    // Transparency controls only the cached information backdrop;
                    // sampled image pixels, swatches and text remain fully opaque.
                    let value =
                        ((sum / count) * backdrop_opacity + tint * tint_opacity + 127) / 255;
                    pixels[(y * width + x) * 4 + channel] = value as u8;
                    sum -= at(y.saturating_sub(radius));
                    sum += at((y + radius + 1).min(height - 1));
                }
            }
        }
    }

    // Independent direct convolution reproduces the original edge replication,
    // horizontal truncation, vertical truncation and final integer tint rounding.
    fn reference(input: &[u8], width: usize, height: usize, radius: usize, percent: u8) -> Vec<u8> {
        let mut horizontal = input.to_vec();
        let mut output = input.to_vec();
        let count = (2 * radius + 1) as u32;
        for y in 0..height {
            for x in 0..width {
                for channel in 0..3 {
                    let sum: u32 = (0..=2 * radius)
                        .map(|offset| {
                            let xx = (x + offset).saturating_sub(radius).min(width - 1);
                            u32::from(input[(y * width + xx) * 4 + channel])
                        })
                        .sum();
                    horizontal[(y * width + x) * 4 + channel] = (sum / count) as u8;
                }
            }
        }
        let opacity = (u32::from(percent) * 255 + 50) / 100;
        for y in 0..height {
            for x in 0..width {
                for (channel, tint) in [51_u32, 32, 23].into_iter().enumerate() {
                    let sum: u32 = (0..=2 * radius)
                        .map(|offset| {
                            let yy = (y + offset).saturating_sub(radius).min(height - 1);
                            u32::from(horizontal[(yy * width + x) * 4 + channel])
                        })
                        .sum();
                    output[(y * width + x) * 4 + channel] =
                        ((sum / count * opacity + tint * (255 - opacity) + 127) / 255) as u8;
                }
            }
        }
        output
    }

    #[test]
    fn migrated_effect_matches_reference_byte_for_byte_including_x() {
        for (width, height) in [(1, 1), (1, 7), (7, 1), (2, 3), (12, 8)] {
            let input: Vec<u8> = (0..width * height * 4)
                .map(|index| ((index * 71 + index / 3 * 19) % 256) as u8)
                .collect();
            for radius in [0, 1, 3, 9, 17] {
                for percent in [0, 1, 35, 79, 80, 100] {
                    let mut pixels = input.clone();
                    let mut scratch = vec![137; input.len()];
                    blur_and_tint(&mut pixels, &mut scratch, width, height, radius, percent)
                        .unwrap();
                    let mut baseline = input.clone();
                    let mut baseline_scratch = vec![137; input.len()];
                    baseline_blur_and_tint(
                        &mut baseline,
                        &mut baseline_scratch,
                        width,
                        height,
                        radius,
                        percent,
                    );
                    assert_eq!(pixels, baseline);
                    assert_eq!(scratch, baseline_scratch);
                    assert_eq!(pixels, reference(&input, width, height, radius, percent));
                    assert!(scratch.chunks_exact(4).all(|pixel| pixel[3] == 137));
                }
            }
        }
    }

    #[test]
    fn invalid_input_does_not_modify_either_slice() {
        for (width, height, radius, percent, error) in [
            (0, 1, 1, 35, PixelEffectError::EmptyImage),
            (1, 0, 1, 35, PixelEffectError::EmptyImage),
            (usize::MAX, 2, 1, 35, PixelEffectError::InvalidLength),
            (2, 1, 1, 35, PixelEffectError::InvalidLength),
            (1, 1, usize::MAX, 35, PixelEffectError::InvalidRadius),
            (
                1,
                1,
                u32::MAX as usize / 255,
                35,
                PixelEffectError::InvalidRadius,
            ),
            (1, 1, 1, 101, PixelEffectError::InvalidTransparency),
        ] {
            let mut pixels = [1, 2, 3, 4];
            let mut scratch = [5, 6, 7, 8];
            assert_eq!(
                blur_and_tint(&mut pixels, &mut scratch, width, height, radius, percent),
                Err(error)
            );
            assert_eq!(pixels, [1, 2, 3, 4]);
            assert_eq!(scratch, [5, 6, 7, 8]);
        }
        assert_eq!(
            blur_and_tint(&mut [0; 4], &mut [0; 3], 1, 1, 1, 35),
            Err(PixelEffectError::InvalidLength)
        );
    }

    #[test]
    fn frosted_background_softens_edges_without_losing_tint_or_small_image_support() {
        for (percent, expected) in [
            (0, [51, 32, 23, 255]),
            (35, [122, 110, 104, 255]),
            (80, [214, 210, 209, 255]),
        ] {
            for (width, height) in [(1, 1), (1, 7), (7, 1), (12, 8)] {
                let mut pixels = vec![255; width * height * 4];
                let mut scratch = vec![0; pixels.len()];
                blur_and_tint(&mut pixels, &mut scratch, width, height, 9, percent).unwrap();
                for pixel in pixels.chunks_exact(4) {
                    assert_eq!(pixel, &expected);
                }
            }
        }
        let mut pixels = vec![0; 12 * 4];
        pixels[6 * 4..].fill(255);
        let mut scratch = vec![0; pixels.len()];
        blur_and_tint(&mut pixels, &mut scratch, 12, 1, 3, 35).unwrap();
        assert!(pixels[5 * 4] > pixels[0]);
        assert!(pixels[6 * 4] < pixels[11 * 4]);
        assert!(pixels[5 * 4] <= pixels[6 * 4]);
    }
}
