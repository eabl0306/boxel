//! The cave stage ([`Caves`], whose docs describe the density model).

use super::terrain::Heightmap;
use crate::Frequency;
use crate::noise::{self, Fbm, Fbm3, Frequency3};
use boxel_voxel::{BlockPos, CHUNK_AREA, Occupancy};

/// Voxels per lattice cell along x, y and z.
pub const CELL: [i64; 3] = [4, 8, 4];
/// Lattice corners of one chunk along x, y and z.
const CORNERS: [usize; 3] = [9, 5, 9];
/// Lattice corners of one chunk.
const CORNER_COUNT: usize = 9 * 5 * 9;
/// Corner columns of one chunk (`9 x 9`).
const COLUMN_COUNT: usize = 9 * 9;
/// Densities are clamped to `+-D_LIMIT` before quantization.
const D_LIMIT: f64 = 4.0;
/// Fixed-point scale of the quantized density: `2^20`.
const Q_SCALE: f64 = 1_048_576.0;
/// Sum of the trilinear weights of a voxel (`4 * 8 * 4`).
const WEIGHT_SUM: i64 = 128;

// Explicit salts, one per noise, XORed into `seed ^ CaveSettings::salt`. Never derived from
// the order of the stages or the types, so adding one never reshuffles another.
const SALT_CHEESE: u64 = 0x63a1_5e5c_8b2d_0f17;
const SALT_CHEESE_LAYER: u64 = 0x1f0c_d3a7_49e2_b6c5;
const SALT_SPAGHETTI_A: u64 = 0x5bd1_e995_7c3f_a20b;
const SALT_SPAGHETTI_B: u64 = 0xc2b2_ae3d_27d4_eb4f;
const SALT_SPAGHETTI_WIDTH: u64 = 0x1656_67b1_9e37_79f9;
const SALT_NOODLE_A: u64 = 0x85eb_ca77_c2b2_ae63;
const SALT_NOODLE_B: u64 = 0x27d4_eb2f_1656_67c5;
const SALT_NOODLE_WIDTH: u64 = 0xff51_afd7_ed55_8ccd;
const SALT_NOODLE_TOGGLE: u64 = 0xc4ce_b9fe_1a85_ec53;
const SALT_ENTRANCE_A: u64 = 0x9e37_79b9_7f4a_7c15;
const SALT_ENTRANCE_B: u64 = 0xbf58_476d_1ce4_e5b9;
const SALT_ENTRANCE_WIDTH: u64 = 0x94d0_49bb_1331_11eb;
const SALT_ENTRANCE_MASK: u64 = 0x2545_f491_4f6c_dd1d;

/// A linear fade by depth under the surface: `strength` at depths `<= full`, 0 at depths `>=
/// none`, linear between. Added to a cave type's density near the surface (positive: solid),
/// so that type stays under ground.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fade {
    /// Depth (voxels under the surface) down to which the whole `strength` applies.
    pub full: i64,
    /// Depth from which nothing applies.
    pub none: i64,
    /// Density added at full strength.
    pub strength: f64,
}

impl Fade {
    /// The fade at `depth` (exactly 0 from `none` down).
    fn at(self, depth: i64) -> f64 {
        if depth >= self.none {
            return 0.0;
        }
        if depth <= self.full {
            return self.strength;
        }
        // full < depth < none: both differences are positive and fit in i64.
        let t = (self.none - depth) as f64 / (self.none - self.full) as f64;
        self.strength * t
    }

    /// The deepest depth where the fade is not 0.
    fn reach(self) -> i64 {
        self.none
    }
}

/// Big caverns (Minecraft's "cheese" caves): air where the noise rises over a threshold.
///
/// `cheese = threshold + layer_weight * L^2 - n + fade(depth)`, with `n` the `noise` fbm and `L`
/// the `layer` fbm. `L` has a short wavelength along y and a long one across, so `L^2` is
/// small in horizontal sheets: caverns gather into floors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cheese {
    /// The cavern noise `n`.
    pub noise: Fbm3,
    /// Higher: fewer and smaller caverns.
    pub threshold: f64,
    /// The layer noise `L`.
    pub layer: Fbm3,
    /// Weight of `L^2` (0 turns the layers off).
    pub layer_weight: f64,
}

