//! Voxel light values and the per-state light tables (ADR 0015).
//!
//! Light is derived data: a pure function of the blocks and of one sky top per column, computed
//! by `boxel_light` and never stored in [`ChunkData`](crate::ChunkData). This module holds the
//! value format and the registry lookups that the computation and the chunk light masks
//! ([`LightMasks`](crate::LightMasks)) need.
//!
//! The rules, all integer:
//! - Opacity 15 is opaque (light stops), 0 is clear (air, glass), 1..=14 is partial (leaves and
//!   water are 1). Entering a voxel costs `max(1, opacity)` levels.
//! - A voxel above the sky top of its column is direct sky (level 15). Any other non-opaque
//!   voxel takes, per channel, the max over its 6 neighbors of their level minus its cost.
//! - An emitter takes its own emission, even when it is opaque. Channels mix by per-channel max.

use crate::{BlockRegistry, BlockState};

/// Opacity of a block that stops light (and the highest opacity).
pub const OPAQUE_OPACITY: u8 = 15;

/// Highest light level of a channel.
pub const MAX_LIGHT: u8 = 15;

/// Light of one voxel (or one quad corner): sky in bits 12-15, red in 8-11, green in 4-7, blue
/// in 0-3. Registry emission uses the same layout with the sky nibble 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct LightValue(pub u16);

impl LightValue {
    /// No light.
    pub const DARK: LightValue = LightValue(0);
    /// Sky 15, no block light (open sky).
    pub const FULL_SKY: LightValue = LightValue(0xF000);
    /// Shift of the sky nibble.
    pub const SKY_SHIFT: u32 = 12;
    /// Shift of the red nibble.
    pub const RED_SHIFT: u32 = 8;
    /// Shift of the green nibble.
    pub const GREEN_SHIFT: u32 = 4;
    /// Shift of the blue nibble.
    pub const BLUE_SHIFT: u32 = 0;
    /// The three block channels (red, green, blue).
    pub const BLOCK_MASK: u16 = 0x0FFF;
    /// Shifts of the block channels, red first.
    pub const BLOCK_SHIFTS: [u32; 3] = [Self::RED_SHIFT, Self::GREEN_SHIFT, Self::BLUE_SHIFT];

    /// From four levels, each in `0..=15` (higher bits are dropped; debug-asserted).
    pub const fn new(sky: u8, red: u8, green: u8, blue: u8) -> Self {
        debug_assert!(sky <= MAX_LIGHT && red <= MAX_LIGHT && green <= MAX_LIGHT);
        debug_assert!(blue <= MAX_LIGHT);
        Self(
            ((sky as u16 & 15) << Self::SKY_SHIFT)
                | ((red as u16 & 15) << Self::RED_SHIFT)
                | ((green as u16 & 15) << Self::GREEN_SHIFT)
                | (blue as u16 & 15),
        )
    }
    /// Block light `[red, green, blue]` with sky 0.
    pub const fn from_rgb(rgb: [u8; 3]) -> Self {
        Self::new(0, rgb[0], rgb[1], rgb[2])
    }
    /// The level at nibble `shift` (one of the `*_SHIFT` constants).
    #[inline]
    pub const fn channel(self, shift: u32) -> u8 {
        ((self.0 >> shift) & 15) as u8
    }
    /// Sky level.
    pub const fn sky(self) -> u8 {
        self.channel(Self::SKY_SHIFT)
    }
    /// Red level.
    pub const fn red(self) -> u8 {
        self.channel(Self::RED_SHIFT)
    }
    /// Green level.
    pub const fn green(self) -> u8 {
        self.channel(Self::GREEN_SHIFT)
    }
    /// Blue level.
    pub const fn blue(self) -> u8 {
        self.channel(Self::BLUE_SHIFT)
    }
    /// `[red, green, blue]`.
    pub const fn rgb(self) -> [u8; 3] {
        [self.red(), self.green(), self.blue()]
    }
    /// The block channels only (sky 0).
    pub const fn block(self) -> Self {
        Self(self.0 & Self::BLOCK_MASK)
    }
    /// The same block light with sky `sky` (`0..=15`).
    pub const fn with_sky(self, sky: u8) -> Self {
        debug_assert!(sky <= MAX_LIGHT);
        Self((self.0 & Self::BLOCK_MASK) | ((sky as u16 & 15) << Self::SKY_SHIFT))
    }
    /// True when every channel is 0.
    pub const fn is_dark(self) -> bool {
        self.0 == 0
    }
    /// Per-channel max (how lights mix).
    pub const fn max(self, other: Self) -> Self {
        let mut out = 0u16;
        let mut shift = 0;
        while shift < 16 {
            let a = (self.0 >> shift) & 15;
            let b = (other.0 >> shift) & 15;
            out |= (if a > b { a } else { b }) << shift;
            shift += 4;
        }
        Self(out)
    }
}

