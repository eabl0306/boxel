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
//! - A filter (ADR 0060, colored glass) scales the brightness of each block channel entering its
//!   voxel: [`transmit`]`(l - cost, filter)`, with `filter` in fifteenths. Sky light ignores
//!   filters (it has one channel, so it stays white). A filter never raises a level, so light
//!   still falls by at least 1 per voxel.

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
/// Class bit: filters block light (not opaque, filter other than `[15; 3]`).
pub(crate) const CLASS_FILTER: u8 = 1 << 3;

/// The level `level` of a block channel after a filter that lets `filter` fifteenths of its
/// brightness through (ADR 0060): the level whose brightness is nearest `filter / 15` times
/// that of `level` (ties go down), on Minecraft's base curve `l / (60 - 3 l)` (the one
/// `boxel_render::light_curve` draws at gamma 0). So a filter scales the light a lamp gives, not
/// its level: half the brightness of level 14 is level 11, not 7. `transmit(l, 15) = l`,
/// `transmit(l, 0) = 0`, never above `level`, and non-decreasing in `level` (what keeps the
/// light's fixpoint unique). A table: exact integer math, the same on every machine.
#[inline]
pub const fn transmit(level: u16, filter: u8) -> u16 {
    debug_assert!(level <= MAX_LIGHT as u16 && filter <= MAX_LIGHT);
    TRANSMIT[(filter & 15) as usize][(level & 15) as usize] as u16
}

/// [`transmit`] by filter, then level.
const TRANSMIT: [[u8; 16]; 16] = {
    let mut table = [[0u8; 16]; 16];
    let mut f = 0;
    while f < 16 {
        let mut l = 0;
        while l < 16 {
            let mut best = 0;
            let mut k = 1;
            while k <= l {
                // Strictly nearer only: ties keep the lower level.
                if miss(k, l, f) * (60 - 3 * best) < miss(best, l, f) * (60 - 3 * k) {
                    best = k;
                }
                k += 1;
            }
            table[f as usize][l as usize] = best as u8;
            l += 1;
        }
        f += 1;
    }
    table
};

/// How far level `k`'s brightness `k / (60 - 3k)` is from `f / 15` of level `l`'s, times
/// `15 (60 - 3l) (60 - 3k)` (so the errors of two levels compare as `miss(k) (60 - 3j)` against
/// `miss(j) (60 - 3k)`).
const fn miss(k: i64, l: i64, f: i64) -> i64 {
    (k * 15 * (60 - 3 * l) - f * l * (60 - 3 * k)).abs()
}

/// Per-state light lookups, built once from a frozen [`BlockRegistry`] (`SoA`, indexed by
/// `BlockState.0`). States outside the registry read as air: opacity 0, no emission.
///
/// Light only: collision is not here (the physics derives it from `BlockDef::collision`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightTables {
    opacity: Vec<u8>,
    emission: Vec<LightValue>,
    /// Light filter of each state (`[15; 3]`: none).
    filter: Vec<[u8; 3]>,
    class: Vec<u8>,
    max_partial: u8,
    any_emitter: bool,
    any_filter: bool,
    id: u64,
}

impl LightTables {
    /// The tables of `registry`.
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let n = registry.len();
        let states = (0..n).map(|i| BlockState(i as u16));
        let opacity: Vec<u8> = states.clone().map(|s| registry.light_opacity(s)).collect();
        let emission: Vec<LightValue> = states.clone().map(|s| registry.emission(s)).collect();
        let filter: Vec<[u8; 3]> = states.clone().map(|s| registry.light_filter(s)).collect();
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
                if opacity[i] < OPAQUE_OPACITY && filter[i] != [MAX_LIGHT; 3] {
                    c |= CLASS_FILTER;
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
        let any_filter = class.iter().any(|c| c & CLASS_FILTER != 0);
        // A fingerprint of the content, so a cache built from other tables can be detected.
        let id = {
            use std::hash::{BuildHasher, Hasher};
            let mut h = foldhash::fast::FixedState::with_seed(0x6c69_6768_7400).build_hasher();
            h.write(&opacity);
            h.write(&class);
            for e in &emission {
                h.write_u16(e.0);
            }
            for f in &filter {
                h.write(f);
            }
            h.finish()
        };
        Self {
            opacity,
            emission,
            filter,
            class,
            max_partial,
            any_emitter,
            any_filter,
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
    /// Light filter of `s` in fifteenths per block channel (`[15; 3]`: none; ADR 0060).
    #[inline]
    pub fn light_filter(&self, s: BlockState) -> [u8; 3] {
        self.filter
            .get(s.0 as usize)
            .copied()
            .unwrap_or([MAX_LIGHT; 3])
    }
    /// True if `s` filters block light: not opaque, with a filter other than `[15; 3]`.
    #[inline]
    pub fn filters(&self, s: BlockState) -> bool {
        self.class(s) & CLASS_FILTER != 0
    }
    /// True if some state filters block light.
    pub fn any_filter(&self) -> bool {
        self.any_filter
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

    #[test]
    fn transmit_keeps_full_drops_none_and_never_raises() {
        for l in 0..=15u16 {
            assert_eq!(transmit(l, 15), l);
            assert_eq!(transmit(l, 0), 0);
            for f in 0..=15u8 {
                assert!(transmit(l, f) <= l);
                if l > 0 {
                    assert!(transmit(l, f) >= transmit(l - 1, f));
                }
            }
        }
        // Brightness, not level: 10/15 of level 14 (0.78) is level 12 (0.5 against 0.52), of
        // level 10 (1/3) exactly level 8 (2/9).
        assert_eq!(transmit(14, 10), 12);
        assert_eq!(transmit(10, 10), 8);
        assert_eq!(transmit(14, 3), 6);
        assert_eq!(transmit(1, 8), 1);
    }
}
