//! The glass scene of the colored sky (ADR 0064): [`GlassHouse`], a greenhouse under a red glass
//! roof with a black skylight and a lamp, and a room lit through a light blue window, built on
//! the ground; [`GlassHouseWorld`], any world with one, for the demos and the `glass-house` perf
//! case.

use crate::blocks::STAINED_GLASS;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, LocalPos};
use boxel_worldgen::{LodSource, RealmStack, WorldGenerator, WorldQuery};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;

/// The greenhouse: blocks from the center column to the inner side of its walls along x and z.
pub const GREENHOUSE_HALF: [i64; 2] = [8, 6];
/// The roof's row above the floor (red glass, the walls end under it).
pub const ROOF: i64 = 7;
/// The black skylight in the roof: its corner columns (x, z from the center), inclusive.
pub const SKYLIGHT: [(i64, i64); 2] = [(2, -5), (5, -2)];
/// The lamp under the roof (x, row, z from the center floor).
pub const LAMP: (i64, i64, i64) = (-4, ROOF - 1, -3);
/// The window room, east of the greenhouse and sharing its east wall: its walls along x
/// (inclusive).
pub const ROOM_X: [i64; 2] = [9, 17];
/// The window room's half width along z, to the inner side of its walls.
pub const ROOM_HALF_Z: i64 = 3;
/// The room's roof row (planks).
pub const ROOM_ROOF: i64 = 6;
/// The light blue window in the room's east wall: its rows (inclusive).
pub const WINDOW_ROWS: [i64; 2] = [2, 4];
/// The window's half width along z.
pub const WINDOW_HALF_Z: i64 = 1;
/// The yard east of the room, so the window faces open air: its last column along x.
const YARD_X: i64 = 21;
/// Rows of air cleared over the whole scene above the roof (trees, a hill).
const CLEAR_ABOVE: i64 = 12;
/// Deepest foundation under the floor.
const FOUNDATION: i64 = 16;

/// The walls.
pub const WALL: &str = "boxel:cobblestone";
/// The foundations.
pub const STONE: &str = "boxel:stone";
/// The floor of the greenhouse and the yard.
pub const GRASS: &str = "boxel:grass";
/// The floor and the roof of the window room.
pub const PLANKS: &str = "boxel:planks";
/// Every cleared cell.
pub const AIR: &str = "boxel:air";
/// The lamp under the greenhouse roof.
pub const LAMP_BLOCK: &str = "boxel:lamp_white";
/// The flowers of the bed.
pub const POPPY: &str = "boxel:poppy";
/// The grass of the bed.
pub const SHORT_GRASS: &str = "boxel:short_grass";
/// Every pane of the [`Glass::Clear`] twin.
pub const CLEAR_GLASS: &str = "boxel:glass";
/// The greenhouse roof.
pub const RED: &str = STAINED_GLASS[0].name;
/// The window of the room.
pub const LIGHT_BLUE: &str = STAINED_GLASS[1].name;
/// The skylight.
pub const BLACK: &str = STAINED_GLASS[2].name;

/// The glass of the scene.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Glass {
    /// Red roof, black skylight, light blue window (the registry needs
    /// [`add_stained_glass`](crate::blocks::add_stained_glass)).
    #[default]
    Stained,
    /// The same scene with common glass everywhere: the clear twin of the perf case.
    Clear,
}

impl Glass {
    /// The block of a pane of `stained` glass.
    fn pane(self, stained: &'static str) -> &'static str {
        match self {
            Glass::Stained => stained,
            Glass::Clear => CLEAR_GLASS,
        }
    }
}

/// Where a camera of the scene looks from ([`GlassHouse::camera`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassView {
    /// In the greenhouse by its south wall, looking north at the bed, the skylight's shade and
    /// the lamp.
    Greenhouse,
    /// In the window room, looking east at the window and the floor it lights.
    WindowRoom,
}

impl GlassView {
    /// Every view, in order.
    pub const ALL: [GlassView; 2] = [GlassView::Greenhouse, GlassView::WindowRoom];

    /// The view's name (`greenhouse`, `window`).
    pub fn name(self) -> &'static str {
        match self {
            GlassView::Greenhouse => "greenhouse",
            GlassView::WindowRoom => "window",
        }
    }

    /// The view named `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == name)
    }
}

