//! Scheduled block ticks (ADR 0021): a block asks to be updated a number of game ticks later,
//! like Minecraft's scheduled ticks. Flowing water is the first user ([`crate::fluid`]).
//!
//! - The game clock runs at [`GAME_TICKS_PER_SECOND`] (20, Minecraft's), counted from the fixed
//!   steps (`FixedUpdate`, 64 Hz): the same at any frame rate.
//! - [`BlockTicks`] keeps the ticks to run as `(due tick, order)` in one ordered queue: every
//!   tick due at a game tick runs in the order it was scheduled, whatever chunk it is in, so a
//!   run is deterministic. A position is scheduled at most once (a later request while one
//!   waits is dropped, as in Minecraft).
//! - Each frame, between the world's interest and edit steps, the due ticks run within a time
//!   budget ([`BlockTickSettings`]): a big flood spreads over frames instead of stalling one.
//!   A late tick runs as if on time: what it schedules is due relative to its own due tick, so
//!   the result does not depend on the budget.
//! - A tick only runs when every chunk it may read is loaded (its handler's reach). Otherwise it
//!   freezes until that chunk loads (`ChunkDataEvent::Loaded`), then runs at once.
//! - What a tick writes goes through the world's [`EditQueue`] (one edit batch per frame: light,
//!   remeshes and `BlockChanged` as for any edit). Within the frame, later ticks read the
//!   writes of earlier ones from an overlay, as Minecraft reads its own `setBlock`.
//! - Every change notifies the block and its 6 neighbors: those with a tick handler are
//!   scheduled `delay` ticks later (Minecraft's neighbor updates and `onPlace`). A tick's own
//!   writes notify at its due tick; other edits (players, tools) at the current tick, from the
//!   frame's `BlockChanged` messages.
//! - A handler may also react at once to those changes ([`TickHandler::react`], lava meeting
//!   water): right inside the write for a tick's writes, at the start of the next frame's ticks
//!   for other edits.
//! - Random ticks (ADR 0041, Minecraft's `randomTickSpeed`): every game tick, each loaded chunk
//!   within [`BlockTickSettings::random_tick_radius`] of a `ChunkViewer` gets
//!   [`BlockTickSettings::random_ticks_per_chunk`] picks of a random cell; a pick on a state with
//!   a [`RandomTickHandler`] runs it through the same [`TickContext`], after the frame's due
//!   ticks. A chunk whose palette holds no such state gets no picks. The picks hash the game
//!   tick, the chunk and the pick's number, and [`TickContext::random`] the tick's block and
//!   game tick, so a run is deterministic.
//! - Light (ADR 0059): a tick reads a cell's light ([`TickContext::light`], sky and block level)
//!   and the brightness Minecraft's growth rules ask for ([`TickContext::brightness`], the sky
//!   level darkened by the time of day, [`BlockTickSettings::sky_darken`]). It is the light the
//!   chunk's last mesh or light job computed, kept within `WorldSettings::light_cache_radius`:
//!   after an edit that changes the light, the light from before until the remesh of the edit's
//!   batch lands. `None` where the chunk has no light yet; a world without light reads open sky.
use crate::streaming::ViewerChunks;
use crate::{
    BlockChanged, BlockEdit, ChunkDataEvent, ChunkMap, DaylightCurve, EditQueue, SkyMap, TimeOfDay,
    WorldContent,
};
use boxel_ecs::prelude::*;
use boxel_light::ChunkLight;
use boxel_voxel::{
    BlockId, BlockPos, BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkPos, Face,
    LightValue, LocalPos, MAX_LIGHT, Paletted,
};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

/// Game ticks per second (Minecraft's 20).
pub const GAME_TICKS_PER_SECOND: u64 = 20;

/// Fixed steps per second the clock counts (`boxel_app::FixedTime`'s default 64 Hz).
const FIXED_STEPS_PER_SECOND: u64 = crate::TICKS_PER_SECOND;

