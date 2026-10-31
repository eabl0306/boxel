//! The data edits of an edit batch (ADR 0048): a cell's [`BlockData`](boxel_voxel::BlockData)
//! changes in its chunk's snapshot (copy-on-write, no version bump: the blocks are the same),
//! the chunk is marked for saving, and a cell whose block draws its data
//! ([`BlockRegistry::meshes_data`](boxel_voxel::BlockRegistry::meshes_data)) remeshes its
//! chunk as one more edit batch.

use super::{Ctx, WorldJobs};
use crate::DataEdit;
use boxel_voxel::BlockPos;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

impl WorldJobs {
    /// Applies `edits` after the batch's block edits. An edit to a chunk that is not `Ready` is
    /// dropped, as a block edit is; data set on air is dropped (data lives with a block).
    pub(super) fn apply_data_edits(
        &mut self,
        cx: &mut Ctx<'_>,
        edits: BTreeMap<BlockPos, DataEdit>,
    ) {
        let registry = Arc::clone(&cx.content.registry);
        let mut remesh = BTreeSet::new();
        for (pos, edit) in edits {
            let DataEdit::Set(new) = edit else {
                continue;
            };
            let (chunk, local) = pos.split();
            let Some(id) = cx.map.id_at(chunk) else {
                tracing::debug!(?pos, "data edit dropped: chunk not loaded");
                continue;
            };
            let Some(data) = cx.map.ready_data_mut(id) else {
                tracing::debug!(?pos, "data edit dropped: chunk not ready");
                continue;
            };
            let state = data.get(local);
            let new = new.filter(|_| !state.is_air());
            if data.block_data(local) == new.as_ref() {
                continue;
            }
            Arc::make_mut(data).set_block_data(local, new);
            if let Some(p) = &mut self.persist {
                let slot = &mut cx.map.slots[id];
                if !slot.dirty() {
                    slot.set_dirty(true);
                    p.dirty.insert(id);
                }
            }
            if registry.meshes_data(state) {
                remesh.insert(id);
            }
        }
        if remesh.is_empty() {
            return;
        }
        let batch = self.new_batch();
        for id in remesh {
            self.remesh_for_edit(cx, id, batch);
        }
        self.close_if_empty(cx, batch);
    }
}
