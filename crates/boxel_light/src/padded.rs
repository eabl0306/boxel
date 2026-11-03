//! [`PaddedLight`]: the light of a chunk and of its 1-voxel border, what the mesher reads.
use boxel_voxel::{LightValue, MAX_LIGHT};

/// Padded edge: 32 + 2 border voxels (local `-1..=32`), as the mesher's `PaddedChunk`.
pub const PADDED: usize = 34;
/// `PADDED^2`.
pub const PADDED_AREA: usize = PADDED * PADDED;
/// `PADDED^3`.
pub const PADDED_VOLUME: usize = PADDED_AREA * PADDED;

/// White colored sky in the layout of [`PaddedLight::Colored`]'s `sky_rgb`: 15 in each of the
/// three block nibbles.
pub const WHITE_SKY: u16 = 0x0FFF;

/// The colored sky value (`sky_rgb` layout: red, green and blue in the block nibbles of a
/// [`LightValue`], sky nibble 0) of an uncolored sky level: the level in all three channels.
#[inline]
pub const fn gray_sky(sky: u8) -> u16 {
    let s = sky as u16 & 15;
    (s << LightValue::RED_SHIFT) | (s << LightValue::GREEN_SHIFT) | (s << LightValue::BLUE_SHIFT)
}

/// Light of the padded chunk (local `x, y, z` in `-1..=32`), indexed like the mesher's
/// `PaddedChunk::index`: `(z + 1) + (x + 1) * 34 + (y + 1) * 34^2`.
///
/// Every form gives the light gameplay reads ([`PaddedLight::get`]): the sky as if no filter
/// tinted it (ADR 0064), and the block channels. The colored form adds the sky's three channels
/// for the picture ([`PaddedLight::sky_rgb`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaddedLight {
    /// The same value everywhere (deep caves are `DARK`, open sky is `FULL_SKY`).
    Uniform(LightValue),
    /// One [`LightValue`] (as `u16`) per voxel.
    Full(Box<[u16; PADDED_VOLUME]>),
    /// Colored sky (ADR 0064): `light` as in [`PaddedLight::Full`] (the uncolored sky and the
    /// block channels), and per voxel the sky's red, green and blue in the block nibbles of
    /// `sky_rgb` (sky nibble 0). Only a box where a filter tinted the sky of some padded voxel
    /// gives this form: where all three channels equal the uncolored sky, the result is `Full`
    /// or `Uniform`.
    Colored {
        /// The light gameplay reads, as in `Full`.
        light: Box<[u16; PADDED_VOLUME]>,
        /// Sky red, green and blue per voxel, in the block nibbles.
        sky_rgb: Box<[u16; PADDED_VOLUME]>,
    },
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
    /// Light at chunk-local `(x, y, z)`, each in `-1..=32`, with the uncolored sky (what
    /// gameplay reads, ADR 0064).
    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> LightValue {
        match self {
            PaddedLight::Uniform(v) => *v,
            PaddedLight::Full(values) | PaddedLight::Colored { light: values, .. } => {
                LightValue(values[Self::index(x, y, z)])
            }
        }
    }
    /// Sky `[red, green, blue]` at chunk-local `(x, y, z)`: the colored channels of a
    /// `Colored` result, the uncolored sky level three times otherwise.
    #[inline]
    pub fn sky_rgb(&self, x: i32, y: i32, z: i32) -> [u8; 3] {
        match self {
            PaddedLight::Colored { sky_rgb, .. } => LightValue(sky_rgb[Self::index(x, y, z)]).rgb(),
            _ => [self.get(x, y, z).sky(); 3],
        }
    }
    /// `Some(value)` if uniform.
    pub fn as_uniform(&self) -> Option<LightValue> {
        match self {
            PaddedLight::Uniform(v) => Some(*v),
            PaddedLight::Full(_) | PaddedLight::Colored { .. } => None,
        }
    }
    /// True for the colored form.
    pub fn is_colored(&self) -> bool {
        matches!(self, PaddedLight::Colored { .. })
    }
    /// Every value in index order (`PADDED_VOLUME` of them), with the uncolored sky.
    pub fn to_vec(&self) -> Vec<LightValue> {
        match self {
            PaddedLight::Uniform(v) => vec![*v; PADDED_VOLUME],
            PaddedLight::Full(values) | PaddedLight::Colored { light: values, .. } => {
                values.iter().map(|&v| LightValue(v)).collect()
            }
        }
    }
    /// Every sky `[red, green, blue]` in index order, as [`PaddedLight::sky_rgb`] gives them.
    pub fn to_sky_rgb_vec(&self) -> Vec<[u8; 3]> {
        match self {
            PaddedLight::Colored { sky_rgb, .. } => {
                sky_rgb.iter().map(|&v| LightValue(v).rgb()).collect()
            }
            _ => self.to_vec().into_iter().map(|v| [v.sky(); 3]).collect(),
        }
    }
}

/// `color` (sky channels in the block nibbles, as `sky_rgb`) after a light filter, per channel,
/// with no cost: what direct sky does under a filter (ADR 0064).
#[inline]
pub(crate) fn filter_sky(color: u16, filter: [u8; 3]) -> u16 {
    let mut out = 0;
    for (k, shift) in LightValue::BLOCK_SHIFTS.into_iter().enumerate() {
        out |= boxel_voxel::transmit((color >> shift) & 15, filter[k]) << shift;
    }
    out
}

/// True if every colored sky value of `sky_rgb` equals the uncolored sky of `light` in all three
/// channels (a colored result that needs no colored form).
pub(crate) fn sky_is_gray(light: &[u16], sky_rgb: &[u16]) -> bool {
    light
        .iter()
        .zip(sky_rgb)
        .all(|(&l, &c)| c == gray_sky(LightValue(l).sky()))
}

/// A zeroed `Full` buffer, allocated on the heap (78 KiB never touches the stack).
pub(crate) fn new_full() -> Box<[u16; PADDED_VOLUME]> {
    vec![0u16; PADDED_VOLUME]
        .into_boxed_slice()
        .try_into()
        .expect("PADDED_VOLUME entries")
}

const _: () = assert!(gray_sky(MAX_LIGHT) == WHITE_SKY);