/// Tunnels as the intersection of two noise ridges (Minecraft's "spaghetti" and "noodle"
/// caves): air where both noises are near 0.
///
/// `tunnel = max(|a|, |b|) - width_here`, `width_here = (width + width_variation * w) * scale`,
/// with `a`, `b` and `w` three independent noises of the same fbm (`w` only when
/// `width_variation` is not 0), and `scale` in `[0, 1]` set by the caller (1 for spaghetti).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tunnels {
    /// The fbm of `a`, `b` and `w`.
    pub noise: Fbm3,
    /// Half-width of the tunnels in noise units.
    pub width: f64,
    /// How much `w` changes the width (0: constant width).
    pub width_variation: f64,
}

impl Tunnels {
    /// `max(|a|, |b|) - (width + width_variation * w) * scale` (`w` read only with a variation).
    fn density(&self, a: f64, b: f64, w: impl FnOnce() -> f64, scale: f64) -> f64 {
        let width = if self.width_variation == 0.0 {
            self.width
        } else {
            self.width + self.width_variation * w()
        };
        a.abs().max(b.abs()) - width * scale
    }
}

/// Thin tunnels (Minecraft's "noodle" caves), on only in some regions.
///
/// Where the `toggle` fbm is `> 0`, `noodles = tunnels` with `scale = min(toggle / toggle_ramp,
/// 1)` (they narrow to nothing at the region's edge), plus the fade; elsewhere the type is off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Noodles {
    /// The tunnels.
    pub tunnels: Tunnels,
    /// Regions with noodles: where this fbm is `> 0`.
    pub toggle: Fbm3,
    /// Toggle values from 0 to this one widen the noodles from 0 to full width.
    pub toggle_ramp: f64,
}

/// Steep tunnels that open the surface and join the caves under it.
///
/// On where the 2D `mask` fbm (value noise, per column) is above `mask_threshold` and the depth
/// is below `max_depth`, with the tunnels' `scale` the product of `min((mask - mask_threshold) /
/// mask_ramp, 1)` and `min((max_depth - depth) / depth_ramp, 1)`: they narrow to nothing at the
/// edge of their regions and at their bottom. They have no surface fade, so they break the
/// surface (unless the roof forbids it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entrances {
    /// The tunnels (a short wavelength along y makes them steep).
    pub tunnels: Tunnels,
    /// Regions with entrances.
    pub mask: Fbm,
    /// Entrances where `mask` is above this.
    pub mask_threshold: f64,
    /// Mask values from the threshold to `threshold + mask_ramp` widen them from 0 to full.
    pub mask_ramp: f64,
    /// Depth (voxels under the surface) where they end.
    pub max_depth: i64,
    /// They narrow over the last `depth_ramp` voxels above `max_depth`.
    pub depth_ramp: i64,
}

impl Entrances {
    /// `scale` of the tunnels at a corner of `depth` in a column with `mask`, `None` where the
    /// entrances are off.
    fn scale(&self, depth: i64, mask: f64) -> Option<f64> {
        if depth >= self.max_depth || mask <= self.mask_threshold {
            return None;
        }
        let across = ((mask - self.mask_threshold) / self.mask_ramp).min(1.0);
        // depth < max_depth: the difference is positive (saturated far above the surface).
        let down =
            (self.max_depth.saturating_sub(depth) as f64 / self.depth_ramp.max(1) as f64).min(1.0);
        Some(across * down)
    }
}

/// How caves keep away from the surface: a fade per type (added density near the surface) and
/// a hard roof.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceFade {
    /// Fade of the caverns.
    pub cheese: Fade,
    /// Fade of the spaghetti tunnels.
    pub spaghetti: Fade,
    /// Fade of the noodles.
    pub noodles: Fade,
    /// Voxels under the surface that are never carved: a voxel at height `y` of a column of
    /// height `h` is only carved when `y <= h - roof`. 0 lets caves open the surface; 1 or more
    /// keeps every cave closed (with `roof` blocks of ground over it).
    pub roof: i64,
}