/// Class bit: opacity 15.
pub(crate) const CLASS_OPAQUE: u8 = 1;
/// Class bit: opacity 1..=14.
pub(crate) const CLASS_PARTIAL: u8 = 1 << 1;
/// Class bit: emits light.
pub(crate) const CLASS_EMITS: u8 = 1 << 2;

/// Per-state light lookups, built once from a frozen [`BlockRegistry`] (`SoA`, indexed by
/// `BlockState.0`). States outside the registry read as air: opacity 0, no emission.
///
/// Light only: collision is not here (the physics derives it from `BlockDef::collision`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightTables {
    opacity: Vec<u8>,
    emission: Vec<LightValue>,
    class: Vec<u8>,
    max_partial: u8,
    any_emitter: bool,
    id: u64,
}

impl LightTables {
    /// The tables of `registry`.
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let n = registry.len();
        let states = (0..n).map(|i| BlockState(i as u16));
        let opacity: Vec<u8> = states.clone().map(|s| registry.light_opacity(s)).collect();
        let emission: Vec<LightValue> = states.clone().map(|s| registry.emission(s)).collect();
        let class: Vec<u8> = states
            .map(|s| {
                let i = s.0 as usize;
                let mut c = 0;
                if opacity[i] >= OPAQUE_OPACITY {
                    c |= CLASS_OPAQUE;
                } else if opacity[i] > 0 {
                    c |= CLASS_PARTIAL;
                }
                if !emission[i].is_dark() {
                    c |= CLASS_EMITS;
                }
                c
            })
            .collect();
        let max_partial = opacity
            .iter()
            .copied()
            .filter(|&o| o < OPAQUE_OPACITY)
            .max()
            .unwrap_or(0);
        let any_emitter = emission.iter().any(|e| !e.is_dark());
        // A fingerprint of the content, so a cache built from other tables can be detected.
        let id = {
            use std::hash::{BuildHasher, Hasher};
            let mut h = foldhash::fast::FixedState::with_seed(0x6c69_6768_7400).build_hasher();
            h.write(&opacity);
            h.write(&class);
            for e in &emission {
                h.write_u16(e.0);
            }
            h.finish()
        };
        Self {
            opacity,
            emission,
            class,
            max_partial,
            any_emitter,
            id,
        }
    }
    /// Number of states.
    pub fn len(&self) -> usize {
        self.opacity.len()
    }
    /// True if the tables have no state (never for a built registry, which has air).
    pub fn is_empty(&self) -> bool {
        self.opacity.is_empty()
    }
    /// Light opacity `0..=15` of `s`.
    #[inline]
    pub fn opacity(&self, s: BlockState) -> u8 {
        self.opacity.get(s.0 as usize).copied().unwrap_or(0)
    }
    /// Emission of `s` (sky 0).
    #[inline]
    pub fn emission(&self, s: BlockState) -> LightValue {
        self.emission
            .get(s.0 as usize)
            .copied()
            .unwrap_or(LightValue::DARK)
    }
    /// True if `s` stops direct sky (opacity above 0): the sky top of a column is its highest
    /// such voxel.
    #[inline]
    pub fn blocks_sky(&self, s: BlockState) -> bool {
        self.opacity(s) > 0
    }
    /// True if `s` is opaque to light (opacity 15).
    #[inline]
    pub fn is_opaque(&self, s: BlockState) -> bool {
        self.class(s) & CLASS_OPAQUE != 0
    }
    /// True if `s` emits light.
    #[inline]
    pub fn emits(&self, s: BlockState) -> bool {
        self.class(s) & CLASS_EMITS != 0
    }
    /// The highest opacity below 15 of any state (0 if there is none). When it is at most 1,
    /// every non-opaque voxel costs 1 level to enter, so the light computation needs no
    /// per-voxel opacity.
    pub fn max_partial_opacity(&self) -> u8 {
        self.max_partial
    }
    /// True if some state emits light.
    pub fn any_emitter(&self) -> bool {
        self.any_emitter
    }
    /// Fingerprint of the tables' content (equal tables, equal id).
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Class bits of `s` (`CLASS_*`).
    #[inline]
    pub(crate) fn class(&self, s: BlockState) -> u8 {
        self.class.get(s.0 as usize).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_value_layout() {
        let v = LightValue::new(15, 1, 2, 3);
        assert_eq!(v.0, 0xF123);
        assert_eq!((v.sky(), v.red(), v.green(), v.blue()), (15, 1, 2, 3));
        assert_eq!(v.rgb(), [1, 2, 3]);
        assert_eq!(v.block(), LightValue(0x0123));
        assert_eq!(v.with_sky(4), LightValue(0x4123));
        assert_eq!(LightValue::from_rgb([14, 11, 6]), LightValue(0x0EB6));
        assert_eq!(
            LightValue(0x1F0A).max(LightValue(0x2A3B)),
            LightValue(0x2F3B)
        );
        assert!(LightValue::DARK.is_dark());
        assert_eq!(LightValue::FULL_SKY.sky(), 15);
    }
}
