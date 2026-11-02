//! Sky sources and realms on the overlay (ADR 0061): the `Realm` and `Source` lines and
//! [`SkySourceMarkers`].

use boxel_camera::CameraView;
use boxel_light::Beam;
use boxel_math::{DVec3, UVec2, Vec4};
use boxel_render::OverlayRect;
use boxel_voxel::{BeamDir, BlockPos, CHUNK_SHIFT};
use boxel_world::{DaylightCurve, RealmLighting, RealmPlanes, SkyMap, TimeOfDay};
use std::fmt::Write;

/// The realm of the eye's voxel, as the `Realm` line shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RealmProbe {
    /// Index of the realm, bottom first ([`RealmPlanes::realm_of`]).
    pub realm: usize,
    /// Realms of the dimension.
    pub realms: usize,
    /// The realm has its own daylight curve ([`boxel_world::RealmLight::curve`]); else it
    /// follows the global one.
    pub own_curve: bool,
    /// Its sky light now (linear RGB), `None` without a clock.
    pub sky_light: Option<[f32; 3]>,
}

impl RealmProbe {
    /// The realm at height `y`, with its curve and its sky light at `time` (when known).
    pub fn at(
        y: i64,
        planes: &RealmPlanes,
        lighting: Option<&RealmLighting>,
        clock: Option<(&DaylightCurve, &TimeOfDay)>,
    ) -> Self {
        let realm = planes.realm_of(y);
        let default = RealmLighting::default();
        let lighting = lighting.unwrap_or(&default);
        Self {
            realm,
            realms: planes.len(),
            own_curve: lighting.get(realm).curve.is_some(),
            sky_light: clock.map(|(curve, time)| lighting.at(realm, curve, time).sky_light),
        }
    }

    /// `Realm 0 of 2  curve global  sky 1.00 0.98 0.95`.
    pub fn line(&self) -> String {
        let curve = if self.own_curve { "own" } else { "global" };
        let mut line = format!("Realm {} of {}  curve {curve}", self.realm, self.realms);
        if let Some([r, g, b]) = self.sky_light {
            let _ = write!(line, "  sky {r:.2} {g:.2} {b:.2}");
        }
        line
    }
}

/// The sky source whose beam lights the crosshair target, as the `Source` line shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceProbe {
    /// No beam tile covers the target yet: no mesh near it needed one, or its build is pending.
    Unknown,
    /// No beam crosses the target.
    None,
    /// The beam through the target (the lowest one if several cross it), and its source cell.
    Beam(BlockPos, Beam),
}

impl SourceProbe {
    /// The beam through `pos`, from the beam tile of its chunk column.
    pub fn at(sky: &SkyMap, pos: BlockPos) -> Self {
        let Some(tile) = sky.beam_tile(pos.x >> CHUNK_SHIFT, pos.z >> CHUNK_SHIFT) else {
            return Self::Unknown;
        };
        if !tile.window().contains(&pos.y) {
            return Self::Unknown;
        }
        let (x, z) = ((pos.x & 31) as u32, (pos.z & 31) as u32);
        tile.column(x, z)
            .find(|b| b.lo <= pos.y && pos.y <= b.hi)
            .map_or(Self::None, |b| {
                Self::Beam(BlockPos::new(pos.x, b.source, pos.z), b)
            })
    }

    /// `Source 3 74 -2  down range 64  beam 11..74`, `Source none` or `Source -`.
    pub fn line(&self) -> String {
        match self {
            Self::Unknown => "Source -".to_string(),
            Self::None => "Source none".to_string(),
            Self::Beam(pos, beam) => {
                let dir = match beam.def.dir {
                    BeamDir::Down => "down",
                    BeamDir::Up => "up",
                };
                format!(
                    "Source {} {} {}  {dir} range {}  beam {}..{}",
                    pos.x, pos.y, pos.z, beam.def.range, beam.lo, beam.hi
                )
            }
        }
    }
}

/// Marks the sky sources and their beams near the camera on the overlay: a square on each
/// source and a dot every [`SkySourceMarkers::beam_step`] cells of its beam, under the F3 text
/// and drawn whether the text is shown or not. Off by default; apps and games flip `visible`
/// (a creative mode, a debug key). It reads the beam tiles the world built
/// ([`SkyMap::beam_tile`]): the beams that reach a window some mesh needed, which near the
/// camera is every beam that lights what it sees.
#[derive(boxel_ecs::prelude::Resource, Clone, Copy, Debug, PartialEq)]
pub struct SkySourceMarkers {
    /// Drawn.
    pub visible: bool,
    /// Only sources and beam cells this close to the eye (in blocks) are marked.
    pub radius: f64,
    /// Side of a source's square, in physical pixels.
    pub source_size: f32,
    /// Side of a beam dot, in physical pixels.
    pub beam_size: f32,
    /// A dot every this many cells of a beam (at least 1), counted from its lowest cell.
    pub beam_step: u32,
    /// Color of a source (linear RGBA).
    pub source_color: [f32; 4],
    /// Color of a beam dot.
    pub beam_color: [f32; 4],
}