/// What a block does when its tick comes: `tick` runs `delay` game ticks after a change at or
/// next to the block, reading at most `reach` blocks away (horizontally, and down and up).
/// `react`, if any, runs at once on such a change, before the tick is scheduled.
#[derive(Clone, Copy, Debug)]
pub struct TickHandler {
    /// Game ticks from a change to the tick (at least 1).
    pub delay: u32,
    /// How far the tick reads: `[horizontal, down, up]` blocks. The tick waits until every
    /// chunk within reach is loaded.
    pub reach: [u32; 3],
    /// The update.
    pub tick: fn(&mut TickContext<'_>, BlockPos, BlockState),
    /// Minecraft's `neighborChanged` (lava turning to stone next to water, ADR 0025): runs when
    /// the block or one of its 6 neighbors changes, right then for a tick's writes and at the
    /// start of the next frame's ticks (before any due tick) for other edits. It reads only
    /// the block's 6 neighbors (`None` where a chunk is not loaded) and may write the block; if
    /// it does, the old state's tick is not scheduled.
    pub react: Option<fn(&mut TickContext<'_>, BlockPos, BlockState)>,
}

/// What a block does on a random tick (Minecraft's `randomTick`: a crop grows, farmland dries):
/// `tick` runs on a block a random pick landed on, reading at most `reach` blocks away
/// (`[horizontal, down, up]`). A pick whose reach is not all loaded is skipped (a random tick
/// does not wait).
#[derive(Clone, Copy, Debug)]
pub struct RandomTickHandler {
    /// How far the tick reads: `[horizontal, down, up]` blocks.
    pub reach: [u32; 3],
    /// The update.
    pub tick: fn(&mut TickContext<'_>, BlockPos, BlockState),
}

/// Knobs of the block ticks. Read every frame.
#[derive(Resource, Clone, Debug)]
pub struct BlockTickSettings {
    /// Main-thread time per frame for running due ticks. At least
    /// [`BlockTickSettings::min_ticks_per_frame`] run whatever the budget.
    pub budget: Duration,
    /// Due ticks that run every frame even past the budget (so the queue always moves).
    pub min_ticks_per_frame: usize,
    /// Stops the game clock (no tick comes due; edits still schedule).
    pub paused: bool,
    /// Random picks per chunk per game tick (Minecraft's `randomTickSpeed` of 3 per 16^3
    /// section is 24 per 32^3 chunk); 0 turns random ticks off.
    pub random_ticks_per_chunk: u32,
    /// The chunks around each viewer's that random tick: `[horizontal, vertical]` radius in
    /// chunks, a box (Minecraft's simulation distance; 4 is 128 blocks).
    pub random_tick_radius: [u32; 2],
    /// The most game ticks of random ticks one frame catches up after a stall (the rest are
    /// skipped, as a server that falls behind skips them).
    pub random_catch_up: u32,
    /// How many levels the time of day takes off the sky light ticks read
    /// ([`TickContext::brightness`], Minecraft's `skyDarken`: 0 by day, 11 at night), from the
    /// world clock and its curve. Asked once per frame, in `First` (before `Update`, where apps
    /// move the clock); 0 without a clock (no `DaylightPlugin`).
    /// The default is [`curve_sky_darken`].
    pub sky_darken: fn(&TimeOfDay, &DaylightCurve) -> u8,
}

impl Default for BlockTickSettings {
    /// 1 ms per frame, at least 64 ticks, running; 24 random picks per chunk within 4 chunks,
    /// catching up 1 second.
    fn default() -> Self {
        Self {
            budget: Duration::from_millis(1),
            min_ticks_per_frame: 64,
            paused: false,
            random_ticks_per_chunk: 24,
            random_tick_radius: [4, 4],
            random_catch_up: GAME_TICKS_PER_SECOND as u32,
            sky_darken: curve_sky_darken,
        }
    }
}

/// The default [`BlockTickSettings::sky_darken`]: the curve's sky light at `time` (its
/// brightest channel, 1 at noon) taken off the full 15 levels, rounded. The default curve gives
/// 0 by day, 6 with the sun on the horizon and 12 at night.
pub fn curve_sky_darken(time: &TimeOfDay, curve: &DaylightCurve) -> u8 {
    let sky = curve.at(time).sky_light;
    let bright = sky.iter().copied().fold(0.0f32, f32::max).clamp(0.0, 1.0);
    ((1.0 - bright) * f32::from(MAX_LIGHT)).round() as u8
}

/// The light of a cell as a tick reads it (ADR 0059): the sky level and the block level, the
/// brightest of the block light's red, green and blue (Minecraft's block light has one level).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickLight {
    /// Sky level, 0 to 15 (15 under open sky, at any time of day).
    pub sky: u8,
    /// Block level, 0 to 15.
    pub block: u8,
}

impl TickLight {
    /// Open sky and no block light: what a world without light reads everywhere.
    pub const OPEN: TickLight = TickLight { sky: 15, block: 0 };

    /// The levels of a voxel's light.
    pub const fn of(light: LightValue) -> Self {
        let [r, g, b] = light.rgb();
        let rg = if r > g { r } else { g };
        TickLight {
            sky: light.sky(),
            block: if rg > b { rg } else { b },
        }
    }

    /// Minecraft's `getRawBrightness(pos, darken)`: the sky level less `darken` (at least 0) or
    /// the block level, the higher. With `darken` 0 the time of day does not count (crops grow
    /// at night under open sky).
    pub const fn raw(self, darken: u8) -> u8 {
        let sky = self.sky.saturating_sub(darken);
        if sky > self.block { sky } else { self.block }
    }
}

/// What the ticks of one frame know of the light: whether the world has any, and the time of
/// day's [`BlockTickSettings::sky_darken`].
#[derive(Clone, Copy, Debug, Default)]
struct LightInput {
    off: bool,
    darken: u8,
}

impl LightInput {
    fn new(sky: Option<&SkyMap>, ticks: &BlockTicks) -> Self {
        LightInput {
            off: sky.is_some_and(SkyMap::light_off),
            darken: ticks.sky_darken,
        }
    }
}

/// Counts of the block ticks, for overlays and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockTickStats {
    /// Ticks waiting in the queue (due or not).
    pub queued: usize,
    /// Ticks waiting for a chunk to load.
    pub frozen: usize,
    /// Ticks run in the last frame.
    pub ran: usize,
    /// Blocks written by the last frame's ticks.
    pub written: usize,
    /// Due ticks left for the next frame by the budget.
    pub behind: usize,
    /// Random picks that ran a handler in the last frame.
    pub random: usize,
}

/// The ordered tick queue and its index.
#[derive(Debug, Default)]
struct Queue {
    /// `(due, order) -> position`.
    ticks: BTreeMap<(u64, u64), BlockPos>,
    /// Position -> its key in `ticks`, or `None` while frozen.
    scheduled: HashMap<BlockPos, Option<(u64, u64)>, foldhash::fast::FixedState>,
    /// Frozen ticks per chunk they wait for.
    frozen: HashMap<ChunkPos, Vec<BlockPos>, foldhash::fast::FixedState>,
    frozen_count: usize,
    next_order: u64,
}

impl Queue {
    /// Schedules `pos` at `due` unless it already waits.
    fn schedule(&mut self, pos: BlockPos, due: u64) {
        if self.scheduled.contains_key(&pos) {
            return;
        }
        let key = (due, self.next_order);
        self.next_order += 1;
        self.ticks.insert(key, pos);
        self.scheduled.insert(pos, Some(key));
    }

    /// Takes the first tick if it is due at `now`.
    fn pop_due(&mut self, now: u64) -> Option<(u64, BlockPos)> {
        let entry = self.ticks.first_entry()?;
        if entry.key().0 > now {
            return None;
        }
        let ((due, _), pos) = entry.remove_entry();
        self.scheduled.remove(&pos);
        Some((due, pos))
    }

    /// Freezes the tick of `pos` until chunk `wait` loads.
    fn freeze(&mut self, pos: BlockPos, wait: ChunkPos) {
        if self.scheduled.contains_key(&pos) {
            return;
        }
        self.scheduled.insert(pos, None);
        self.frozen.entry(wait).or_default().push(pos);
        self.frozen_count += 1;
    }

    /// Requeues the ticks frozen on `chunk`, due at `now`, in the order they froze.
    fn thaw(&mut self, chunk: ChunkPos, now: u64) {
        let Some(list) = self.frozen.remove(&chunk) else {
            return;
        };
        self.frozen_count -= list.len();
        for pos in list {
            self.scheduled.remove(&pos);
            self.schedule(pos, now);
        }
    }

    fn due_count(&self, now: u64) -> usize {
        self.ticks.range(..(now + 1, 0)).count()
    }
}

/// The scheduled block ticks: the game clock, the queue and the tick handlers (see the
/// [module docs](self)). Added by [`BlockTicksPlugin`].
#[derive(Resource, Debug, Default)]
pub struct BlockTicks {
    /// Current game tick.
    now: u64,
    /// Fixed steps not yet turned into a game tick, times [`GAME_TICKS_PER_SECOND`].
    fraction: u64,
    queue: Queue,
    /// Handler per block state (empty until a handler is registered).
    handlers: Vec<Option<TickHandler>>,
    /// Random tick handler per block state (empty until one is registered).
    random: Vec<Option<RandomTickHandler>>,
    /// The last game tick whose random ticks ran.
    random_done: u64,
    /// The last frame's writes (final state per position), to tell their `BlockChanged`
    /// echoes from other edits.
    echoes: HashMap<BlockPos, BlockState, foldhash::fast::FixedState>,
    /// Other edits whose blocks or neighbors react ([`TickHandler::react`]), in order, with the
    /// game tick they happened at: run at the start of the next `run_ticks`.
    reactions: Vec<(BlockPos, u64)>,
    /// The cells whose data the running ticks keep through a change of their block
    /// ([`TickContext::keep_data`]), queued with the writes.
    kept: Vec<BlockPos>,
    /// The frame's [`BlockTickSettings::sky_darken`] (`sample_sky_darken`, in `First`).
    sky_darken: u8,
    stats: BlockTickStats,
}

impl BlockTicks {
    /// The current game tick.
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Back to a new world's ticks (a world's close; its pending ticks were saved): the clock at
    /// 0, the queue, the echoes, the reactions and the counts empty. The handlers stay (they
    /// are the app's).
    pub fn clear_world(&mut self) {
        self.now = 0;
        self.fraction = 0;
        self.queue = Queue::default();
        self.echoes.clear();
        self.reactions.clear();
        self.kept.clear();
        self.stats = BlockTickStats::default();
        self.random_done = 0;
    }