/// Parameters of the [`Caves`] stage. The default is the Minecraft 1.18-like look (about 10% of
/// the volume is air, with openings to the surface); [`CaveSettings::closed`] has no openings.
///
/// Every field is a knob for the look; changing one changes the world (cave goldens are only
/// frozen once the look is approved).
#[derive(Clone, Debug, PartialEq)]
pub struct CaveSettings {
    /// Salt mixed (XOR) into the world seed for every cave noise (each noise also has its own
    /// constant salt): another salt gives other caves over the same terrain.
    pub salt: u64,
    /// Big caverns.
    pub cheese: Option<Cheese>,
    /// Long tunnels.
    pub spaghetti: Option<Tunnels>,
    /// Thin tunnels in some regions.
    pub noodles: Option<Noodles>,
    /// Openings to the surface.
    pub entrances: Option<Entrances>,
    /// How caves keep away from the surface.
    pub surface_fade: SurfaceFade,
    /// Subtracted from the combined density before the carve decision: 0 is the tuned look
    /// (about 10% of the deep volume is air), positive values open more air everywhere (wider
    /// tunnels, bigger caverns) and negative values close caves. It is a strong knob: measured
    /// deep, -0.1 leaves 1.3% air, -0.05 3%, 0.05 23% and 0.1 40%.
    pub density: f64,
}

/// An fbm with lacunarity 2 and gain 0.5, with the frequency `xz` across and `y` up.
const fn fbm3(xz: Frequency, y: Frequency, octaves: u32) -> Fbm3 {
    Fbm3 {
        frequency: Frequency3 { xz, y },
        octaves,
        lacunarity: 2,
        gain: 0.5,
    }
}

impl Default for CaveSettings {
    /// Cheese, spaghetti, noodles and entrances. Wavelengths in voxels, across by up: cheese
    /// 128 by 85 (3 octaves) in layers 512 by 32, spaghetti 93 by 64, noodles 32 in regions of
    /// 128, entrances 64 by 23 in regions of 256 (2D).
    ///
    /// Measured deep down, about 10% of the volume is air: caverns 6%, spaghetti 3% and
    /// noodles 1% (each alone; they overlap). About 0.3% of the surface columns open into a
    /// cave.
    fn default() -> Self {
        Self {
            salt: 0,
            cheese: Some(Cheese {
                noise: fbm3(Frequency::one_in(128), Frequency::new(3, 256), 3),
                threshold: 0.23,
                layer: fbm3(Frequency::one_in(512), Frequency::one_in(32), 1),
                layer_weight: 1.0,
            }),
            spaghetti: Some(Tunnels {
                noise: fbm3(Frequency::new(11, 1024), Frequency::one_in(64), 1),
                width: 0.068,
                width_variation: 0.02,
            }),
            noodles: Some(Noodles {
                tunnels: Tunnels {
                    noise: fbm3(Frequency::one_in(32), Frequency::one_in(32), 1),
                    width: 0.1,
                    width_variation: 0.0,
                },
                toggle: fbm3(Frequency::one_in(128), Frequency::one_in(128), 1),
                toggle_ramp: 0.1,
            }),
            entrances: Some(Entrances {
                tunnels: Tunnels {
                    noise: fbm3(Frequency::one_in(64), Frequency::new(11, 256), 1),
                    width: 0.1,
                    width_variation: 0.0,
                },
                mask: Fbm {
                    frequency: Frequency::one_in(256),
                    octaves: 2,
                    lacunarity: 2,
                    gain: 0.5,
                },
                mask_threshold: 0.36,
                mask_ramp: 0.1,
                max_depth: 64,
                depth_ramp: 16,
            }),
            surface_fade: SurfaceFade {
                cheese: Fade {
                    full: 8,
                    none: 24,
                    strength: 1.5,
                },
                spaghetti: Fade {
                    full: 4,
                    none: 12,
                    strength: 1.0,
                },
                noodles: Fade {
                    full: 2,
                    none: 8,
                    strength: 1.0,
                },
                roof: 0,
            },
            density: 0.0,
        }
    }
}

impl CaveSettings {
    /// The default caves with no openings: no entrances and a roof of 8 voxels, so no cave is
    /// visible from the surface (the worst case for occlusion culling).
    pub fn closed() -> Self {
        let default = Self::default();
        Self {
            entrances: None,
            surface_fade: SurfaceFade {
                roof: 8,
                ..default.surface_fade
            },
            ..default
        }
    }

    /// Depth under the lowest column from which no depth rule applies: every fade is 0, the
    /// entrances are off and the roof is above. Chunks wholly below it need no heights.
    pub(crate) fn surface_reach(&self) -> i64 {
        let f = &self.surface_fade;
        let mut reach = f.roof;
        if self.cheese.is_some() {
            reach = reach.max(f.cheese.reach());
        }
        if self.spaghetti.is_some() {
            reach = reach.max(f.spaghetti.reach());
        }
        if self.noodles.is_some() {
            reach = reach.max(f.noodles.reach());
        }
        if let Some(e) = &self.entrances {
            reach = reach.max(e.max_depth);
        }
        reach
    }
}

