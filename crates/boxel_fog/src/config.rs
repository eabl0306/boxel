//! The style of the fog: layers of cubic cells, each with its height band, cell size, noise,
//! density, color and wind. Plain data; every value is the game's choice (the engine fixes no
//! look). [`validate`] checks a set of layers against the limits the renderer relies on.

use crate::MAX_FOG_LAYERS;

/// The farthest a layer is drawn, in blocks ([`FogLayer::max_distance`]): it bounds the cell
/// offsets the GPU walks to 4096 cells, so every integer stays small.
pub const MAX_FOG_DISTANCE: f32 = 4096.0;
/// Largest cell edge, in blocks.
pub const MAX_CELL: u32 = 32;
/// Largest noise scale, in cells per lattice step.
pub const MAX_SCALE: u32 = 64;
/// Largest ground texel ([`validate`]), in blocks.
pub const MAX_GROUND_TEXEL: u32 = 16;
/// Height a ground-mode layer may span, in blocks: the ground map stores heights in a byte.
pub const MAX_GROUND_SPAN: i64 = 254;
/// Largest step budget of the march ([`validate`]).
pub const MAX_STEPS: u32 = 256;

/// One layer of fog or clouds: the cells of `[min_y, max_y)` with one density each.
///
/// A cell's density is `density * coverage`, where the coverage (0 to 1) comes from integer
/// value noise over the cell's absolute index minus the wind times the time: the cells never
/// move, they fill up on the side the wind blows to and empty on the other. Every cell is a
/// crisp box: the light a ray loses through it is `exp(-density * coverage * length)`.
#[derive(Clone, Debug, PartialEq)]
pub struct FogLayer {
    /// Bottom of the band (world blocks, inclusive). A multiple of `cell[1]`.
    pub min_y: i64,
    /// Top of the band (exclusive). A multiple of `cell[1]`, above `min_y`. Bands of different
    /// layers never overlap.
    pub max_y: i64,
    /// Cell size in blocks per axis, each `1..=32` (`[2, 2, 2]`, `[12, 4, 12]`).
    pub cell: [u32; 3],
    /// The noise that gives each cell its coverage.
    pub noise: FogNoise,
    /// Noise level (0 to 1) where cells start to fill: 0 fills every cell (a blanket), 1 none.
    pub threshold: f32,
    /// Width of the noise range (0 to 1) over which a cell goes from empty to full: 0 makes
    /// every cell empty or full (binary cubes).
    pub softness: f32,
    /// Extinction per block of a full cell: 3 / V sees about V blocks
    /// ([`density_for_visibility`]).
    pub density: f32,
    /// Color of the fog lit by a white noon sky (linear RGB).
    pub color: [f32; 3],
    /// How much of the sky color the fog takes (0 to 1): the warm tint of a sunset.
    pub sky_tint: f32,
    /// Brightness of the fog by the face a ray enters a body of cells through: top, bottom,
    /// sides along x, sides along z. Default `[1.0, 0.5, 0.8, 0.65]`, the voxel faces' shades.
    pub face_shade: [f32; 4],
    /// Wind in blocks per second. `y` only moves [`NoiseDims::Volume`] noise.
    pub wind: [f32; 3],
    /// How the layer meets the terrain.
    pub ground: FogGround,
    /// The farthest the layer is drawn (blocks along the ray), `(0, 4096]`.
    pub max_distance: f32,
    /// How the layer fades into the sky with distance.
    pub fade: FogFade,
}

impl FogLayer {
    /// A plain blanket over `[min_y, max_y)`: 1-block cells, every cell full, white, seen about
    /// 50 blocks through, no wind, drawn to 512 blocks, faded like the terrain.
    pub fn new(min_y: i64, max_y: i64) -> Self {
        Self {
            min_y,
            max_y,
            cell: [1, 1, 1],
            noise: FogNoise::default(),
            threshold: 0.0,
            softness: 0.0,
            density: density_for_visibility(50.0),
            color: [1.0; 3],
            sky_tint: 0.0,
            face_shade: DEFAULT_FACE_SHADE,
            wind: [0.0; 3],
            ground: FogGround::Free,
            max_distance: 512.0,
            fade: FogFade::Scene,
        }
    }

    /// True if the layer reads the ground map ([`FogGround::AboveGround`] or
    /// [`FogGround::Pooled`]).
    pub fn uses_ground(&self) -> bool {
        !matches!(self.ground, FogGround::Free)
    }
}

/// The face shades of the voxel shaders: top, bottom, sides along x, sides along z.
pub const DEFAULT_FACE_SHADE: [f32; 4] = [1.0, 0.5, 0.8, 0.65];