    /// Nothing of a world: the clock at 0 and nothing queued, frozen or waiting to react.
    pub(crate) fn is_world_clear(&self) -> bool {
        self.now == 0
            && self.fraction == 0
            && self.queue.ticks.is_empty()
            && self.queue.scheduled.is_empty()
            && self.queue.frozen.is_empty()
            && self.echoes.is_empty()
            && self.reactions.is_empty()
            && self.random_done == 0
    }

    /// Moves the game clock `ticks` forward (tests and tools; the plugin advances it from the
    /// fixed steps).
    pub fn advance(&mut self, ticks: u64) {
        self.now += ticks;
    }

    /// One fixed step of the clock: [`GAME_TICKS_PER_SECOND`] game ticks per
    /// `FIXED_STEPS_PER_SECOND` steps, exactly (integer).
    fn fixed_step(&mut self) {
        self.fraction += GAME_TICKS_PER_SECOND;
        self.now += self.fraction / FIXED_STEPS_PER_SECOND;
        self.fraction %= FIXED_STEPS_PER_SECOND;
    }

    /// Registers `handler` for every state of `block` (replaces an earlier one).
    pub fn register(&mut self, registry: &BlockRegistry, block: BlockId, handler: TickHandler) {
        if self.handlers.len() < registry.len() {
            self.handlers.resize(registry.len(), None);
        }
        let handler = TickHandler {
            delay: handler.delay.max(1),
            ..handler
        };
        for state in registry.states(block) {
            self.handlers[usize::from(state.0)] = Some(handler);
        }
    }

    /// Registers `handler` for `state` alone (replaces an earlier one): the water of a
    /// waterlogged block, for instance, ticks while the dry states of its block do not.
    pub fn register_state(
        &mut self,
        registry: &BlockRegistry,
        state: BlockState,
        handler: TickHandler,
    ) {
        if self.handlers.len() < registry.len() {
            self.handlers.resize(registry.len(), None);
        }
        if let Some(slot) = self.handlers.get_mut(usize::from(state.0)) {
            *slot = Some(TickHandler {
                delay: handler.delay.max(1),
                ..handler
            });
        }
    }

    /// The handler of `state`, if any.
    pub fn handler(&self, state: BlockState) -> Option<TickHandler> {
        self.handlers.get(usize::from(state.0)).copied().flatten()
    }

    /// Registers the random tick `handler` for every state of `block` (replaces an earlier one).
    pub fn register_random(
        &mut self,
        registry: &BlockRegistry,
        block: BlockId,
        handler: RandomTickHandler,
    ) {
        if self.random.len() < registry.len() {
            self.random.resize(registry.len(), None);
        }
        for state in registry.states(block) {
            self.random[usize::from(state.0)] = Some(handler);
        }
    }

    /// The random tick handler of `state`, if any.
    pub fn random_handler(&self, state: BlockState) -> Option<RandomTickHandler> {
        self.random.get(usize::from(state.0)).copied().flatten()
    }

    /// Schedules a tick of `pos` `delay` game ticks from now (unless one already waits).
    pub fn schedule(&mut self, pos: BlockPos, delay: u32) {
        self.queue.schedule(pos, self.now + u64::from(delay));
    }

    /// True if a tick of `pos` waits (queued or frozen).
    pub fn is_scheduled(&self, pos: BlockPos) -> bool {
        self.queue.scheduled.contains_key(&pos)
    }

    /// Every tick waiting, as `(position, game ticks from now)` in the order they run: first
    /// the ticks frozen until a chunk loads (already due: 0), by chunk and in the order they
    /// froze, then the queue. What a saved world keeps (ADR 0024, [`BlockTicks::restore`]).
    pub fn pending(&self) -> Vec<(BlockPos, u64)> {
        let mut frozen: Vec<(&ChunkPos, &Vec<BlockPos>)> = self.queue.frozen.iter().collect();
        frozen.sort_unstable_by_key(|(chunk, _)| **chunk);
        let mut out = Vec::with_capacity(self.queue.ticks.len() + self.queue.frozen_count);
        out.extend(
            frozen
                .into_iter()
                .flat_map(|(_, list)| list.iter().map(|&pos| (pos, 0))),
        );
        out.extend(
            self.queue
                .ticks
                .iter()
                .map(|(&(due, _), &pos)| (pos, due.saturating_sub(self.now))),
        );
        out
    }

    /// Schedules saved ticks, `(position, game ticks from now)` in the order they run, as
    /// [`BlockTicks::pending`] gives them: each one that many game ticks after the current one
    /// (a position that already waits keeps its tick). Those due in chunks that are not loaded
    /// yet freeze until the chunk loads, like any tick.
    pub fn restore(&mut self, ticks: impl IntoIterator<Item = (BlockPos, u64)>) {
        for (pos, delay) in ticks {
            self.queue.schedule(pos, self.now.saturating_add(delay));
        }
    }

    /// The counts.
    pub fn stats(&self) -> BlockTickStats {
        BlockTickStats {
            queued: self.queue.ticks.len(),
            frozen: self.queue.frozen_count,
            ..self.stats
        }
    }

    /// Schedules the blocks at and around `pos` that have a handler, `delay` after `at`, as a
    /// change at `pos` does, and keeps `pos` for the reactions of the next frame if one of
    /// those blocks reacts. `state_at` reads the blocks.
    fn notify(
        &mut self,
        pos: BlockPos,
        at: u64,
        state_at: impl Fn(BlockPos) -> Option<BlockState>,
    ) {
        let mut reacts = false;
        for p in around(pos) {
            if let Some(handler) = state_at(p).and_then(|s| self.handler(s)) {
                reacts |= handler.react.is_some();
                self.queue.schedule(p, at + u64::from(handler.delay));
            }
        }
        if reacts {
            self.reactions.push((pos, at));
        }
    }
}

/// `pos` and its 6 neighbors, in the order a change notifies them (`Face::ALL`).
fn around(pos: BlockPos) -> impl Iterator<Item = BlockPos> {
    std::iter::once(pos).chain(Face::ALL.map(|f| offset(pos, f)))
}

