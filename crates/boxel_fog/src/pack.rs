//! How the march stores its result in one `Rg32Uint` texel, mirrored by `fog.wgsl`:
//!
//! - `x`: the fog color per unit of opacity, 11-11-10 bits of `sqrt(color)` (red, green, blue
//!   from bit 0), each clamped to 0..1.
//! - `y`: four opacities (`1 - transmittance`), one byte of `sqrt(alpha)` each, one per pixel of
//!   the 2x2 block at half resolution (byte `k` for pixel `(k & 1, k >> 1)`; byte 0 only at full
//!   resolution).
//!
//! A byte of 0 is exactly no fog: the composite discards the pixel and leaves it untouched. The
//! square root spends the bits where the eye needs them (thin fog, dark colors). Rounding is
//! `floor(x + 0.5)` on both sides (WGSL's `round` rounds half to even).

/// Largest red and green code.
const MAX11: f32 = 2047.0;
/// Largest blue code.
const MAX10: f32 = 1023.0;

fn code(v: f32, max: f32) -> u32 {
    (v.clamp(0.0, 1.0).sqrt() * max + 0.5).floor() as u32
}

/// Packs a linear color (each channel clamped to 0..1).
pub fn encode_color(rgb: [f32; 3]) -> u32 {
    code(rgb[0], MAX11) | (code(rgb[1], MAX11) << 11) | (code(rgb[2], MAX10) << 22)
}

/// Unpacks a color.
pub fn decode_color(word: u32) -> [f32; 3] {
    let channel = |v: u32, max: f32| {
        let s = v as f32 / max;
        s * s
    };
    [
        channel(word & 0x7ff, MAX11),
        channel((word >> 11) & 0x7ff, MAX11),
        channel(word >> 22, MAX10),
    ]
}

/// Packs an opacity (clamped to 0..1) into a byte: 0 only for (almost) no fog.
pub fn encode_alpha(alpha: f32) -> u8 {
    code(alpha, 255.0) as u8
}

/// Unpacks an opacity.
pub fn decode_alpha(byte: u8) -> f32 {
    let s = f32::from(byte) / 255.0;
    s * s
}

/// Packs four opacities, byte `k` = `alphas[k]`.
pub fn encode_alphas(alphas: [f32; 4]) -> u32 {
    alphas
        .iter()
        .enumerate()
        .fold(0, |w, (k, &a)| w | (u32::from(encode_alpha(a)) << (8 * k)))
}

/// What the composite does with one pixel: `dst * (1 - alpha) + color * alpha`, linear, with the
/// color and the opacity of byte `k` of a texel; `None` when the byte is 0 (the pixel stays).
pub fn composite(texel: [u32; 2], k: usize, dst: [f32; 3]) -> Option<[f32; 3]> {
    let byte = ((texel[1] >> (8 * k)) & 0xff) as u8;
    if byte == 0 {
        return None;
    }
    let alpha = decode_alpha(byte);
    let color = decode_color(texel[0]);
    Some(std::array::from_fn(|c| {
        dst[c] * (1.0 - alpha) + color[c] * alpha
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_exactly_no_fog() {
        assert_eq!(encode_alpha(0.0), 0);
        assert_eq!(decode_alpha(0), 0.0);
        assert_eq!(encode_alpha(-1.0), 0);
        assert_eq!(composite([encode_color([1.0; 3]), 0], 0, [0.3; 3]), None);
        // The smallest opacity that survives: about (0.5 / 255)^2.
        assert_eq!(encode_alpha(3.0e-6), 0);
        assert_eq!(encode_alpha(5.0e-6), 1);
    }

    #[test]
    fn round_trips_are_close_and_idempotent() {
        for i in 0..=1000 {
            let a = i as f32 / 1000.0;
            let b = encode_alpha(a);
            let back = decode_alpha(b);
            // Half a code of sqrt(alpha): at most about 2 * 0.5 / 255 in alpha.
            assert!((back - a).abs() <= 0.004, "{a}: {back}");
            assert_eq!(encode_alpha(back), b, "idempotent at {a}");
        }
        for i in 0..=256 {
            let v = i as f32 / 256.0;
            let rgb = [v, 1.0 - v, (v * 7.0).fract()];
            let back = decode_color(encode_color(rgb));
            for c in 0..3 {
                assert!((back[c] - rgb[c]).abs() <= 0.002, "{rgb:?}: {back:?}");
            }
            assert_eq!(
                encode_color(back),
                encode_color(rgb),
                "idempotent at {rgb:?}"
            );
        }
        assert_eq!(
            decode_color(encode_color([2.0, -1.0, 0.25])),
            [1.0, 0.0, decode_color(encode_color([0.0, 0.0, 0.25]))[2]]
        );
    }

    #[test]
    fn four_bytes_one_per_pixel() {
        let w = encode_alphas([0.0, 1.0, 0.25, 0.5]);
        assert_eq!(w & 0xff, 0);
        assert_eq!((w >> 8) & 0xff, 255);
        assert_eq!((w >> 16) & 0xff, 128);
        let texel = [encode_color([1.0, 0.0, 0.0]), w];
        assert_eq!(composite(texel, 1, [0.0, 0.0, 1.0]), Some([1.0, 0.0, 0.0]));
        let half = composite(texel, 2, [0.0; 3]).expect("fog");
        assert!((half[0] - decode_alpha(128)).abs() < 1e-6);
    }
}