/// The noises of a corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Noise {
    Cheese,
    CheeseLayer,
    SpaghettiA,
    SpaghettiB,
    SpaghettiWidth,
    NoodleA,
    NoodleB,
    NoodleWidth,
    NoodleToggle,
    EntranceA,
    EntranceB,
    EntranceWidth,
}

/// Number of [`Noise`]s.
const NOISE_COUNT: usize = 12;

/// The caves stage: a pure function of `(seed, voxel)` with a point form ([`Caves::carves`],
/// the spec) and a bulk form ([`Caves::chunk_corners`] and [`carve_mask`]) that gives the same
/// voxels, faster.
///
/// # Density model
///
/// The density `D` (positive: solid) is sampled at a world-anchored lattice with one corner every
/// 4 voxels along x and z and every 8 along y ([`CELL`]), so a chunk has `9 x 5 x 9 = 405`
/// corners and shares the ones on its faces with its neighbors (no seams). At each corner:
///
/// - `D = min(cheese, spaghetti, noodles, entrance) - density`, over the cave types that are on
///   at that corner (a type that is off is left out, not counted as 0; with none, the corner is
///   solid). Each type is a pure function of the corner and of the height of its column
///   ([`CaveSettings`] has the formulas).
/// - `q = round(clamp(D, -4, 4) * 2^20)`, an `i32`.
///
/// A voxel at offsets `(fx, fy, fz)` inside its cell (`fx, fz` in `0..4`, `fy` in `0..8`) gets
/// the integer trilinear sum `S = sum(wx * wy * wz * q)` over the cell's 8 corners, with weights
/// `4 - fx` and `fx` along x (the same along z) and `8 - fy` and `fy` along y. The weights sum to
/// 128, so `|S| < 2^30`, and the sum is exact. The voxel is carved (air) iff `S < 0`, and it is
/// under the surface roof ([`SurfaceFade::roof`]).
///
/// The decision is integer arithmetic on integers that the `f64` noise produced through a fixed
/// sequence of `+ - * /`: every platform carves the same voxels. The interpolation is a convex
/// mix of the corners, so a cell whose 8 corners are `>= 0` is solid and one whose corners are
/// all `< 0` is air, exactly: the bulk path fills such cells whole.
#[derive(Clone, Debug, PartialEq)]
pub struct Caves {
    /// Parameters.
    pub settings: CaveSettings,
}

/// The column inputs of a corner: its depth under the surface and the entrance mask of its
/// column.
#[derive(Clone, Copy, Debug)]
struct Column {
    /// `height - y`, saturating; `i64::MAX` stands for "below every depth rule" (deep chunks).
    depth: i64,
    /// The entrance mask of the column (0 when there are no entrances).
    mask: f64,
}

impl Caves {
    /// The seed and fbm of one noise.
    fn noise(&self, seed: u64, n: Noise) -> (u64, Fbm3) {
        let s = &self.settings;
        let unset = || -> Fbm3 { unreachable!("noise of a cave type that is off") };
        let (salt, fbm) = match n {
            Noise::Cheese => (SALT_CHEESE, s.cheese.map_or_else(unset, |c| c.noise)),
            Noise::CheeseLayer => (SALT_CHEESE_LAYER, s.cheese.map_or_else(unset, |c| c.layer)),
            Noise::SpaghettiA => (
                SALT_SPAGHETTI_A,
                s.spaghetti.map_or_else(unset, |t| t.noise),
            ),
            Noise::SpaghettiB => (
                SALT_SPAGHETTI_B,
                s.spaghetti.map_or_else(unset, |t| t.noise),
            ),
            Noise::SpaghettiWidth => (
                SALT_SPAGHETTI_WIDTH,
                s.spaghetti.map_or_else(unset, |t| t.noise),
            ),
            Noise::NoodleA => (
                SALT_NOODLE_A,
                s.noodles.map_or_else(unset, |n| n.tunnels.noise),
            ),
            Noise::NoodleB => (
                SALT_NOODLE_B,
                s.noodles.map_or_else(unset, |n| n.tunnels.noise),
            ),
            Noise::NoodleWidth => (
                SALT_NOODLE_WIDTH,
                s.noodles.map_or_else(unset, |n| n.tunnels.noise),
            ),
            Noise::NoodleToggle => (
                SALT_NOODLE_TOGGLE,
                s.noodles.map_or_else(unset, |n| n.toggle),
            ),
            Noise::EntranceA => (
                SALT_ENTRANCE_A,
                s.entrances.map_or_else(unset, |e| e.tunnels.noise),
            ),
            Noise::EntranceB => (
                SALT_ENTRANCE_B,
                s.entrances.map_or_else(unset, |e| e.tunnels.noise),
            ),
            Noise::EntranceWidth => (
                SALT_ENTRANCE_WIDTH,
                s.entrances.map_or_else(unset, |e| e.tunnels.noise),
            ),
        };
        (seed ^ s.salt ^ salt, fbm)
    }