/// `pos` moved one block across `face`.
pub(crate) fn offset(pos: BlockPos, face: Face) -> BlockPos {
    let n = face.normal();
    BlockPos::new(
        pos.x + i64::from(n.x),
        pos.y + i64::from(n.y),
        pos.z + i64::from(n.z),
    )
}

/// What a running tick sees and changes: the world as of the tick (the map plus the writes of
/// the ticks before it this frame) and the queue.
pub struct TickContext<'a> {
    map: &'a ChunkMap,
    registry: &'a BlockRegistry,
    overlay: &'a mut HashMap<BlockPos, BlockState, foldhash::fast::FixedState>,
    writes: &'a mut Vec<BlockEdit>,
    ticks: &'a mut BlockTicks,
    /// The tick's due game tick.
    tick: u64,
    /// The last chunk read (a tick reads many blocks of one or two chunks).
    cache: ChunkCache<'a>,
    /// The state of [`TickContext::random`].
    rng: u64,
    /// The frame's light rule and darkening.
    light: LightInput,
    /// The last chunk whose light was read (`None` inside: it has none yet).
    light_cache: Cell<Option<(ChunkPos, Option<&'a ChunkLight>)>>,
}

/// The last chunk a [`TickContext`] read, so most reads skip the map's hash lookup.
type ChunkCache<'a> = Cell<Option<(ChunkPos, &'a ChunkData)>>;

/// The block at `pos` in `map` (`None` if its chunk is not loaded), through `cache`.
#[inline]
fn map_block<'a>(map: &'a ChunkMap, cache: &ChunkCache<'a>, pos: BlockPos) -> Option<BlockState> {
    let (chunk, local) = pos.split();
    let data = match cache.get() {
        Some((c, data)) if c == chunk => data,
        _ => {
            let data: &'a ChunkData = map.data_at(chunk)?;
            cache.set(Some((chunk, data)));
            data
        }
    };
    Some(data.get(local))
}

impl<'a> TickContext<'a> {
    /// The data of the block at `pos` ([`boxel_voxel::BlockData`], ADR 0048), as the map holds
    /// it before this frame's edits. Read only: a handler writes blocks; data is written by
    /// systems, through the [`EditQueue`].
    pub fn block_data(&self, pos: BlockPos) -> Option<&'a boxel_voxel::BlockData> {
        self.map.block_data_at(pos)
    }

    /// Keeps the data of `pos` through this tick's change of its block to another block (a
    /// copper chest that oxidizes keeps its items, ADR 0048): [`EditQueue::keep_data`] in the
    /// batch of the tick's writes.
    pub fn keep_data(&mut self, pos: BlockPos) {
        self.ticks.kept.push(pos);
    }

    /// The light at `pos` (ADR 0059): the chunk's light as its last mesh or light job computed
    /// it, so after an edit that changes the light it is the light from before until the
    /// remesh of the edit's batch lands (a few frames). `None` while the chunk has none: not
    /// loaded, not meshed or lit yet, or farther than `WorldSettings::light_cache_radius` from
    /// every viewer (keep the random tick radius inside it). A handler takes `None` as a check
    /// that fails (a crop does not grow this time). A world without light
    /// (`WorldSettings::light` off) reads [`TickLight::OPEN`] everywhere, as its meshes do. The
    /// writes of this frame's ticks do not change it.
    pub fn light(&self, pos: BlockPos) -> Option<TickLight> {
        if self.light.off {
            return Some(TickLight::OPEN);
        }
        let (chunk, local) = pos.split();
        let light = match self.light_cache.get() {
            Some((c, light)) if c == chunk => light,
            _ => {
                let map: &'a ChunkMap = self.map;
                let light = map
                    .id_at(chunk)
                    .and_then(|id| map.get(id))
                    .and_then(|slot| slot.light());
                self.light_cache.set(Some((chunk, light)));
                light
            }
        };
        light.map(|l| TickLight::of(l.get(local)))
    }

    /// Minecraft's `getMaxLocalRawBrightness`: the light at `pos` with the sky darkened by the
    /// time of day ([`TickLight::raw`] of [`TickContext::sky_darken`]). Saplings and grass ask
    /// for 9. `None` as [`TickContext::light`].
    pub fn brightness(&self, pos: BlockPos) -> Option<u8> {
        self.light(pos).map(|l| l.raw(self.light.darken))
    }

    /// How many levels the time of day takes off the sky light this frame
    /// ([`BlockTickSettings::sky_darken`]; 0 without a clock).
    pub fn sky_darken(&self) -> u8 {
        self.light.darken
    }

    /// The block at `pos` (`None` where the chunk is not loaded: outside the handler's reach).
    pub fn get(&self, pos: BlockPos) -> Option<BlockState> {
        if !self.overlay.is_empty()
            && let Some(&state) = self.overlay.get(&pos)
        {
            return Some(state);
        }
        map_block(self.map, &self.cache, pos)
    }

    /// Writes `state` at `pos` (nothing if it is already there) and notifies `pos` and its
    /// neighbors in order: each one with a tick handler first reacts ([`TickHandler::react`],
    /// which may write it in turn) and, unless that changed it, is scheduled its delay after
    /// this tick.
    pub fn set(&mut self, pos: BlockPos, state: BlockState) {
        if self.get(pos) == Some(state) {
            return;
        }
        self.overlay.insert(pos, state);
        self.writes.push(BlockEdit { pos, state });
        for p in around(pos) {
            self.notify(p, true);
        }
    }

    /// A change at or next to `p`: its reaction (if `react`) and the schedule of its tick.
    fn notify(&mut self, p: BlockPos, react: bool) {
        let Some(state) = self.get(p) else {
            return;
        };
        let Some(handler) = self.ticks.handler(state) else {
            return;
        };
        if let (true, Some(react)) = (react, handler.react) {
            react(self, p, state);
            if self.get(p) != Some(state) {
                return;
            }
        }
        self.ticks
            .queue
            .schedule(p, self.tick + u64::from(handler.delay));
    }

    /// The reactions of a change at `pos` made by another edit (already scheduled).
    fn react_around(&mut self, pos: BlockPos) {
        for p in around(pos) {
            let Some(state) = self.get(p) else {
                continue;
            };
            if let Some(react) = self.ticks.handler(state).and_then(|h| h.react) {
                react(self, p, state);
            }
        }
    }

    /// Schedules a tick of `pos` `delay` game ticks after this one (unless one already waits).
    pub fn schedule(&mut self, pos: BlockPos, delay: u32) {
        self.ticks
            .queue
            .schedule(pos, self.tick + u64::from(delay.max(1)));
    }

    /// The registry.
    pub fn registry(&self) -> &'a BlockRegistry {
        self.registry
    }

    /// The game tick this tick was due at.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// A random number in `0..n` (`0..1` for 0): from the tick's block and game tick and the
    /// calls before it in the tick, so a run is deterministic.
    pub fn random(&mut self, n: u32) -> u32 {
        self.rng = mix(self.rng);
        (((self.rng >> 32) * u64::from(n.max(1))) >> 32) as u32
    }
}