/// The glass scene around one column, on its ground:
///
/// - a greenhouse of cobblestone walls ([`GREENHOUSE_HALF`] to their inner side, rows 1 to 6)
///   on a grass floor, under a red glass roof ([`ROOF`]) with a black skylight ([`SKYLIGHT`]),
///   a white lamp hanging under the roof ([`LAMP`]) and a bed of poppies and grass, closed (a
///   door would let the white sky in by its side);
/// - a closed room east of it ([`ROOM_X`]) with a planks floor and roof, lit only through a
///   light blue window in its east wall;
/// - a yard east of the room so the window faces open air, every column on a stone foundation
///   down to its own ground, and the air above the scene cleared.
#[derive(Clone, Debug, PartialEq)]
pub struct GlassHouse {
    /// The floor block of the center column (on the ground of that column).
    pub floor: BlockPos,
    /// Every cell, with the name of its block (air included: the scene is cleared).
    pub cells: Vec<(BlockPos, &'static str)>,
}

/// The highest block `is_ground` in column `(x, z)` from `top` down `depth` blocks. `None`
/// while a block on the way is not loaded, `Some(None)` without ground in reach.
fn ground(
    (x, z): (i64, i64),
    top: i64,
    depth: i64,
    block_at: &impl Fn(BlockPos) -> Option<BlockState>,
    is_ground: &impl Fn(BlockState) -> bool,
) -> Option<Option<i64>> {
    for y in (top.saturating_sub(depth)..=top).rev() {
        if is_ground(block_at(BlockPos::new(x, y, z))?) {
            return Some(Some(y));
        }
    }
    Some(None)
}

/// What a scene cell is, from its offset to the center floor (`row` 0 is the floor).
fn cell(x: i64, row: i64, z: i64, glass: Glass) -> &'static str {
    let [hx, hz] = GREENHOUSE_HALF;
    let in_greenhouse = x.abs() <= hx + 1 && z.abs() <= hz + 1;
    let in_room = (ROOM_X[0] + 1..=ROOM_X[1]).contains(&x) && z.abs() <= ROOM_HALF_Z + 1;
    if in_greenhouse {
        let wall = x.abs() == hx + 1 || z.abs() == hz + 1;
        return match row {
            0 if wall => WALL,
            0 => GRASS,
            ROOF => {
                let [(x0, z0), (x1, z1)] = SKYLIGHT;
                if (x0..=x1).contains(&x) && (z0..=z1).contains(&z) {
                    glass.pane(BLACK)
                } else {
                    glass.pane(RED)
                }
            }
            r if r > ROOF => AIR,
            _ if wall => WALL,
            r if (x, r, z) == LAMP => LAMP_BLOCK,
            // The bed: west of the center, a poppy in every other cell and grass between.
            1 if (-hx + 1..=-2).contains(&x) && (1..=hz - 1).contains(&z) => {
                if (x + z).rem_euclid(2) == 0 {
                    POPPY
                } else {
                    SHORT_GRASS
                }
            }
            _ => AIR,
        };
    }
    if in_room {
        let wall = x == ROOM_X[1] || z.abs() == ROOM_HALF_Z + 1;
        return match row {
            0 | ROOM_ROOF => PLANKS,
            r if r > ROOM_ROOF => AIR,
            r if wall => {
                let window = x == ROOM_X[1]
                    && z.abs() <= WINDOW_HALF_Z
                    && (WINDOW_ROWS[0]..=WINDOW_ROWS[1]).contains(&r);
                if window { glass.pane(LIGHT_BLUE) } else { WALL }
            }
            _ => AIR,
        };
    }
    if row == 0 { GRASS } else { AIR }
}

impl GlassHouse {
    /// The scene around column `(x, z)`: its floor replaces the ground of that column (the first
    /// block `is_ground` from `top` down `depth`), so it stands on the ground whatever grows on
    /// it. `None` while a block on the way is not loaded (try again later), `Some(None)` when
    /// the center column has no ground in reach.
    pub fn around(
        (cx, cz): (i64, i64),
        top: i64,
        depth: i64,
        glass: Glass,
        block_at: impl Fn(BlockPos) -> Option<BlockState>,
        is_ground: impl Fn(BlockState) -> bool,
    ) -> Option<Option<Self>> {
        let Some(floor) = ground((cx, cz), top, depth, &block_at, &is_ground)? else {
            return Some(None);
        };
        let [hx, hz] = GREENHOUSE_HALF;
        let mut cells = Vec::new();
        for x in -hx - 1..=YARD_X {
            for z in -hz - 1..=hz + 1 {
                let (wx, wz) = (cx + x, cz + z);
                let at = |y: i64| BlockPos::new(wx, y, wz);
                // The foundation, from the column's ground (or as deep as it goes) to the floor.
                let base = ground((wx, wz), floor - 1, FOUNDATION, &block_at, &is_ground)?
                    .unwrap_or(floor - FOUNDATION - 1);
                cells.extend((base + 1..floor).map(|y| (at(y), STONE)));
                for row in 0..=ROOF + CLEAR_ABOVE {
                    cells.push((at(floor + row), cell(x, row, z, glass)));
                }
            }
        }
        Some(Some(Self {
            floor: BlockPos::new(cx, floor, cz),
            cells,
        }))
    }