    /// Seed of the entrance mask.
    fn mask_seed(&self, seed: u64) -> u64 {
        seed ^ self.settings.salt ^ SALT_ENTRANCE_MASK
    }

    /// The entrance mask of column `(x, z)` (0 without entrances).
    pub fn entrance_mask(&self, seed: u64, x: i64, z: i64) -> f64 {
        self.settings
            .entrances
            .as_ref()
            .map_or(0.0, |e| noise::fbm_2d(self.mask_seed(seed), x, z, e.mask))
    }

    /// The quantized density of a corner. `value(noise)` gives the corner's noises; it is only
    /// asked for the ones the corner needs. The point and the bulk paths both call this, so
    /// they get the same bits.
    #[inline]
    fn corner_q(&self, column: Column, mut value: impl FnMut(Noise) -> f64) -> i32 {
        let s = &self.settings;
        let fade = &s.surface_fade;
        let depth = column.depth;
        let mut d = f64::INFINITY;
        if let Some(c) = &s.cheese {
            let layer = value(Noise::CheeseLayer);
            let cheese = c.threshold + c.layer_weight * (layer * layer) - value(Noise::Cheese);
            d = d.min(cheese + fade.cheese.at(depth));
        }
        if let Some(t) = &s.spaghetti {
            let (a, b) = (value(Noise::SpaghettiA), value(Noise::SpaghettiB));
            let tunnel = t.density(a, b, || value(Noise::SpaghettiWidth), 1.0);
            d = d.min(tunnel + fade.spaghetti.at(depth));
        }
        if let Some(n) = &s.noodles {
            let toggle = value(Noise::NoodleToggle);
            if toggle > 0.0 {
                let scale = (toggle / n.toggle_ramp).min(1.0);
                let (a, b) = (value(Noise::NoodleA), value(Noise::NoodleB));
                let tunnel = n.tunnels.density(a, b, || value(Noise::NoodleWidth), scale);
                d = d.min(tunnel + fade.noodles.at(depth));
            }
        }
        if let Some(e) = &s.entrances
            && let Some(scale) = e.scale(depth, column.mask)
        {
            let (a, b) = (value(Noise::EntranceA), value(Noise::EntranceB));
            d = d.min(
                e.tunnels
                    .density(a, b, || value(Noise::EntranceWidth), scale),
            );
        }
        ((d - s.density).clamp(-D_LIMIT, D_LIMIT) * Q_SCALE).round() as i32
    }

    /// The quantized density at lattice corner `(x, y, z)` (multiples of [`CELL`]) of a column
    /// of height `h` (the spec, one corner at a time).
    pub fn corner_density(&self, seed: u64, x: i64, y: i64, z: i64, h: i64) -> i32 {
        let column = Column {
            depth: h.saturating_sub(y),
            mask: self.entrance_mask(seed, x, z),
        };
        self.corner_q(column, |n| {
            let (seed, fbm) = self.noise(seed, n);
            noise::fbm_3d(seed, x, y, z, fbm)
        })
    }

    /// The trilinear sum `S` of voxel `p` (see [`Caves`]): carved iff `S < 0`.
    /// `height(x, z)` gives the surface height of a corner column. The spec: 8 corners, each
    /// computed from scratch.
    pub fn voxel_sum(&self, seed: u64, p: BlockPos, height: impl Fn(i64, i64) -> i64) -> i64 {
        let [cx, cy, cz] = CELL;
        let (fx, fy, fz) = (p.x.rem_euclid(cx), p.y.rem_euclid(cy), p.z.rem_euclid(cz));
        let (x0, y0, z0) = (p.x - fx, p.y - fy, p.z - fz);
        let mut sum = 0i64;
        for (x, wx) in [(x0, cx - fx), (x0 + cx, fx)] {
            for (z, wz) in [(z0, cz - fz), (z0 + cz, fz)] {
                let h = height(x, z);
                for (y, wy) in [(y0, cy - fy), (y0 + cy, fy)] {
                    let q = self.corner_density(seed, x, y, z, h);
                    sum += wx * wy * wz * i64::from(q);
                }
            }
        }
        sum
    }

