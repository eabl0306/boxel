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
/// Largest step budget of the march ([`validate`]).
pub const MAX_STEPS: u32 = 256;
/// The step budget a game gets unless it picks one ([`validate`], ADR 0030).
pub const DEFAULT_MAX_STEPS: u32 = 64;
/// Largest [`FogLayer::soft_distance`], in blocks: it bounds the extra samples of a ray.
pub const MAX_SOFT_DISTANCE: f32 = 32.0;
/// Most cell rows a band with a [`FogLayer::threshold_rise`] may have.
pub const MAX_PROFILE_ROWS: i64 = 1 << 16;
/// Deepest [`FogLayer::edge_bottom`] and [`FogLayer::edge_top`], in blocks.
pub const MAX_EDGE: u32 = 1024;

/// One layer of fog or clouds: the cells of `[min_y, max_y)` with one density each.
///
/// A cell's density is `density * coverage`, where the coverage (0 to 1) comes from integer
/// value noise over the cell's absolute index minus the wind times the time: the cells never
/// move, they fill up on the side the wind blows to and empty on the other. Every cell is a
/// crisp box: the light a ray loses through it is `exp(-density * coverage * length)`. Near the
/// camera ([`FogLayer::soft_distance`]) the fog can be soft instead, without cell edges.
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
    /// How much the threshold rises from the bottom of the band to its top (0 to 1), along the
    /// square of the height: 0 keeps it level. With more, the cells thin out upwards from a
    /// flat, wide base into rounded, uneven tops (cumulus clouds).
    pub threshold_rise: f32,
    /// Depth of the bottom edge, in blocks (0 to [`MAX_EDGE`]). Within it the threshold rises
    /// along a smoothstep to never covered at `min_y`, so near the floor only cells of strong
    /// noise have fog and every column ends at its own height: an uneven underside of cubes
    /// instead of a flat plane. Distances are measured to a row's face nearest the plane, so the
    /// row along the plane is always empty, and an edge of a few cells looks best (one under a
    /// cell only empties that row). 0 (the default) cuts the band at the plane.
    pub edge_bottom: u32,
    /// Depth of the top edge, in blocks (0 to [`MAX_EDGE`]): like [`FogLayer::edge_bottom`],
    /// below `max_y`, for an uneven skyline of cubes instead of a flat top.
    pub edge_top: u32,
    /// Width of the noise range (0 to 1) over which a cell goes from empty to full, along a
    /// smoothstep. As the wind moves the noise, a cell is born almost transparent and fills in,
    /// and a dying one fades out; the outer cells of a bank are thinner than the inner ones. 0
    /// makes every cell empty or full: binary cubes that pop in and out.
    pub softness: f32,
    /// Extinction per block of a full cell: 3 / V sees about V blocks
    /// ([`density_for_visibility`]).
    pub density: f32,
    /// Color of the fog lit by a white noon sky (linear RGB).
    pub color: [f32; 3],
    /// How much of the sky color the fog takes (0 to 1): the warm tint of a sunset.
    pub sky_tint: f32,
    /// Brightness of the fog by the face a ray enters a body of cells through: top, bottom,
    /// sides along x, sides along z. All 1 (the default) is off: fog has no faces.
    /// [`VOXEL_FACE_SHADE`] shades it like the voxels' faces.
    pub face_shade: [f32; 4],
    /// Brightness at the bottom and at the top of the band, linear in between: `[0.7, 1.0]`
    /// gives clouds a gray base and a lit top. `[1.0, 1.0]` (the default) is off.
    pub height_shade: [f32; 2],
    /// Distance from the camera (blocks, 0 to [`MAX_SOFT_DISTANCE`]) within which the fog is
    /// soft: the noise is read at every point along the ray instead of once per cell, so no cell
    /// edge shows. Beyond it the fog blends back into the cells over half that distance. 0 (the
    /// default) keeps every cell cubic.
    pub soft_distance: f32,
    /// Wind in blocks per second. `y` only moves [`NoiseDims::Volume`] noise.
    pub wind: [f32; 3],
    /// The farthest the layer is drawn (blocks along the ray), `(0, 4096]`.
    pub max_distance: f32,
    /// How the layer fades into the sky with distance.
    pub fade: FogFade,
}

impl FogLayer {
    /// A plain blanket over `[min_y, max_y)`: 1-block cells, every cell full, white, seen about
    /// 50 blocks through, no shading, cubic up to the camera, cut flat at both planes, no wind,
    /// drawn to 512 blocks, faded like the terrain.
    pub fn new(min_y: i64, max_y: i64) -> Self {
        Self {
            min_y,
            max_y,
            cell: [1, 1, 1],
            noise: FogNoise::default(),
            threshold: 0.0,
            threshold_rise: 0.0,
            edge_bottom: 0,
            edge_top: 0,
            softness: 0.0,
            density: density_for_visibility(50.0),
            color: [1.0; 3],
            sky_tint: 0.0,
            face_shade: [1.0; 4],
            height_shade: [1.0; 2],
            soft_distance: 0.0,
            wind: [0.0; 3],
            max_distance: 512.0,
            fade: FogFade::Scene,
        }
    }
}