    /// Whether `s` is ground for the scene in `registry`: an opaque cube but a log, so the
    /// scene goes under a tree.
    pub fn ground_test(registry: &BlockRegistry) -> impl Fn(BlockState) -> bool + '_ {
        let logs: Vec<BlockState> = registry
            .block_by_name("boxel:log")
            .map(|log| registry.states(log).collect())
            .unwrap_or_default();
        move |s| registry.occludes(s) && !logs.contains(&s)
    }

    /// The camera of `view`: x, y, z, yaw and pitch (degrees, as `--camera`: yaw 0 looks north
    /// along -z, -90 east along +x), standing on the floor.
    pub fn camera(&self, view: GlassView) -> [f64; 5] {
        let f = self.floor;
        let eye = |x: i64, z: i64| {
            [
                (f.x + x) as f64 + 0.5,
                f.y as f64 + 2.6,
                (f.z + z) as f64 + 0.5,
            ]
        };
        let ([x, y, z], yaw, pitch) = match view {
            GlassView::Greenhouse => (eye(1, GREENHOUSE_HALF[1]), 0.0, -8.0),
            GlassView::WindowRoom => (eye(ROOM_X[0] + 2, 0), -90.0, -15.0),
        };
        [x, y, z, yaw, pitch]
    }
}

/// `inner` with a [`GlassHouse`] built into its chunks: the world of `headless_world`,
/// `stress --stained-glass` and the `glass-house` perf case. Chunks the scene touches get its
/// cells over `inner`'s; the rest, the LOD, the realms and the queries are `inner`'s. The
/// registry must hold the scene's blocks ([`registry_with_stained_glass`] for
/// [`Glass::Stained`]); a cell whose block is missing keeps `inner`'s.
///
/// [`registry_with_stained_glass`]: crate::blocks::registry_with_stained_glass
pub struct GlassHouseWorld {
    /// The world the scene is built in.
    pub inner: Arc<dyn WorldGenerator>,
    /// The scene, `None` when its column had no ground in reach.
    pub house: Option<GlassHouse>,
    /// The scene's cells, by chunk.
    cells: HashMap<ChunkPos, Vec<(LocalPos, &'static str)>>,
    /// `inner`'s layers, raised to the scene's top.
    layers: RangeInclusive<i64>,
}

impl GlassHouseWorld {
    /// Blocks searched for ground under the top of `inner`'s layers.
    pub const GROUND_DEPTH: i64 = 512;

    /// `inner` with the scene of `glass` on the ground of column `(x, z)`, found in `inner`'s
    /// own chunks from the top of its layers down [`Self::GROUND_DEPTH`] blocks
    /// ([`GlassHouse::ground_test`]).
    pub fn around(
        inner: Arc<dyn WorldGenerator>,
        registry: &BlockRegistry,
        (x, z): (i64, i64),
        glass: Glass,
    ) -> Self {
        let layers = inner.vertical_chunks();
        let top = layers.end().saturating_add(1).saturating_mul(32) - 1;
        let chunks: RefCell<HashMap<ChunkPos, ChunkData>> = RefCell::default();
        let block_at = |p: BlockPos| {
            let (chunk, local) = p.split();
            let mut chunks = chunks.borrow_mut();
            let data = chunks
                .entry(chunk)
                .or_insert_with(|| inner.generate(ChunkKey::new(chunk), registry));
            Some(data.get(local))
        };
        let house = GlassHouse::around(
            (x, z),
            top,
            Self::GROUND_DEPTH,
            glass,
            block_at,
            GlassHouse::ground_test(registry),
        )
        .flatten();
        let mut cells: HashMap<ChunkPos, Vec<(LocalPos, &'static str)>> = HashMap::new();
        let mut high = *layers.end();
        for &(pos, name) in house.iter().flat_map(|h| &h.cells) {
            let (chunk, local) = pos.split();
            high = high.max(chunk.y);
            cells.entry(chunk).or_default().push((local, name));
        }
        Self {
            layers: *layers.start()..=high,
            inner,
            house,
            cells,
        }
    }
}

impl WorldGenerator for GlassHouseWorld {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let mut chunk = self.inner.generate(key, registry);
        if key.lod != 0 {
            return chunk;
        }
        for &(local, name) in self.cells.get(&key.pos).into_iter().flatten() {
            if let Some(state) = registry.by_name(name) {
                chunk.set(local, state);
            }
        }
        chunk
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        self.layers.clone()
    }
    fn sky_source_layers(&self) -> RangeInclusive<i64> {
        self.inner.sky_source_layers()
    }
    fn lod(&self) -> Option<&dyn LodSource> {
        self.inner.lod()
    }
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn version(&self) -> u32 {
        self.inner.version()
    }
    fn realms(&self) -> &RealmStack {
        self.inner.realms()
    }
    fn as_query(&self) -> Option<&dyn WorldQuery> {
        self.inner.as_query()
    }
}