    /// True if the caves carve voxel `p` of a column of height `h` (the spec): under the roof
    /// and with a negative trilinear sum. `height(x, z)` gives the surface height of the corner
    /// columns. Voxels above the surface are not "carved" (they are air anyway).
    pub fn carves(&self, seed: u64, p: BlockPos, h: i64, height: impl Fn(i64, i64) -> i64) -> bool {
        p.y <= h.saturating_sub(self.settings.surface_fade.roof)
            && self.voxel_sum(seed, p, height) < 0
    }

    /// The density at voxel `p`, `S / (128 * 2^20)` in density units: negative where the caves
    /// carve (under the roof).
    pub fn density_at(&self, seed: u64, p: BlockPos, height: impl Fn(i64, i64) -> i64) -> f64 {
        self.voxel_sum(seed, p, height) as f64 / (WEIGHT_SUM as f64 * Q_SCALE)
    }

    /// The quantized densities of the 405 corners of the chunk whose lowest corner is `origin`,
    /// indexed `(i * 5 + j) * 9 + k` for the corner at `origin + (4i, 8j, 4k)`.
    ///
    /// `surface` gives the heights of the 9x9 corner columns (indexed `i * 9 + k`) for chunks
    /// near the surface; `None` is only right for chunks wholly below every depth rule (the
    /// fades, the entrances and the roof) under the lowest possible column.
    /// Bit-identical to [`Caves::corner_density`] per corner (with the true heights), with
    /// one noise grid per noise ([`noise::fbm_3d_grid`]).
    pub fn chunk_corners(
        &self,
        seed: u64,
        origin: BlockPos,
        surface: Option<&[i64; COLUMN_COUNT]>,
    ) -> [i32; CORNER_COUNT] {
        let s = &self.settings;
        let lattice = |o: i64, step: i64, n: usize| -> Vec<i64> {
            (0..n as i64).map(|i| o + i * step).collect()
        };
        let xs = lattice(origin.x, CELL[0], CORNERS[0]);
        let ys = lattice(origin.y, CELL[1], CORNERS[1]);
        let zs = lattice(origin.z, CELL[2], CORNERS[2]);
        let column_of = |i: usize| (i / (CORNERS[1] * CORNERS[2])) * CORNERS[2] + i % CORNERS[2];
        let y_of = |i: usize| ys[(i / CORNERS[2]) % CORNERS[1]];
        // Column inputs per corner.
        let mut masks = [0.0; COLUMN_COUNT];
        if let (Some(e), Some(_)) = (&s.entrances, surface) {
            noise::fbm_2d_grid(self.mask_seed(seed), &xs, &zs, e.mask, &mut masks);
        }
        let columns: [Column; CORNER_COUNT] = std::array::from_fn(|i| {
            let c = column_of(i);
            Column {
                depth: surface.map_or(i64::MAX, |h| h[c].saturating_sub(y_of(i))),
                mask: masks[c],
            }
        });
        // The noise grids the corners need.
        let mut grids = vec![[0.0; CORNER_COUNT]; NOISE_COUNT];
        let fill = |n: Noise, grids: &mut Vec<[f64; CORNER_COUNT]>| {
            let (seed, fbm) = self.noise(seed, n);
            noise::fbm_3d_grid(seed, &xs, &ys, &zs, fbm, &mut grids[n as usize]);
        };
        if s.cheese.is_some() {
            fill(Noise::Cheese, &mut grids);
            fill(Noise::CheeseLayer, &mut grids);
        }
        if let Some(t) = &s.spaghetti {
            fill(Noise::SpaghettiA, &mut grids);
            fill(Noise::SpaghettiB, &mut grids);
            if t.width_variation != 0.0 {
                fill(Noise::SpaghettiWidth, &mut grids);
            }
        }
        if let Some(n) = &s.noodles {
            fill(Noise::NoodleToggle, &mut grids);
            if grids[Noise::NoodleToggle as usize].iter().any(|&t| t > 0.0) {
                fill(Noise::NoodleA, &mut grids);
                fill(Noise::NoodleB, &mut grids);
                if n.tunnels.width_variation != 0.0 {
                    fill(Noise::NoodleWidth, &mut grids);
                }
            }
        }
        if let Some(e) = &s.entrances
            && columns.iter().any(|c| e.scale(c.depth, c.mask).is_some())
        {
            fill(Noise::EntranceA, &mut grids);
            fill(Noise::EntranceB, &mut grids);
            if e.tunnels.width_variation != 0.0 {
                fill(Noise::EntranceWidth, &mut grids);
            }
        }
        std::array::from_fn(|i| self.corner_q(columns[i], |n| grids[n as usize][i]))
    }

