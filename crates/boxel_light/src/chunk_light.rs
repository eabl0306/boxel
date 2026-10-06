//! [`ChunkLight`]: a chunk's own 32^3 light, kept for gameplay queries.
use crate::PaddedLight;
use boxel_voxel::{CHUNK_VOLUME, LightValue, LocalPos};

/// Light of the 32^3 voxels of one chunk, indexed by [`LocalPos::index`], in the smallest of
/// three forms: uniform (0 bytes), sky only (4 bits per voxel, 16 KiB) or full (16 bits per
/// voxel, 64 KiB).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkLight {
    /// Every voxel has this light.
    Uniform(LightValue),
    /// No block light: the sky level of voxel `i` is nibble `i & 1` (low first) of byte
    /// `i >> 1`.
    SkyOnly(Box<[u8; CHUNK_VOLUME / 2]>),
    /// One [`LightValue`] (as `u16`) per voxel.
    Full(Box<[u16; CHUNK_VOLUME]>),
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
            ChunkLight::Full(values) => LightValue(values[i]),
        }
    }
    /// Heap bytes used.
    pub fn heap_bytes(&self) -> usize {
        match self {
            ChunkLight::Uniform(_) => 0,
            ChunkLight::SkyOnly(_) => CHUNK_VOLUME / 2,
            ChunkLight::Full(_) => CHUNK_VOLUME * 2,
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
}
