//! [`PaddedLight`]: the light of a chunk and of its 1-voxel border, what the mesher reads.
use boxel_voxel::LightValue;

/// Padded edge: 32 + 2 border voxels (local `-1..=32`), as the mesher's `PaddedChunk`.
pub const PADDED: usize = 34;
/// `PADDED^2`.
pub const PADDED_AREA: usize = PADDED * PADDED;
/// `PADDED^3`.
pub const PADDED_VOLUME: usize = PADDED_AREA * PADDED;

/// Light of the padded chunk (local `x, y, z` in `-1..=32`), indexed like the mesher's
/// `PaddedChunk::index`: `(z + 1) + (x + 1) * 34 + (y + 1) * 34^2`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaddedLight {
    /// The same value everywhere (deep caves are `DARK`, open sky is `FULL_SKY`).
    Uniform(LightValue),
    /// One [`LightValue`] (as `u16`) per voxel.
    Full(Box<[u16; PADDED_VOLUME]>),
}

impl Default for PaddedLight {
    fn default() -> Self {
        PaddedLight::Uniform(LightValue::DARK)
    }
}

impl PaddedLight {
    /// Index of chunk-local `(x, y, z)`, each in `-1..=32` (the mesher's `PaddedChunk::index`).
    #[inline]
    pub const fn index(x: i32, y: i32, z: i32) -> usize {
        (z + 1) as usize + (x + 1) as usize * PADDED + (y + 1) as usize * PADDED_AREA
    }
    /// Light at chunk-local `(x, y, z)`, each in `-1..=32`.
    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> LightValue {
        match self {
            PaddedLight::Uniform(v) => *v,
            PaddedLight::Full(values) => LightValue(values[Self::index(x, y, z)]),
        }
    }
    /// `Some(value)` if uniform.
    pub fn as_uniform(&self) -> Option<LightValue> {
        match self {
            PaddedLight::Uniform(v) => Some(*v),
            PaddedLight::Full(_) => None,
        }
    }
    /// Every value in index order (`PADDED_VOLUME` of them).
    pub fn to_vec(&self) -> Vec<LightValue> {
        match self {
            PaddedLight::Uniform(v) => vec![*v; PADDED_VOLUME],
            PaddedLight::Full(values) => values.iter().map(|&v| LightValue(v)).collect(),
        }
    }
}

/// A zeroed `Full` buffer, allocated on the heap (78 KiB never touches the stack).
pub(crate) fn new_full() -> Box<[u16; PADDED_VOLUME]> {
    vec![0u16; PADDED_VOLUME]
        .into_boxed_slice()
        .try_into()
        .expect("PADDED_VOLUME entries")
}
