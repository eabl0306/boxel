//! Dynamic lights baked into the meshes near them (ADR 0063).
//!
//! A light that is not a block (`crate::DynamicLight`) shines from a voxel. Every mesh job whose
//! padded chunk it may reach adds its flood to the light the mesh shows
//! (`boxel_light::add_point_lights`), the light of an emitter placed at that voxel, so it goes
//! around walls and never through them, and lit faces get their AO as under a placed torch. The
//! light the job keeps for gameplay is the light of the blocks alone (ADR 0059).
//!
//! When a light moves to another voxel (past [`HYSTERESIS`]), changes its emission, appears or
//! goes, the chunks it reached and the ones it reaches now are remeshed as one edit batch,
//! published together. A light whose last batch is still open stays where that batch shows it
//! until it is published (and moves in the next frame), so a light faster than the meshes still
//! lands. A light at rest
//! remeshes nothing.
use super::{Ctx, WorldJobs, box_chunks};
use boxel_light::PointEmitter;
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, ChunkPos};
use std::collections::BTreeSet;

/// How far, in blocks, a light goes past the voxel it shines from before it moves to the next
/// one: a light that rests on a voxel border (an eye at a whole height) does not remesh every
/// frame as it shakes.
pub(crate) const HYSTERESIS: f64 = 0.125;

/// A light the world bakes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BakedLight {
    /// Its id (the entity's bits).
    pub(crate) id: u64,
    /// Its voxel and emission, as the meshes show it.
    pub(crate) light: PointEmitter,
    /// The edit batch of its last remesh: the light stays while it is open.
    batch: Option<u32>,
}

/// One light the world is asked to bake this frame: id, position, emission.
pub(crate) type WantedLight = (u64, DVec3, [u8; 3]);

/// What the dynamic lights cost so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PointCounts {
    /// Lights that moved to another voxel, changed, appeared or went.
    pub(crate) moves: usize,
    /// Chunks remeshed for them.
    pub(crate) remeshes: usize,
    /// Mesh jobs that baked a dynamic light.
    pub(crate) boxes: usize,
}

/// The voxel a light at `at` shines from: `old` while `at` stays within [`HYSTERESIS`] of it,
/// else the voxel holding `at`.
fn voxel(at: DVec3, old: Option<BlockPos>) -> BlockPos {
    let new = BlockPos::from_world(at);
    match old {
        Some(old) if old != new => {
            let near = |p: f64, o: i64| {
                let o = o as f64;
                p >= o - HYSTERESIS && p < o + 1.0 + HYSTERESIS
            };
            if near(at.x, old.x) && near(at.y, old.y) && near(at.z, old.z) {
                old
            } else {
                new
            }
        }
        _ => new,
    }
}

impl WorldJobs {
    /// The baked lights that may light the padded chunk at `pos`, sorted (a job's input does not
    /// depend on the order the lights came in).
    pub(super) fn points_for(&self, pos: ChunkPos) -> Vec<PointEmitter> {
        let mut out: Vec<PointEmitter> = self
            .point_lights
            .iter()
            .map(|b| b.light)
            .filter(|l| l.reaches_chunk(pos))
            .collect();
        out.sort_unstable();
        out
    }

    /// The voxel `p` changed what light goes through (opacity or filter): the chunks of every
    /// baked light whose reach holds it go into `remesh`.
    pub(super) fn point_light_edit(&self, p: BlockPos, remesh: &mut BTreeSet<ChunkPos>) {
        for b in &self.point_lights {
            let (l, r) = (b.light.pos, u64::from(b.light.reach()));
            if p.x.abs_diff(l.x) <= r && p.y.abs_diff(l.y) <= r && p.z.abs_diff(l.z) <= r {
                box_chunks(l, b.light.reach(), remesh);
            }
        }
    }

    /// The frame's dynamic lights to bake (`wanted`: lit, finite, at most
    /// `WorldSettings::baked_lights`; none with light off), see the [module docs](self).
    pub(crate) fn update_point_lights(&mut self, cx: &mut Ctx<'_>, wanted: &[WantedLight]) {
        if wanted.is_empty() && self.point_lights.is_empty() {
            return;
        }
        let mut next: Vec<BakedLight> = Vec::with_capacity(wanted.len());
        let mut moved = Vec::new();
        let mut remesh = BTreeSet::new();
        for &(id, at, emission) in wanted {
            let old = self.point_lights.iter().find(|b| b.id == id).copied();
            let light = PointEmitter::new(voxel(at, old.map(|b| b.light.pos)), emission);
            match old {
                Some(b) if b.light == light => next.push(b),
                // Its last remesh is still on its way: it waits for it.
                Some(b) if b.batch.is_some_and(|k| self.batches.contains_key(&k)) => next.push(b),
                _ => {
                    if let Some(b) = old {
                        box_chunks(b.light.pos, b.light.reach(), &mut remesh);
                    }
                    box_chunks(light.pos, light.reach(), &mut remesh);
                    moved.push(next.len());
                    next.push(BakedLight {
                        id,
                        light,
                        batch: None,
                    });
                }
            }
        }
        let mut gone = 0;
        for b in &self.point_lights {
            if !next.iter().any(|n| n.id == b.id) {
                box_chunks(b.light.pos, b.light.reach(), &mut remesh);
                gone += 1;
            }
        }
        self.point_counts.moves += moved.len() + gone;
        // The jobs started from here on read the new lights.
        self.point_lights = next;
        if remesh.is_empty() {
            return;
        }
        let batch = self.new_batch();
        for i in moved {
            self.point_lights[i].batch = Some(batch);
        }
        for pos in remesh {
            if let Some(id) = cx.map.id_at(pos) {
                self.remesh_for_edit(cx, id, batch);
                self.point_counts.remeshes += 1;
            }
        }
        self.close_if_empty(cx, batch);
    }
}