/// Splitmix64's finalizer: the hash of the random picks and of [`TickContext::random`].
fn mix(x: u64) -> u64 {
    let mut x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The seed of [`TickContext::random`] for a tick of `pos` at game tick `tick`.
fn seed_of(pos: BlockPos, tick: u64) -> u64 {
    mix(mix(mix(pos.x as u64) ^ pos.y as u64) ^ pos.z as u64) ^ mix(tick)
}

/// The seed of the random picks of `chunk` at game tick `tick`.
fn chunk_seed(chunk: ChunkPos, tick: u64) -> u64 {
    mix(mix(mix(mix(chunk.x as u64) ^ chunk.y as u64) ^ chunk.z as u64) ^ tick)
}

/// Whether `data` may hold a state with a random tick handler: a palette scan (a direct chunk
/// always may).
fn may_random_tick(data: &ChunkData, random: &[Option<RandomTickHandler>]) -> bool {
    let ticks = |s: BlockState| random.get(usize::from(s.0)).is_some_and(Option::is_some);
    match data.blocks() {
        Paletted::Uniform(s) => ticks(*s),
        Paletted::Packed { palette, .. } => palette.iter().any(|&s| ticks(s)),
        Paletted::Direct(_) => true,
    }
}

/// Adds [`BlockTicks`], [`BlockTickSettings`], the game clock (in `FixedUpdate`) and the
/// systems that run the due ticks (after `WorldSystems::Interest`, before
/// `WorldSystems::Edits`) and schedule ticks from other edits (after `WorldSystems::Edits`).
/// Needs [`WorldPlugin`](crate::WorldPlugin) first.
pub struct BlockTicksPlugin;

impl boxel_app::Plugin for BlockTicksPlugin {
    fn build(&self, app: &mut boxel_app::App) {
        use crate::WorldSystems;
        use crate::lifecycle::{TeardownSystems, WorldScopeApp, WorldTeardown, world_open};
        app.init_resource::<BlockTicks>()
            .init_resource::<BlockTickSettings>()
            .world_resource::<BlockTicks>(BlockTicks::is_world_clear)
            .add_systems(WorldTeardown, clear_ticks.in_set(TeardownSystems::Ticks))
            // A closed world's clock stands (the next world starts at 0).
            .add_systems(boxel_app::FixedUpdate, advance_clock.run_if(world_open))
            .add_systems(
                boxel_app::First,
                sample_sky_darken.after(boxel_ecs::message::MessageUpdateSystems),
            )
            .add_systems(
                boxel_app::Update,
                (
                    run_ticks
                        .after(WorldSystems::Interest)
                        .before(WorldSystems::Edits),
                    run_random_ticks
                        .after(run_ticks)
                        .before(WorldSystems::Edits),
                    schedule_from_edits
                        .after(WorldSystems::Edits)
                        .before(WorldSystems::Dispatch),
                ),
            )
            // Like the other board lines: its order does not matter.
            .add_systems(boxel_app::Last, publish_tick_stats.ambiguous_with_all());
    }
}

/// The `"ticks"` line of the `DiagnosticsBoard`, when one is present and the counts changed.
fn publish_tick_stats(
    ticks: Res<BlockTicks>,
    board: Option<ResMut<boxel_diagnostic::DiagnosticsBoard>>,
    mut last: Local<Option<BlockTickStats>>,
) {
    let Some(mut board) = board else {
        return;
    };
    let stats = ticks.stats();
    if *last == Some(stats) {
        return;
    }
    *last = Some(stats);
    board.set(
        "ticks",
        format!(
            "tick={} queued={} frozen={} ran={} written={} behind={} random={}",
            ticks.now(),
            stats.queued,
            stats.frozen,
            stats.ran,
            stats.written,
            stats.behind,
            stats.random
        ),
    );
}

/// `WorldTeardown`, `TeardownSystems::Ticks` ([`BlockTicks::clear_world`]).
fn clear_ticks(mut ticks: ResMut<BlockTicks>) {
    ticks.clear_world();
}

fn advance_clock(settings: Res<BlockTickSettings>, mut ticks: ResMut<BlockTicks>) {
    if !settings.paused {
        ticks.fixed_step();
    }
}

/// The chunks a tick at `pos` with `reach` reads, the first one not loaded.
fn missing_chunk(
    map: &ChunkMap,
    pos: BlockPos,
    [horizontal, down, up]: [u32; 3],
    loaded: &mut HashMap<ChunkPos, bool, foldhash::fast::FixedState>,
) -> Option<ChunkPos> {
    let (h, down, up) = (i64::from(horizontal), i64::from(down), i64::from(up));
    let lo = ChunkPos::containing(BlockPos::new(pos.x - h, pos.y - down, pos.z - h));
    let hi = ChunkPos::containing(BlockPos::new(pos.x + h, pos.y + up, pos.z + h));
    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                let c = ChunkPos::new(x, y, z);
                if !*loaded.entry(c).or_insert_with(|| map.data_at(c).is_some()) {
                    return Some(c);
                }
            }
        }
    }
    None
}

/// `First`: the frame's [`BlockTickSettings::sky_darken`] from the world clock, kept for the
/// ticks of the frame. Sampled before `Update`, where apps move the clock (keys, sleep), so the
/// ticks read no clock of their own.
fn sample_sky_darken(
    settings: Res<BlockTickSettings>,
    time: Option<Res<TimeOfDay>>,
    curve: Option<Res<DaylightCurve>>,
    mut ticks: ResMut<BlockTicks>,
) {
    let darken = time.map_or(0, |time| match curve {
        Some(curve) => (settings.sky_darken)(&time, &curve),
        None => (settings.sky_darken)(&time, &DaylightCurve::default()),
    });
    let darken = darken.min(MAX_LIGHT);
    if ticks.sky_darken != darken {
        ticks.sky_darken = darken;
    }
}