/// The face shades of the voxel shaders (top, bottom, sides along x, sides along z), for a game
/// that wants its fog shaded like blocks ([`FogLayer::face_shade`]; off by default).
pub const VOXEL_FACE_SHADE: [f32; 4] = [1.0, 0.5, 0.8, 0.65];

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
    /// A value that is not finite or is out of its range (threshold, its rise, softness and sky
    /// tint in `0..=1`; density, colors and shades not negative).
    NonFinite(usize),
    /// `max_distance` outside `(0, 4096]`, a fade range that is empty or negative, or a soft
    /// distance outside `0..=32`.
    BadDistance(usize),
    /// A threshold rise over a band of more than [`MAX_PROFILE_ROWS`] cell rows.
    TallProfile(usize),
    /// An edge deeper than [`MAX_EDGE`] blocks.
    DeepEdge(usize),
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
                "fog layer {i}: max_distance goes up to {MAX_FOG_DISTANCE}, a fade range must not be empty and soft_distance goes up to {MAX_SOFT_DISTANCE}"
            ),
            FogError::TallProfile(i) => write!(
                f,
                "fog layer {i}: a threshold rise needs a band of at most {MAX_PROFILE_ROWS} cell rows"
            ),
            FogError::DeepEdge(i) => {
                write!(f, "fog layer {i}: an edge goes up to {MAX_EDGE} blocks")
            }
            FogError::BadSteps => write!(f, "the fog step budget goes from 1 to {MAX_STEPS}"),
        }
    }
}

impl std::error::Error for FogError {}

/// Checks a set of layers and the step budget against the limits of the march.
pub fn validate(layers: &[FogLayer], max_steps: u32) -> Result<(), FogError> {
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
        || !unit(l.threshold_rise)
        || !unit(l.softness)
        || !unit(l.sky_tint)
        || !positive(l.density)
        || !l.color.iter().all(|&c| positive(c))
        || !l.face_shade.iter().all(|&c| positive(c))
        || !l.height_shade.iter().all(|&c| positive(c))
        || !l.wind.iter().all(|w| w.is_finite() && w.abs() <= 1024.0)
        || !l.max_distance.is_finite()
        || !l.soft_distance.is_finite()
    {
        return Err(FogError::NonFinite(i));
    }
    if l.max_distance <= 0.0
        || l.max_distance > MAX_FOG_DISTANCE
        || !fade_ok
        || !(0.0..=MAX_SOFT_DISTANCE).contains(&l.soft_distance)
    {
        return Err(FogError::BadDistance(i));
    }
    let rows = (i128::from(l.max_y) - i128::from(l.min_y)) / i128::from(cy);
    if l.threshold_rise > 0.0 && rows > i128::from(MAX_PROFILE_ROWS) {
        return Err(FogError::TallProfile(i));
    }
    if l.edge_bottom > MAX_EDGE || l.edge_top > MAX_EDGE {
        return Err(FogError::DeepEdge(i));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(layers: &[FogLayer]) -> Result<(), FogError> {
        validate(layers, 64)
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
    fn a_plain_layer_has_no_shading_and_no_soft_fog() {
        let l = FogLayer::new(0, 16);
        assert_eq!(
            l.face_shade, [1.0; 4],
            "fog has no faces unless the game asks"
        );
        assert_eq!(l.height_shade, [1.0; 2]);
        assert_eq!((l.soft_distance, l.threshold_rise), (0.0, 0.0));
        assert_eq!(
            (l.edge_bottom, l.edge_top),
            (0, 0),
            "cut flat at both planes"
        );
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
            &|l| l.height_shade[0] = -0.5,
            &|l| l.threshold_rise = 1.1,
            &|l| l.wind[0] = f32::NAN,
            &|l| l.max_distance = f32::NAN,
            &|l| l.soft_distance = f32::INFINITY,
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
            &|l| l.soft_distance = -1.0,
            &|l| l.soft_distance = 33.0,
        ] {
            assert_eq!(ok(&[with(bad)]), Err(FogError::BadDistance(0)));
        }
        // A rise over a band of 2^16 rows is fine, one more row is not; without a rise any
        // height is.
        let tall = |rows: i64, rise: f32| {
            let mut l = FogLayer::new(-rows, 0);
            l.threshold_rise = rise;
            ok(&[l])
        };
        assert_eq!(tall(MAX_PROFILE_ROWS, 0.5), Ok(()));
        assert_eq!(
            tall(MAX_PROFILE_ROWS + 1, 0.5),
            Err(FogError::TallProfile(0))
        );
        assert_eq!(tall(1 << 40, 0.0), Ok(()));
        // Edges up to MAX_EDGE blocks, on any band.
        assert_eq!(
            ok(&[with(&|l| (l.edge_bottom, l.edge_top) = (MAX_EDGE, MAX_EDGE))]),
            Ok(())
        );
        for edges in [(MAX_EDGE + 1, 0), (0, MAX_EDGE + 1)] {
            assert_eq!(
                ok(&[with(&|l| (l.edge_bottom, l.edge_top) = edges)]),
                Err(FogError::DeepEdge(0))
            );
        }
        assert_eq!(
            validate(std::slice::from_ref(&base), 0),
            Err(FogError::BadSteps)
        );
        assert_eq!(validate(&[base], 257), Err(FogError::BadSteps));
        // Every error has a message.
        assert!(FogError::BadSteps.to_string().contains("256"));
        assert!(FogError::TallProfile(2).to_string().contains("65536"));
        assert!(FogError::DeepEdge(1).to_string().contains("1024"));
    }
}