/// The noise of a layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FogNoise {
    /// Cells per lattice step, a power of two `1..=64`: larger makes bigger blobs.
    pub scale: u32,
    /// 1 or 2. The second octave has half the scale (so 2 needs a scale of 2 or more) and a
    /// third of the weight.
    pub octaves: u8,
    /// 2D (columns) or 3D.
    pub dims: NoiseDims,
    /// Seed of the hash.
    pub seed: u64,
}

impl Default for FogNoise {
    /// Scale 1, one octave, flat, seed 0.
    fn default() -> Self {
        Self {
            scale: 1,
            octaves: 1,
            dims: NoiseDims::Flat,
            seed: 0,
        }
    }
}

/// Dimensions of the noise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoiseDims {
    /// Over x and z only: every cell of a column has the same coverage (clouds).
    #[default]
    Flat,
    /// Over x, y and z (fog banks).
    Volume,
}

/// How a layer meets the terrain. The ground comes from the sky tops of the light (ADR 0015),
/// never from the blocks: water and leaves are ground.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FogGround {
    /// The band as it is: fog fills caves and overhangs inside it too.
    #[default]
    Free,
    /// Only above the ground of each column: nothing under the surface, water or caves.
    AboveGround,
    /// Above the ground and only up to a fill level that pools in the valleys: the lowest
    /// ground within `radius` ground texels plus `depth` blocks (at most `max_y`). One per set
    /// of layers.
    Pooled {
        /// Blocks of fog over the regional floor.
        depth: u32,
        /// Radius of the regional floor, in ground texels.
        radius: u32,
    },
}

/// How a layer fades into the sky color with distance.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum FogFade {
    /// Like the terrain: from the scene's fog start to its fog end.
    #[default]
    Scene,
    /// Linearly from `start` to `end` blocks.
    Range {
        /// Distance where the fade starts.
        start: f32,
        /// Distance where the layer has the sky color.
        end: f32,
    },
}

/// Resolution of the fog march.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FogResolution {
    /// One ray per 2x2 pixels, with the depth of each pixel (the default).
    #[default]
    Half,
    /// One ray per pixel.
    Full,
}

/// Extinction per block that leaves 5 % of the light (`e^-3`) after `visibility` blocks:
/// `3 / visibility`. 2 blocks give 1.5, 50 give 0.06.
pub fn density_for_visibility(visibility: f32) -> f32 {
    3.0 / visibility
}

/// Why a set of layers cannot be drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FogError {
    /// More than [`MAX_FOG_LAYERS`] layers.
    TooManyLayers(usize),
    /// Two bands overlap (layer indices).
    Overlapping(usize, usize),
    /// A band edge is not a multiple of the cell height.
    Unaligned(usize),
    /// `min_y >= max_y`.
    EmptyBand(usize),
    /// A cell edge outside `1..=32`.
    BadCell(usize),
    /// A noise scale that is not a power of two in `1..=64`.
    BadScale(usize),
    /// Octaves outside `1..=2`, or 2 with scale 1.
    BadOctaves(usize),
    /// A value that is not finite or is out of its range (threshold, softness and sky tint in
    /// `0..=1`; density, colors and shades not negative).
    NonFinite(usize),
    /// `max_distance` outside `(0, 4096]`, or a fade range that is empty or negative.
    BadDistance(usize),
    /// More than one [`FogGround::Pooled`] layer.
    TwoPooled,
    /// The layers that read the ground span more than 254 blocks.
    GroundSpan,
    /// The ground texel is not a power of two in `1..=16`, or a ground-mode layer's cells along
    /// x or z do not divide it.
    GroundTexel,
    /// The step budget is outside `1..=256`.
    BadSteps,
}

impl std::fmt::Display for FogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FogError::TooManyLayers(n) => write!(f, "{n} fog layers (at most {MAX_FOG_LAYERS})"),
            FogError::Overlapping(a, b) => write!(f, "fog layers {a} and {b} overlap"),
            FogError::Unaligned(i) => {
                write!(
                    f,
                    "fog layer {i}: band edges must be multiples of the cell height"
                )
            }
            FogError::EmptyBand(i) => write!(f, "fog layer {i}: min_y must be below max_y"),
            FogError::BadCell(i) => write!(f, "fog layer {i}: cell edges go from 1 to {MAX_CELL}"),
            FogError::BadScale(i) => write!(
                f,
                "fog layer {i}: the noise scale must be a power of two from 1 to {MAX_SCALE}"
            ),
            FogError::BadOctaves(i) => write!(
                f,
                "fog layer {i}: 1 or 2 octaves, and 2 need a noise scale of 2 or more"
            ),
            FogError::NonFinite(i) => {
                write!(f, "fog layer {i}: a value is not finite or out of range")
            }
            FogError::BadDistance(i) => write!(
                f,
                "fog layer {i}: max_distance goes up to {MAX_FOG_DISTANCE} and a fade range must not be empty"
            ),
            FogError::TwoPooled => write!(f, "at most one pooled fog layer"),
            FogError::GroundSpan => write!(
                f,
                "the fog layers that follow the ground span more than {MAX_GROUND_SPAN} blocks"
            ),
            FogError::GroundTexel => write!(
                f,
                "the ground texel must be a power of two up to {MAX_GROUND_TEXEL} and a multiple of \
                 the cells of the layers that follow the ground"
            ),
            FogError::BadSteps => write!(f, "the fog step budget goes from 1 to {MAX_STEPS}"),
        }
    }
}