/// Runs the due ticks within the budget and queues their writes as one edit batch.
fn run_ticks(
    mut ticks: ResMut<BlockTicks>,
    mut queue: ResMut<EditQueue>,
    mut loads: MessageReader<ChunkDataEvent>,
    map: Res<ChunkMap>,
    content: Res<WorldContent>,
    settings: Res<BlockTickSettings>,
    sky: Option<Res<SkyMap>>,
) {
    let ticks = &mut *ticks;
    ticks.echoes.clear();
    for event in loads.read() {
        if let ChunkDataEvent::Loaded { key, .. } = event {
            let now = ticks.now;
            ticks.queue.thaw(key.pos, now);
        }
    }
    let now = ticks.now;
    ticks.stats.ran = 0;
    ticks.stats.written = 0;
    if ticks.reactions.is_empty()
        && ticks
            .queue
            .ticks
            .first_key_value()
            .is_none_or(|(k, _)| k.0 > now)
    {
        ticks.stats.behind = 0;
        return;
    }
    let start = Instant::now();
    let light = LightInput::new(sky.as_deref(), ticks);
    let mut overlay = HashMap::with_hasher(foldhash::fast::FixedState::default());
    let mut writes = Vec::new();
    let mut loaded = HashMap::with_hasher(foldhash::fast::FixedState::default());
    // The reactions to other edits, at the ticks they happened at, before any due tick.
    for (pos, at) in std::mem::take(&mut ticks.reactions) {
        let mut cx = TickContext {
            map: &map,
            registry: &content.registry,
            overlay: &mut overlay,
            writes: &mut writes,
            ticks: &mut *ticks,
            tick: at,
            cache: Cell::new(None),
            rng: seed_of(pos, at),
            light,
            light_cache: Cell::new(None),
        };
        cx.react_around(pos);
    }
    let mut ran = 0usize;
    loop {
        if ran >= settings.min_ticks_per_frame
            && ran.is_multiple_of(16)
            && start.elapsed() >= settings.budget
        {
            break;
        }
        let Some((due, pos)) = ticks.queue.pop_due(now) else {
            break;
        };
        ran += 1;
        let (chunk, local) = pos.split();
        let state = match overlay.get(&pos) {
            Some(&s) => s,
            None => match map.data_at(chunk) {
                Some(data) => data.get(local),
                None => {
                    ticks.queue.freeze(pos, chunk);
                    continue;
                }
            },
        };
        let Some(handler) = ticks.handler(state) else {
            continue;
        };
        if let Some(wait) = missing_chunk(&map, pos, handler.reach, &mut loaded) {
            ticks.queue.freeze(pos, wait);
            continue;
        }
        let mut cx = TickContext {
            map: &map,
            registry: &content.registry,
            overlay: &mut overlay,
            writes: &mut writes,
            ticks: &mut *ticks,
            tick: due,
            cache: Cell::new(None),
            rng: seed_of(pos, due),
            light,
            light_cache: Cell::new(None),
        };
        (handler.tick)(&mut cx, pos, state);
    }
    ticks.stats.ran = ran;
    ticks.stats.written = writes.len();
    ticks.stats.behind = ticks.queue.due_count(now);
    for edit in writes {
        queue.set_block(edit.pos, edit.state);
    }
    for pos in ticks.kept.drain(..) {
        queue.keep_data(pos);
    }
    ticks.echoes = overlay;
}

/// The random ticks of the game ticks since the last frame (at most
/// [`BlockTickSettings::random_catch_up`] of them) in the loaded chunks around every
/// `ChunkViewer` (see the [module docs](self)): they read the frame's due ticks' writes, and
/// theirs join the same edit batch.
fn run_random_ticks(
    mut ticks: ResMut<BlockTicks>,
    mut queue: ResMut<EditQueue>,
    map: Res<ChunkMap>,
    content: Res<WorldContent>,
    settings: Res<BlockTickSettings>,
    viewers: Option<Res<ViewerChunks>>,
    sky: Option<Res<SkyMap>>,
) {
    let ticks = &mut *ticks;
    let now = ticks.now;
    if ticks.random.is_empty() || settings.random_ticks_per_chunk == 0 {
        ticks.random_done = now;
        ticks.stats.random = 0;
        return;
    }
    if now <= ticks.random_done {
        ticks.random_done = ticks.random_done.min(now);
        ticks.stats.random = 0;
        return;
    }
    let from = ticks
        .random_done
        .max(now.saturating_sub(u64::from(settings.random_catch_up)));
    ticks.random_done = now;
    let [h, v] = settings.random_tick_radius.map(i64::from);
    let mut around = BTreeSet::new();
    let centers = viewers
        .iter()
        .flat_map(|v| v.0.iter().map(|(_, viewer)| viewer.center));
    for c in centers {
        for y in -v..=v {
            for z in -h..=h {
                for x in -h..=h {
                    around.insert(ChunkPos::new(c.x + x, c.y + y, c.z + z));
                }
            }
        }
    }
    let chunks: Vec<ChunkPos> = around
        .into_iter()
        .filter(|&c| {
            map.data_at(c)
                .is_some_and(|d| may_random_tick(d, &ticks.random))
        })
        .collect();
    let mut ran = 0usize;
    if !chunks.is_empty() {
        let light = LightInput::new(sky.as_deref(), ticks);
        let mut overlay = std::mem::take(&mut ticks.echoes);
        let mut writes = Vec::new();
        let mut loaded = HashMap::with_hasher(foldhash::fast::FixedState::default());
        for tick in from + 1..=now {
            for &chunk in &chunks {
                let Some(data) = map.data_at(chunk) else {
                    continue;
                };
                // Every pick starts with its own chunk's blocks and light at hand.
                let data: &ChunkData = data;
                let chunk_light = map
                    .id_at(chunk)
                    .and_then(|id| map.get(id))
                    .and_then(|slot| slot.light());
                let seed = chunk_seed(chunk, tick);
                for i in 0..settings.random_ticks_per_chunk {
                    let pick = mix(seed ^ u64::from(i));
                    let local = LocalPos::from_index(pick as usize & (CHUNK_VOLUME - 1));
                    let pos = BlockPos::from_parts(chunk, local);
                    let state = overlay
                        .get(&pos)
                        .copied()
                        .unwrap_or_else(|| data.get(local));
                    let Some(handler) = ticks.random_handler(state) else {
                        continue;
                    };
                    if missing_chunk(&map, pos, handler.reach, &mut loaded).is_some() {
                        continue;
                    }
                    ran += 1;
                    let mut cx = TickContext {
                        map: &map,
                        registry: &content.registry,
                        overlay: &mut overlay,
                        writes: &mut writes,
                        ticks: &mut *ticks,
                        tick,
                        cache: Cell::new(Some((chunk, data))),
                        rng: mix(pick),
                        light,
                        light_cache: Cell::new(Some((chunk, chunk_light))),
                    };
                    (handler.tick)(&mut cx, pos, state);
                }
            }
        }
        for edit in writes {
            queue.set_block(edit.pos, edit.state);
        }
        for pos in ticks.kept.drain(..) {
            queue.keep_data(pos);
        }
        ticks.echoes = overlay;
    }
    ticks.stats.random = ran;
}