    /// The heights of the 9x9 corner columns of the chunk at `origin` (indexed `i * 9 + k`).
    pub(crate) fn corner_heights(
        heightmap: &Heightmap,
        seed: u64,
        origin: BlockPos,
    ) -> [i64; COLUMN_COUNT] {
        let axis = |o: i64, step: i64| -> [i64; 9] { std::array::from_fn(|i| o + i as i64 * step) };
        let mut out = [0; COLUMN_COUNT];
        heightmap.heights_with(
            seed,
            &axis(origin.x, CELL[0]),
            &axis(origin.z, CELL[2]),
            &mut [0.0; COLUMN_COUNT],
            &mut out,
        );
        out
    }
}

/// Which voxels of a chunk the caves leave solid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CarveMask {
    /// No voxel is carved.
    Solid,
    /// Every voxel is carved.
    Air,
    /// Bit `z` of word `y * 32 + x` is 1 where the voxel stays solid.
    Mixed(Box<Occupancy>),
}

/// The voxels of a chunk the corner densities `q` ([`Caves::chunk_corners`]) leave solid
/// (`S >= 0`), without the roof. Cells whose 8 corners share a sign are filled whole; the others
/// voxel by voxel, with the trilinear sum split as `S = (4 - fz) * A0 + fz * A1`, where `A0`
/// and `A1` are the bilinear x-y sums of the cell's two z layers of corners (the same integer
/// as the spec's sum).
pub fn carve_mask(q: &[i32; CORNER_COUNT]) -> CarveMask {
    let (lo, hi) = q
        .iter()
        .fold((i32::MAX, i32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    if lo >= 0 {
        return CarveMask::Solid;
    }
    if hi < 0 {
        return CarveMask::Air;
    }
    let [cell_x, cell_y, cell_z] = CELL.map(|c| c as usize);
    let at = |i: usize, j: usize| (i * CORNERS[1] + j) * CORNERS[2];
    let mut occupancy: Box<Occupancy> = Box::new([0; CHUNK_AREA]);
    for cx in 0..CORNERS[0] - 1 {
        for cy in 0..CORNERS[1] - 1 {
            // The four corner rows along z around this column of cells.
            let rows = [
                &q[at(cx, cy)..][..CORNERS[2]],
                &q[at(cx + 1, cy)..][..CORNERS[2]],
                &q[at(cx, cy + 1)..][..CORNERS[2]],
                &q[at(cx + 1, cy + 1)..][..CORNERS[2]],
            ];
            // Solid cells as bits of a z word, and the cells to do voxel by voxel.
            let (mut solid, mut mixed) = (0u32, 0u32);
            for cz in 0..CORNERS[2] - 1 {
                let (mut lo, mut hi) = (i32::MAX, i32::MIN);
                for row in rows {
                    for &v in &row[cz..cz + 2] {
                        (lo, hi) = (lo.min(v), hi.max(v));
                    }
                }
                let bits = ((1u32 << cell_z) - 1) << (cz * cell_z);
                if lo >= 0 {
                    solid |= bits;
                } else if hi >= 0 {
                    mixed |= 1 << cz;
                }
            }
            for fy in 0..cell_y {
                let (wy0, wy1) = ((cell_y - fy) as i32, fy as i32);
                for fx in 0..cell_x {
                    let (wx0, wx1) = ((cell_x - fx) as i32, fx as i32);
                    let weights = [wx0 * wy0, wx1 * wy0, wx0 * wy1, wx1 * wy1];
                    // The bilinear sum of corner layer k.
                    let layer = |k: usize| -> i32 {
                        weights.iter().zip(rows).map(|(&w, row)| w * row[k]).sum()
                    };
                    let mut word = solid;
                    let mut cells = mixed;
                    while cells != 0 {
                        let cz = cells.trailing_zeros() as usize;
                        cells &= cells - 1;
                        let (a0, a1) = (layer(cz), layer(cz + 1));
                        // S at fz = 0, 1, 2, 3: (4 - fz) * a0 + fz * a1, by steps of a1 - a0.
                        let mut sum = cell_z as i32 * a0;
                        for fz in 0..cell_z {
                            word |= u32::from(sum >= 0) << (cz * cell_z + fz);
                            sum += a1 - a0;
                        }
                    }
                    occupancy[(cy * cell_y + fy) * 32 + cx * cell_x + fx] = word;
                }
            }
        }
    }
    CarveMask::Mixed(occupancy)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trilinear sum of local voxel `(x, y, z)`, corner by corner.
    fn naive_sum(q: &[i32; CORNER_COUNT], x: usize, y: usize, z: usize) -> i64 {
        let [cx, cy, cz] = CELL.map(|c| c as usize);
        let (i, j, k) = (x / cx, y / cy, z / cz);
        let (fx, fy, fz) = (x % cx, y % cy, z % cz);
        let mut sum = 0;
        for (di, wx) in [(0, cx - fx), (1, fx)] {
            for (dj, wy) in [(0, cy - fy), (1, fy)] {
                for (dk, wz) in [(0, cz - fz), (1, fz)] {
                    let corner = q[((i + di) * CORNERS[1] + j + dj) * CORNERS[2] + k + dk];
                    sum += (wx * wy * wz) as i64 * i64::from(corner);
                }
            }
        }
        sum
    }

    fn assert_mask_is_naive(q: &[i32; CORNER_COUNT]) {
        let solid = |x: usize, y: usize, z: usize| naive_sum(q, x, y, z) >= 0;
        match carve_mask(q) {
            CarveMask::Solid => {
                assert!((0..CHUNK_AREA * 32).all(|i| solid(i >> 10, i >> 5 & 31, i & 31)));
            }
            CarveMask::Air => {
                assert!((0..CHUNK_AREA * 32).all(|i| !solid(i >> 10, i >> 5 & 31, i & 31)));
            }
            CarveMask::Mixed(occupancy) => {
                for y in 0..32 {
                    for x in 0..32 {
                        for z in 0..32 {
                            let bit = occupancy[y * 32 + x] >> z & 1 == 1;
                            assert_eq!(bit, solid(x, y, z), "voxel ({x}, {y}, {z})");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn carve_mask_equals_the_trilinear_sum() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // Small values make exact zeros common (S = 0 is solid), large ones the full range.
        for (span, offset) in [
            (7, 3),
            (3, 1),
            (2, 1),
            (1 << 23, 1 << 22),
            (1 << 23, 1 << 21),
        ] {
            for _ in 0..8 {
                let q: [i32; CORNER_COUNT] =
                    std::array::from_fn(|_| (next() % span) as i32 - offset);
                assert_mask_is_naive(&q);
            }
        }
        // The extremes: every corner at the clamp, and a single negative corner.
        let limit = (D_LIMIT * Q_SCALE) as i32;
        assert_eq!(carve_mask(&[limit; CORNER_COUNT]), CarveMask::Solid);
        assert_eq!(carve_mask(&[-limit; CORNER_COUNT]), CarveMask::Air);
        assert_eq!(carve_mask(&[0; CORNER_COUNT]), CarveMask::Solid);
        let mut one = [limit; CORNER_COUNT];
        one[(4 * CORNERS[1] + 2) * CORNERS[2] + 4] = -limit;
        assert_mask_is_naive(&one);
        let mut alternating = [0; CORNER_COUNT];
        for (i, q) in alternating.iter_mut().enumerate() {
            *q = if i % 2 == 0 { limit } else { -limit };
        }
        assert_mask_is_naive(&alternating);
    }

    #[test]
    fn fades_and_entrance_scales_are_exact_at_their_ends() {
        let f = Fade {
            full: 8,
            none: 24,
            strength: 1.5,
        };
        assert_eq!(f.at(i64::MIN), 1.5);
        assert_eq!(f.at(8), 1.5);
        assert_eq!(f.at(16), 0.75);
        assert_eq!(f.at(24), 0.0);
        assert_eq!(f.at(i64::MAX), 0.0);
        let e = CaveSettings::default()
            .entrances
            .expect("default entrances");
        assert_eq!(e.scale(e.max_depth, 1.0), None);
        assert_eq!(e.scale(0, e.mask_threshold), None);
        assert_eq!(e.scale(0, 1.0), Some(1.0));
        assert_eq!(e.scale(e.max_depth - e.depth_ramp / 2, 1.0), Some(0.5));
        assert_eq!(e.scale(i64::MIN, 1.0), Some(1.0));
    }
}