impl Default for SkySourceMarkers {
    fn default() -> Self {
        Self {
            visible: false,
            radius: 48.0,
            source_size: 8.0,
            beam_size: 3.0,
            beam_step: 2,
            source_color: [1.0, 0.85, 0.2, 0.9],
            beam_color: [1.0, 0.95, 0.55, 0.5],
        }
    }
}

/// Most marks drawn in one frame (a full ceiling of sources fills it before the radius does).
pub const MAX_MARKS: usize = 16_384;

/// Where the center of voxel `pos` lands on a frame of `size` physical pixels seen through
/// `view` (pixels from the top left), `None` behind the camera, nearer than its near plane or
/// outside the frame. Exact at any world coordinate: the voxel is taken relative to the
/// view's render origin in integers first.
pub fn project(view: &CameraView, size: UVec2, pos: BlockPos) -> Option<[f32; 2]> {
    let o = view.render_origin;
    let rel = |p: i64, o: i64| p.saturating_sub(o) as f32 + 0.5;
    let clip = view.view_proj * Vec4::new(rel(pos.x, o.x), rel(pos.y, o.y), rel(pos.z, o.z), 1.0);
    if clip.w.is_nan() || clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    // Reverse-Z: the near plane is at 1, infinity at 0 (NaN fails every test).
    let inside = ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0 && ndc.z > 0.0 && ndc.z <= 1.0;
    if !inside {
        return None;
    }
    let (w, h) = (size.x as f32, size.y as f32);
    Some([(ndc.x + 1.0) * 0.5 * w, (1.0 - ndc.y) * 0.5 * h])
}

/// A square of side `side` around `center`, cut to the frame.
fn square(center: [f32; 2], side: f32, size: UVec2, color: [f32; 4]) -> OverlayRect {
    let half = side / 2.0;
    let (w, h) = (size.x as f32, size.y as f32);
    OverlayRect {
        min: [(center[0] - half).max(0.0), (center[1] - half).max(0.0)],
        max: [(center[0] + half).min(w), (center[1] + half).min(h)],
        color,
    }
}

impl SkySourceMarkers {
    /// The marks of `beams` (each with the world column `(x, z)` it lies in) seen through `view`
    /// from `eye` on a frame of `size` pixels, appended to `out`: the beam dots first, then the
    /// sources over them, at most [`MAX_MARKS`]. Cells farther than [`SkySourceMarkers::radius`]
    /// from `eye`, behind the camera or outside the frame are left out.
    pub fn marks(
        &self,
        view: &CameraView,
        eye: DVec3,
        size: UVec2,
        beams: impl IntoIterator<Item = (i64, i64, Beam)>,
        out: &mut Vec<OverlayRect>,
    ) {
        let r = self.radius.max(0.0);
        let step = i64::from(self.beam_step.max(1));
        let near = |x: i64, y: i64, z: i64| {
            let d = DVec3::new(x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5) - eye;
            d.length_squared() <= r * r
        };
        let (y_lo, y_hi) = ((eye.y - r).floor() as i64, (eye.y + r).ceil() as i64);
        let start = out.len();
        let mut sources = Vec::new();
        for (x, z, beam) in beams {
            let (dx, dz) = (x as f64 + 0.5 - eye.x, z as f64 + 0.5 - eye.z);
            if dx * dx + dz * dz > r * r {
                continue;
            }
            if near(x, beam.source, z) {
                sources.push(BlockPos::new(x, beam.source, z));
            }
            // The beam's cells near the eye, on its grid of `step` from `lo`.
            let lo = beam.lo.max(y_lo);
            let first = lo + (beam.lo - lo).rem_euclid(step);
            let mut y = first;
            while y <= beam.hi.min(y_hi) && out.len() - start < MAX_MARKS {
                if y != beam.source
                    && near(x, y, z)
                    && let Some(at) = project(view, size, BlockPos::new(x, y, z))
                {
                    out.push(square(at, self.beam_size, size, self.beam_color));
                }
                y += step;
            }
        }
        for pos in sources {
            if out.len() - start >= MAX_MARKS {
                break;
            }
            if let Some(at) = project(view, size, pos) {
                out.push(square(at, self.source_size, size, self.source_color));
            }
        }
    }

    /// The marks of the beam tiles of `sky` around `eye` ([`SkySourceMarkers::marks`]).
    pub fn marks_in(
        &self,
        sky: &SkyMap,
        view: &CameraView,
        eye: DVec3,
        size: UVec2,
        out: &mut Vec<OverlayRect>,
    ) {
        let r = self.radius.max(0.0);
        let chunk = |v: f64| (v.floor() as i64) >> CHUNK_SHIFT;
        let (x0, x1) = (chunk(eye.x - r), chunk(eye.x + r));
        let (z0, z1) = (chunk(eye.z - r), chunk(eye.z + r));
        let beams = (x0..=x1)
            .flat_map(|cx| (z0..=z1).map(move |cz| (cx, cz)))
            .filter_map(|(cx, cz)| Some((cx, cz, sky.beam_tile(cx, cz)?)))
            .flat_map(|(cx, cz, tile)| {
                tile.iter().map(move |(x, z, beam)| {
                    let wx = (cx << CHUNK_SHIFT) + i64::from(x);
                    let wz = (cz << CHUNK_SHIFT) + i64::from(z);
                    (wx, wz, beam)
                })
            });
        self.marks(view, eye, size, beams, out);
    }
}