/// Schedules the blocks around the frame's other edits (players, tools): the `BlockChanged`
/// messages that are not the echo of a tick's write.
fn schedule_from_edits(
    mut ticks: ResMut<BlockTicks>,
    mut changes: MessageReader<BlockChanged>,
    map: Res<ChunkMap>,
) {
    let ticks = &mut *ticks;
    for change in changes.read() {
        if ticks.echoes.get(&change.pos) == Some(&change.new) {
            continue;
        }
        let now = ticks.now;
        ticks.notify(change.pos, now, |p| {
            let (chunk, local) = p.split();
            map.data_at(chunk).map(|data| data.get(local))
        });
    }
    ticks.echoes.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::InlineExecutor;
    use crate::{ChunkViewer, WorldPlugin};
    use boxel_app::App;
    use boxel_math::DVec3;
    use boxel_transform::Transform;
    use boxel_voxel::{ChunkKey, testing};
    use boxel_worldgen::WorldGenerator;
    use std::sync::Arc;

    /// Every chunk at y = 0 is solid at local y 0 with a plant on every cell above.
    struct Plants;

    impl WorldGenerator for Plants {
        fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
            let mut data = ChunkData::uniform(BlockState::AIR);
            if key.pos.y == 0 {
                let solid = registry.by_name("test:solid").expect("solid");
                let plant = registry.by_name("test:plant").expect("plant");
                for z in 0..32 {
                    for x in 0..32 {
                        data.set(LocalPos::new(x, 0, z), solid);
                        data.set(LocalPos::new(x, 1, z), plant);
                    }
                }
            }
            data
        }
        fn vertical_chunks(&self) -> std::ops::RangeInclusive<i64> {
            -1..=1
        }
        fn name(&self) -> &str {
            "plants"
        }
    }

    /// A random tick turns a plant into solid.
    fn harden(cx: &mut TickContext<'_>, pos: BlockPos, _: BlockState) {
        let solid = cx.registry().by_name("test:solid").expect("solid");
        // Only half of the picks: `random` decides.
        if cx.random(2) == 0 {
            cx.set(pos, solid);
        }
    }

    /// A world of [`Plants`] around a viewer at the origin's chunk, random ticks only there
    /// (radius 0), with the plant's handler; settled.
    fn planted() -> App {
        let registry = Arc::new(testing::registry());
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(Plants),
            registry: Arc::clone(&registry),
            executor: Arc::new(InlineExecutor),
            ..WorldPlugin::default()
        })
        .add_plugins(BlockTicksPlugin);
        {
            let mut settings = app.world_mut().resource_mut::<BlockTickSettings>();
            settings.random_tick_radius = [0, 0];
            settings.random_catch_up = 1000;
            // The clock moves only by hand: a run does not depend on the frames' real time.
            settings.paused = true;
        }
        let plant = registry.block_by_name("test:plant").expect("plant");
        app.world_mut()
            .resource_mut::<BlockTicks>()
            .register_random(
                &registry,
                plant,
                RandomTickHandler {
                    reach: [0, 0, 0],
                    tick: harden,
                },
            );
        app.world_mut().spawn((
            Transform::from_translation(DVec3::new(16.0, 8.0, 16.0)),
            ChunkViewer::new(2).with_vertical_radius(1),
        ));
        for _ in 0..200 {
            app.update();
        }
        app
    }

    /// The plants that hardened in the chunk at `cx`, `cz`.
    fn hardened(app: &App, cx: i64, cz: i64) -> Vec<BlockPos> {
        let map = app.world().resource::<ChunkMap>();
        let solid = testing::registry().by_name("test:solid").expect("solid");
        let mut out = Vec::new();
        for z in 0..32 {
            for x in 0..32 {
                let pos = BlockPos::new(32 * cx + x, 1, 32 * cz + z);
                assert!(map.block_at(pos).is_some(), "{pos:?} is loaded");
                if map.block_at(pos) == Some(solid) {
                    out.push(pos);
                }
            }
        }
        out
    }

    /// 100 game ticks of 24 picks in a chunk of 1024 plants (one cell in 32): about 75 picks
    /// land on a plant and half of them harden it; the neighbor chunk, out of the radius, keeps
    /// all its plants; and the run is the same every time.
    #[test]
    fn random_ticks_pick_cells_of_the_chunks_around_the_viewer() {
        let mut app = planted();
        assert!(
            hardened(&app, 0, 0).is_empty(),
            "nothing before the clock moves"
        );
        app.world_mut().resource_mut::<BlockTicks>().advance(100);
        app.update();
        app.update();
        let first = hardened(&app, 0, 0);
        assert!((15..=70).contains(&first.len()), "{} hardened", first.len());
        assert!(hardened(&app, 1, 0).is_empty(), "out of the radius");
        let ran = app.world().resource::<BlockTicks>().stats().random;
        assert_eq!(ran, 0, "the last frame had no new game tick");
        // Again: the same cells.
        let mut again = planted();
        again.world_mut().resource_mut::<BlockTicks>().advance(100);
        again.update();
        again.update();
        assert_eq!(hardened(&again, 0, 0), first);
        // Off: nothing more hardens.
        app.world_mut()
            .resource_mut::<BlockTickSettings>()
            .random_ticks_per_chunk = 0;
        app.world_mut().resource_mut::<BlockTicks>().advance(100);
        app.update();
        app.update();
        assert_eq!(hardened(&app, 0, 0), first);
    }

    /// [`Plants`], with the origin's chunk roofed at local y 3 and a lamp under the roof in its
    /// middle: dark in the middle but around the lamp, lit along the open sides.
    struct Roofed;

    /// The lamp of [`Roofed`].
    const LAMP: BlockPos = BlockPos::new(16, 2, 16);

    impl WorldGenerator for Roofed {
        fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
            let mut data = Plants.generate(key, registry);
            if key.pos == ChunkPos::new(0, 0, 0) {
                let solid = registry.by_name("test:solid").expect("solid");
                for z in 0..32 {
                    for x in 0..32 {
                        data.set(LocalPos::new(x, 3, z), solid);
                    }
                }
                let lamp = registry.by_name("test:lamp").expect("lamp");
                data.set(LocalPos::new(16, 2, 16), lamp);
            }
            data
        }
        fn vertical_chunks(&self) -> std::ops::RangeInclusive<i64> {
            -1..=1
        }
        fn name(&self) -> &str {
            "roofed"
        }
    }

    /// Hardens a plant in light 9 or more, the time of day aside (a crop's rule).
    fn harden_lit(cx: &mut TickContext<'_>, pos: BlockPos, _: BlockState) {
        let solid = cx.registry().by_name("test:solid").expect("solid");
        if cx.light(pos).is_some_and(|l| l.raw(0) >= 9) {
            cx.set(pos, solid);
        }
    }

    /// Hardens a plant in a brightness of 9 or more, the time of day counted (a sapling's).
    fn harden_bright(cx: &mut TickContext<'_>, pos: BlockPos, _: BlockState) {
        let solid = cx.registry().by_name("test:solid").expect("solid");
        if cx.brightness(pos).is_some_and(|b| b >= 9) {
            cx.set(pos, solid);
        }
    }

    /// A settled world of [`Roofed`] with `tick` on the plants of the origin's chunk, `setup`
    /// run on the app before it starts, then 4000 game ticks in one frame (so every tick reads
    /// the light from before them); with the light of the plants' cells before the ticks.
    fn roofed(
        tick: fn(&mut TickContext<'_>, BlockPos, BlockState),
        setup: impl FnOnce(&mut App),
    ) -> (
        App,
        HashMap<BlockPos, Option<TickLight>, foldhash::fast::FixedState>,
    ) {
        let registry = Arc::new(testing::registry());
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(Roofed),
            registry: Arc::clone(&registry),
            executor: Arc::new(InlineExecutor),
            ..WorldPlugin::default()
        })
        .add_plugins(BlockTicksPlugin);
        {
            let mut settings = app.world_mut().resource_mut::<BlockTickSettings>();
            settings.random_tick_radius = [0, 0];
            settings.random_catch_up = 10_000;
            settings.paused = true;
        }
        setup(&mut app);
        let plant = registry.block_by_name("test:plant").expect("plant");
        app.world_mut()
            .resource_mut::<BlockTicks>()
            .register_random(
                &registry,
                plant,
                RandomTickHandler {
                    reach: [0, 0, 0],
                    tick,
                },
            );
        app.world_mut().spawn((
            Transform::from_translation(DVec3::new(16.0, 8.0, 16.0)),
            ChunkViewer::new(2).with_vertical_radius(1),
        ));
        for _ in 0..200 {
            app.update();
        }
        let map = app.world().resource::<ChunkMap>();
        let before = (0..32)
            .flat_map(|z| (0..32).map(move |x| BlockPos::new(x, 1, z)))
            .map(|pos| (pos, map.light_at(pos).map(TickLight::of)))
            .collect();
        app.world_mut().resource_mut::<BlockTicks>().advance(4000);
        app.update();
        app.update();
        (app, before)
    }

    /// The plants of the origin's chunk that hardened and those that did not, with the light
    /// they had before the ticks (`None`: no light).
    fn hardened_and_light(
        (app, before): &(
            App,
            HashMap<BlockPos, Option<TickLight>, foldhash::fast::FixedState>,
        ),
    ) -> Vec<(BlockPos, bool, Option<TickLight>)> {
        let map = app.world().resource::<ChunkMap>();
        let solid = testing::registry().by_name("test:solid").expect("solid");
        let mut out = Vec::new();
        for z in 0..32 {
            for x in 0..32 {
                let pos = BlockPos::new(x, 1, z);
                out.push((pos, map.block_at(pos) == Some(solid), before[&pos]));
            }
        }
        out
    }

    /// A random tick reads the cell's light: under the roof only the plants by the open sides
    /// and around the lamp (light 9 or more) harden, the dark middle keeps its plants, and the
    /// light it read is the map's.
    #[test]
    fn a_random_tick_reads_the_light_of_its_cell() {
        let cells = hardened_and_light(&roofed(harden_lit, |_| {}));
        assert!(
            cells.iter().all(|(_, _, l)| l.is_some()),
            "the chunk is lit"
        );
        let lit = |l: &Option<TickLight>| l.is_some_and(|l| l.raw(0) >= 9);
        for (pos, hard, light) in &cells {
            assert!(!hard || lit(light), "{pos:?} hardened in {light:?}");
        }
        let hard =
            |f: &dyn Fn(BlockPos) -> bool| cells.iter().filter(|(p, h, _)| *h && f(*p)).count();
        assert!(hard(&|p| p.x <= 2) > 0, "by an open side");
        let near_lamp = |p: BlockPos| (p.x - LAMP.x).abs() + (p.z - LAMP.z).abs() <= 3;
        assert!(hard(&near_lamp) > 0, "around the lamp");
        let dark = cells.iter().filter(|(_, _, l)| !lit(l)).count();
        assert!(dark > 100, "{dark} dark plants");
        // The lamp's level: its color's brightest channel less one per block.
        let beside = cells
            .iter()
            .find(|(p, _, _)| *p == BlockPos::new(LAMP.x + 1, 1, LAMP.z))
            .and_then(|c| c.2);
        let [r, g, b] = testing::EMISSION;
        assert_eq!(beside.map(|l| l.block), Some(r.max(g).max(b) - 2));
    }

    /// At midnight the sky is darkened by the time of day for `brightness` (Minecraft's
    /// `getMaxLocalRawBrightness`): the plants by the open sides do not harden, the ones around
    /// the lamp still do. Without light every cell reads open sky.
    #[test]
    fn brightness_darkens_the_sky_at_night_and_a_world_without_light_is_open() {
        let night = TimeOfDay {
            paused: true,
            ..TimeOfDay::at_clock(0.0)
        };
        assert_eq!(curve_sky_darken(&night, &DaylightCurve::default()), 12);
        let noon = TimeOfDay::at_clock(12.0);
        assert_eq!(curve_sky_darken(&noon, &DaylightCurve::default()), 0);
        let cells = hardened_and_light(&roofed(harden_bright, |app| {
            app.insert_resource(night);
        }));
        for (pos, hard, light) in &cells {
            let block = light.map_or(0, |l| l.block);
            assert!(
                !hard || block >= 9,
                "{pos:?} hardened in {light:?} at night"
            );
        }
        assert!(
            cells.iter().any(|c| c.1),
            "the lamp's light does not change at night"
        );
        // No light: open sky everywhere, so the dark middle hardens too.
        let cells = hardened_and_light(&roofed(harden_lit, |app| {
            app.world_mut().resource_mut::<crate::WorldSettings>().light = false;
        }));
        assert!(cells.iter().all(|c| c.2.is_none()), "no light is computed");
        let middle = |p: BlockPos| (10..22).contains(&p.x) && (10..22).contains(&p.z);
        assert!(cells.iter().any(|c| c.1 && middle(c.0)));
    }

    #[test]
    fn tick_light_levels() {
        let l = TickLight::of(LightValue::new(12, 3, 9, 5));
        assert_eq!(l, TickLight { sky: 12, block: 9 });
        assert_eq!((l.raw(0), l.raw(2), l.raw(11), l.raw(15)), (12, 10, 9, 9));
        assert_eq!(TickLight::of(LightValue::FULL_SKY), TickLight::OPEN);
        assert_eq!(TickLight::OPEN.raw(11), 4);
    }

    #[test]
    fn tick_randoms_are_in_range_and_follow_the_seed() {
        let a: Vec<u32> = (0..64)
            .scan(seed_of(BlockPos::new(1, 2, 3), 9), |s, _| {
                *s = mix(*s);
                Some((((*s >> 32) * 7) >> 32) as u32)
            })
            .collect();
        assert!(a.iter().all(|&n| n < 7));
        assert!((0..7).all(|n| a.contains(&n)), "{a:?}");
        assert_ne!(
            seed_of(BlockPos::new(1, 2, 3), 9),
            seed_of(BlockPos::new(1, 2, 3), 10)
        );
        assert_ne!(
            chunk_seed(ChunkPos::new(0, 0, 0), 1),
            chunk_seed(ChunkPos::new(1, 0, 0), 1)
        );
    }
}
