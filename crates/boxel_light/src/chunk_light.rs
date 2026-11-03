//! [`ChunkLight`]: a chunk's own 32^3 light, kept for gameplay queries.
use crate::PaddedLight;
use crate::padded::{PADDED_VOLUME, gray_sky};
use boxel_voxel::{CHUNK_VOLUME, LightValue, LocalPos};

/// Light of the 32^3 voxels of one chunk, indexed by [`LocalPos::index`], in the smallest of
/// four forms: uniform (0 bytes), sky only (4 bits per voxel, 16 KiB), full (16 bits per voxel,
/// 64 KiB) or colored (twice full, 128 KiB, only where a filter tinted the sky, ADR 0064).
///
/// [`ChunkLight::get`] gives the light gameplay reads, with the uncolored sky in every form;
/// [`ChunkLight::sky_rgb`] the sky's three channels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkLight {
    /// Every voxel has this light.
    Uniform(LightValue),
    /// No block light: the sky level of voxel `i` is nibble `i & 1` (low first) of byte
    /// `i >> 1`.
    SkyOnly(Box<[u8; CHUNK_VOLUME / 2]>),
    /// One [`LightValue`] (as `u16`) per voxel.
    Full(Box<[u16; CHUNK_VOLUME]>),
    /// Colored sky (ADR 0064), as [`PaddedLight::Colored`]: `light` as in `Full`, and the sky's
    /// red, green and blue in the block nibbles of `sky_rgb`.
    Colored {
        /// The light gameplay reads, as in `Full`.
        light: Box<[u16; CHUNK_VOLUME]>,
        /// Sky red, green and blue per voxel, in the block nibbles.
        sky_rgb: Box<[u16; CHUNK_VOLUME]>,
    },
}

/// The chunk's part (local `0..32`) of padded values, in [`LocalPos`] order.
fn inner(values: &[u16; PADDED_VOLUME]) -> Box<[u16; CHUNK_VOLUME]> {
    let mut out: Box<[u16; CHUNK_VOLUME]> = vec![0u16; CHUNK_VOLUME]
        .into_boxed_slice()
        .try_into()
        .expect("CHUNK_VOLUME entries");
    for (r, row) in out.as_chunks_mut::<32>().0.iter_mut().enumerate() {
        let start = PaddedLight::index((r & 31) as i32, (r >> 5) as i32, 0);
        row.copy_from_slice(&values[start..start + 32]);
    }
    out
}

impl Default for ChunkLight {
    fn default() -> Self {
        ChunkLight::Uniform(LightValue::DARK)
    }
}