impl std::error::Error for FogError {}

/// Checks a set of layers, the ground texel (blocks per ground map texel) and the step budget
/// against the limits of the march.
pub fn validate(layers: &[FogLayer], ground_texel: u32, max_steps: u32) -> Result<(), FogError> {
    if layers.len() > MAX_FOG_LAYERS {
        return Err(FogError::TooManyLayers(layers.len()));
    }
    if !(1..=MAX_STEPS).contains(&max_steps) {
        return Err(FogError::BadSteps);
    }
    for (i, layer) in layers.iter().enumerate() {
        validate_layer(i, layer)?;
    }
    for (i, a) in layers.iter().enumerate() {
        for (j, b) in layers.iter().enumerate().skip(i + 1) {
            if a.min_y < b.max_y && b.min_y < a.max_y {
                return Err(FogError::Overlapping(i, j));
            }
        }
    }
    let pooled = layers
        .iter()
        .filter(|l| matches!(l.ground, FogGround::Pooled { .. }))
        .count();
    if pooled > 1 {
        return Err(FogError::TwoPooled);
    }
    let ground: Vec<&FogLayer> = layers.iter().filter(|l| l.uses_ground()).collect();
    if let (Some(lo), Some(hi)) = (
        ground.iter().map(|l| l.min_y).min(),
        ground.iter().map(|l| l.max_y).max(),
    ) && hi.saturating_sub(lo) > MAX_GROUND_SPAN
    {
        return Err(FogError::GroundSpan);
    }
    if !ground.is_empty()
        && (!ground_texel.is_power_of_two()
            || ground_texel > MAX_GROUND_TEXEL
            || ground.iter().any(|l| {
                !ground_texel.is_multiple_of(l.cell[0]) || !ground_texel.is_multiple_of(l.cell[2])
            }))
    {
        return Err(FogError::GroundTexel);
    }
    Ok(())
}

fn validate_layer(i: usize, l: &FogLayer) -> Result<(), FogError> {
    if l.min_y >= l.max_y {
        return Err(FogError::EmptyBand(i));
    }
    if l.cell.iter().any(|&c| !(1..=MAX_CELL).contains(&c)) {
        return Err(FogError::BadCell(i));
    }
    let cy = i64::from(l.cell[1]);
    if l.min_y.rem_euclid(cy) != 0 || l.max_y.rem_euclid(cy) != 0 {
        return Err(FogError::Unaligned(i));
    }
    if !l.noise.scale.is_power_of_two() || l.noise.scale > MAX_SCALE {
        return Err(FogError::BadScale(i));
    }
    if !(1..=2).contains(&l.noise.octaves) || (l.noise.octaves == 2 && l.noise.scale < 2) {
        return Err(FogError::BadOctaves(i));
    }
    let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
    let positive = |v: f32| v.is_finite() && v >= 0.0;
    let fade_ok = match l.fade {
        FogFade::Scene => true,
        FogFade::Range { start, end } => positive(start) && end.is_finite() && end > start,
    };
    if !unit(l.threshold)
        || !unit(l.softness)
        || !unit(l.sky_tint)
        || !positive(l.density)
        || !l.color.iter().all(|&c| positive(c))
        || !l.face_shade.iter().all(|&c| positive(c))
        || !l.wind.iter().all(|w| w.is_finite() && w.abs() <= 1024.0)
        || !l.max_distance.is_finite()
    {
        return Err(FogError::NonFinite(i));
    }
    if l.max_distance <= 0.0 || l.max_distance > MAX_FOG_DISTANCE || !fade_ok {
        return Err(FogError::BadDistance(i));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(layers: &[FogLayer]) -> Result<(), FogError> {
        validate(layers, 4, 64)
    }

    #[test]
    fn visibility_density_leaves_five_percent() {
        for v in [2.0f32, 10.0, 40.0, 50.0] {
            let t = (-density_for_visibility(v) * v).exp();
            assert!((t - (-3.0f32).exp()).abs() < 1e-6, "{v}: {t}");
        }
        assert_eq!(density_for_visibility(2.0), 1.5);
        assert!((density_for_visibility(50.0) - 0.06).abs() < 1e-7);
    }

    #[test]
    fn a_plain_layer_and_empty_sets_are_valid() {
        assert_eq!(ok(&[]), Ok(()));
        assert_eq!(ok(&[FogLayer::new(0, 16)]), Ok(()));
        let mut clouds = FogLayer::new(192, 196);
        clouds.cell = [12, 4, 12];
        clouds.noise.scale = 4;
        assert_eq!(ok(&[FogLayer::new(60, 70), clouds]), Ok(()));
    }

    #[test]
    fn every_error() {
        let base = FogLayer::new(0, 16);
        let with = |f: &dyn Fn(&mut FogLayer)| {
            let mut l = base.clone();
            f(&mut l);
            l
        };
        assert_eq!(ok(&vec![base.clone(); 5]), Err(FogError::TooManyLayers(5)));
        assert_eq!(
            ok(&[base.clone(), with(&|l| (l.min_y, l.max_y) = (15, 20))]),
            Err(FogError::Overlapping(0, 1))
        );
        assert_eq!(
            ok(&[with(&|l| l.cell = [2, 3, 2])]),
            Err(FogError::Unaligned(0))
        );
        assert_eq!(
            ok(&[with(&|l| l.max_y = l.min_y)]),
            Err(FogError::EmptyBand(0))
        );
        for cell in [[0, 1, 1], [1, 33, 1], [1, 1, 64]] {
            assert_eq!(ok(&[with(&|l| l.cell = cell)]), Err(FogError::BadCell(0)));
        }
        for scale in [0, 3, 128] {
            assert_eq!(
                ok(&[with(&|l| l.noise.scale = scale)]),
                Err(FogError::BadScale(0))
            );
        }
        assert_eq!(
            ok(&[with(&|l| l.noise.octaves = 2)]),
            Err(FogError::BadOctaves(0)),
            "2 octaves need a scale of 2"
        );
        assert_eq!(
            ok(&[with(&|l| (l.noise.octaves, l.noise.scale) = (3, 4))]),
            Err(FogError::BadOctaves(0))
        );
        for bad in [
            &(|l: &mut FogLayer| l.threshold = 1.5) as &dyn Fn(&mut FogLayer),
            &|l| l.softness = f32::NAN,
            &|l| l.density = -1.0,
            &|l| l.color[1] = f32::INFINITY,
            &|l| l.sky_tint = 2.0,
            &|l| l.face_shade[3] = -0.1,
            &|l| l.wind[0] = f32::NAN,
            &|l| l.max_distance = f32::NAN,
        ] {
            assert_eq!(ok(&[with(bad)]), Err(FogError::NonFinite(0)));
        }
        for bad in [
            &(|l: &mut FogLayer| l.max_distance = 0.0) as &dyn Fn(&mut FogLayer),
            &|l| l.max_distance = 5000.0,
            &|l| {
                l.fade = FogFade::Range {
                    start: 10.0,
                    end: 10.0,
                }
            },
            &|l| {
                l.fade = FogFade::Range {
                    start: -1.0,
                    end: 10.0,
                }
            },
        ] {
            assert_eq!(ok(&[with(bad)]), Err(FogError::BadDistance(0)));
        }
        let pooled = |min, max| {
            let mut l = FogLayer::new(min, max);
            l.ground = FogGround::Pooled {
                depth: 4,
                radius: 4,
            };
            l
        };
        assert_eq!(ok(&[pooled(0, 8), pooled(8, 16)]), Err(FogError::TwoPooled));
        let mut above = FogLayer::new(200, 300);
        above.ground = FogGround::AboveGround;
        assert_eq!(ok(&[pooled(0, 8), above]), Err(FogError::GroundSpan));
        assert_eq!(validate(&[pooled(0, 8)], 3, 64), Err(FogError::GroundTexel));
        assert_eq!(
            validate(&[pooled(0, 8)], 32, 64),
            Err(FogError::GroundTexel)
        );
        let mut wide = pooled(0, 8);
        wide.cell = [8, 1, 8];
        assert_eq!(validate(&[wide], 4, 64), Err(FogError::GroundTexel));
        assert_eq!(
            validate(std::slice::from_ref(&base), 3, 64),
            Ok(()),
            "the texel only matters with ground layers"
        );
        assert_eq!(
            validate(std::slice::from_ref(&base), 4, 0),
            Err(FogError::BadSteps)
        );
        assert_eq!(validate(&[base], 4, 257), Err(FogError::BadSteps));
        // Every error has a message.
        assert!(FogError::GroundSpan.to_string().contains("254"));
    }
}