impl ChunkLight {
    /// The chunk's part (local `0..32`) of a padded result, compressed. Scans the values
    /// first, so only the final form is allocated.
    pub fn from_padded(padded: &PaddedLight) -> Self {
        let values = match padded {
            PaddedLight::Uniform(v) => return ChunkLight::Uniform(*v),
            PaddedLight::Full(values) => values,
            PaddedLight::Colored { light, sky_rgb } => {
                // The tint may lie only in the border: then the chunk keeps a plain form.
                let sky = inner(sky_rgb);
                let light_in = inner(light);
                if light_in
                    .iter()
                    .zip(sky.iter())
                    .any(|(&l, &c)| c != gray_sky(LightValue(l).sky()))
                {
                    return ChunkLight::Colored {
                        light: light_in,
                        sky_rgb: sky,
                    };
                }
                light
            }
        };
        // One padded row (34 values along z) per (y, x): its inner 32, in `LocalPos` row order
        // (row `(y << 5) | x`).
        let row = |r: usize| {
            let start = PaddedLight::index((r & 31) as i32, (r >> 5) as i32, 0);
            &values[start..start + 32]
        };
        let rows = 0..CHUNK_VOLUME / 32;
        let first = row(0)[0];
        // One pass: the bits that differ from the first value, and the block bits.
        let (diff, block) = rows.clone().fold((0u16, 0u16), |(diff, block), r| {
            row(r)
                .iter()
                .fold((diff, block), |(d, b), &v| (d | (v ^ first), b | v))
        });
        if diff == 0 {
            return ChunkLight::Uniform(LightValue(first));
        }
        if block & LightValue::BLOCK_MASK == 0 {
            let mut sky: Box<[u8; CHUNK_VOLUME / 2]> = vec![0u8; CHUNK_VOLUME / 2]
                .into_boxed_slice()
                .try_into()
                .expect("CHUNK_VOLUME / 2 entries");
            for (r, out) in rows.zip(sky.as_chunks_mut::<16>().0) {
                for (byte, pair) in out.iter_mut().zip(row(r).as_chunks::<2>().0) {
                    *byte = LightValue(pair[0]).sky() | (LightValue(pair[1]).sky() << 4);
                }
            }
            return ChunkLight::SkyOnly(sky);
        }
        let mut full: Box<[u16; CHUNK_VOLUME]> = vec![0u16; CHUNK_VOLUME]
            .into_boxed_slice()
            .try_into()
            .expect("CHUNK_VOLUME entries");
        for (r, out) in rows.zip(full.as_chunks_mut::<32>().0) {
            out.copy_from_slice(row(r));
        }
        ChunkLight::Full(full)
    }
    /// Light at `p`.
    #[inline]
    pub fn get(&self, p: LocalPos) -> LightValue {
        let i = p.index();
        match self {
            ChunkLight::Uniform(v) => *v,
            ChunkLight::SkyOnly(sky) => {
                let byte = sky[i >> 1];
                let level = if i & 1 == 0 { byte & 15 } else { byte >> 4 };
                LightValue::new(level, 0, 0, 0)
            }
            ChunkLight::Full(values) | ChunkLight::Colored { light: values, .. } => {
                LightValue(values[i])
            }
        }
    }
    /// Sky `[red, green, blue]` at `p`: the colored channels of a `Colored` chunk, the
    /// uncolored sky level three times otherwise.
    #[inline]
    pub fn sky_rgb(&self, p: LocalPos) -> [u8; 3] {
        match self {
            ChunkLight::Colored { sky_rgb, .. } => LightValue(sky_rgb[p.index()]).rgb(),
            _ => [self.get(p).sky(); 3],
        }
    }
    /// True for the colored form.
    pub fn is_colored(&self) -> bool {
        matches!(self, ChunkLight::Colored { .. })
    }
    /// Heap bytes used.
    pub fn heap_bytes(&self) -> usize {
        match self {
            ChunkLight::Uniform(_) => 0,
            ChunkLight::SkyOnly(_) => CHUNK_VOLUME / 2,
            ChunkLight::Full(_) => CHUNK_VOLUME * 2,
            ChunkLight::Colored { .. } => CHUNK_VOLUME * 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::padded::new_full;

    #[test]
    fn from_padded_picks_the_smallest_form_and_reads_back() {
        assert_eq!(
            ChunkLight::from_padded(&PaddedLight::Uniform(LightValue::FULL_SKY)),
            ChunkLight::Uniform(LightValue::FULL_SKY)
        );
        // The border differs but the chunk itself is uniform.
        let mut values = new_full();
        values[PaddedLight::index(-1, 5, 5)] = 0x1234;
        let uniform = ChunkLight::from_padded(&PaddedLight::Full(values.clone()));
        assert_eq!(uniform, ChunkLight::Uniform(LightValue::DARK));

        let at = |x, y, z| LocalPos::new(x, y, z);
        let sky = |x: u32, y: u32, z: u32| ((x + 3 * y + 7 * z) % 16) as u16;
        for y in 0..32 {
            for x in 0..32 {
                for z in 0..32 {
                    values[PaddedLight::index(x as i32, y as i32, z as i32)] = sky(x, y, z) << 12;
                }
            }
        }
        let sky_only = ChunkLight::from_padded(&PaddedLight::Full(values.clone()));
        assert!(matches!(sky_only, ChunkLight::SkyOnly(_)));
        assert_eq!(sky_only.heap_bytes(), 16384);
        values[PaddedLight::index(31, 31, 30)] |= 0x0E00;
        let full = ChunkLight::from_padded(&PaddedLight::Full(values.clone()));
        assert!(matches!(full, ChunkLight::Full(_)));
        for (x, y, z) in [
            (0, 0, 0),
            (1, 0, 0),
            (0, 0, 1),
            (31, 31, 31),
            (7, 19, 4),
            (31, 31, 30),
        ] {
            let want = LightValue(values[PaddedLight::index(x as i32, y as i32, z as i32)]);
            assert_eq!(full.get(at(x, y, z)), want);
            assert_eq!(
                sky_only.get(at(x, y, z)),
                LightValue::new(sky(x, y, z) as u8, 0, 0, 0)
            );
        }
    }

    #[test]
    fn a_colored_result_keeps_its_tint_only_inside_the_chunk() {
        let sky_15 = || {
            let mut v = new_full();
            v.fill(LightValue::FULL_SKY.0);
            v
        };
        let gray = || {
            let mut v = new_full();
            v.fill(gray_sky(15));
            v
        };
        // A tint in the border only: the chunk is uniform open sky.
        let mut sky_rgb = gray();
        sky_rgb[PaddedLight::index(-1, 3, 3)] = 0x0D76;
        let border = PaddedLight::Colored {
            light: sky_15(),
            sky_rgb,
        };
        assert_eq!(border.sky_rgb(-1, 3, 3), [13, 7, 6]);
        assert_eq!(border.get(-1, 3, 3), LightValue::FULL_SKY);
        assert_eq!(
            ChunkLight::from_padded(&border),
            ChunkLight::Uniform(LightValue::FULL_SKY)
        );
        // Inside: the colored form, with the uncolored light for gameplay.
        let mut sky_rgb = gray();
        sky_rgb[PaddedLight::index(4, 5, 6)] = 0x0D76;
        let mut light = sky_15();
        light[PaddedLight::index(4, 5, 6)] = LightValue::new(15, 2, 0, 0).0;
        let inside = ChunkLight::from_padded(&PaddedLight::Colored { light, sky_rgb });
        assert!(inside.is_colored());
        assert_eq!(inside.heap_bytes(), 131072);
        let p = LocalPos::new(4, 5, 6);
        assert_eq!(inside.get(p), LightValue::new(15, 2, 0, 0));
        assert_eq!(inside.sky_rgb(p), [13, 7, 6]);
        assert_eq!(inside.sky_rgb(LocalPos::new(0, 0, 0)), [15; 3]);
        assert_eq!(
            ChunkLight::Uniform(LightValue::new(9, 0, 0, 0)).sky_rgb(p),
            [9; 3]
        );
    }
}
