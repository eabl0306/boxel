//! Binary greedy meshing of a padded 32^3 chunk, merging by a per-face key.
//!
//! A fork of `binary-greedy-meshing` 0.5.2 (`src/lib.rs`, <https://github.com/Inspirateur/binary-greedy-meshing>),
//! itself a port of <https://github.com/cgerikj/binary-greedy-meshing>. Changes:
//! - The chunk size is fixed at 32 (`Mesher<32>`), and only the mask-based `fast_mesh` path is
//!   kept. Face culling is unchanged.
//! - Merging runs one face at a time and compares a key per unit face instead of the voxel's
//!   `u16`: any slice in the padded layout (the voxels themselves, or a `u32` key scratch that
//!   the caller fills at the visible faces of that face only). Every comparison in the merge is
//!   already guarded by a face bit of the same face, so keys are only read where they were
//!   written.
//! - bgm's two face-mask row indices of the culling (rows `x + y * 32` and `y + x * 32`) are
//!   renamed `xy_row` and `yx_row`.
//! - Quads come out canonical (the min-corner voxel and the extents along the face's u and v
//!   axes, the bits of `PackedQuad::geo` without the face) with the key in the high 32 bits,
//!   instead of bgm's `Quad` and its per-face vertex conventions.
//!
//! With the voxels as keys the quads are the same, in the same order, as bgm's (the regression
//! test in `tests/bgm_regression.rs` checks it byte for byte).
//!
//! ```text
//! MIT License
//!
//! Copyright (c) 2024 Teo Orthlieb
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
//! ```

use crate::packed::layout::{GEO_H_SHIFT, GEO_W_SHIFT, GEO_Y_SHIFT, GEO_Z_SHIFT};

/// Chunk size.
const CS: usize = 32;
/// Padded size.
const CS_P: usize = CS + 2;
/// `CS^2`.
const CS_2: usize = CS * CS;
/// `CS_P^2`.
const CS_P2: usize = CS_P * CS_P;
/// The 32 inner bits of a padded column.
const P_MASK: u64 = !(1 << (CS_P - 1) | 1);

/// Greedy mesher state: the face masks of the last culled chunk, the merge scratch and the
/// output quads.
#[derive(Debug)]
pub(crate) struct Greedy {
    /// Quads per face (`Face` order): `key << 32 | geo`, where `geo` holds the position and
    /// `w - 1`, `h - 1` of the quad at their `PackedQuad::geo` bits (no face).
    pub(crate) quads: [Vec<u64>; 6],
    /// `CS_2 * 6` visible-face columns (see [`Greedy::face_rows`]).
    face_masks: Box<[u64]>,
    /// `CS_2`.
    forward_merged: Box<[u8]>,
    /// `CS`.
    right_merged: Box<[u8]>,
}

impl Greedy {
    /// Allocates the buffers.
    pub(crate) fn new() -> Self {
        Self {
            quads: std::array::from_fn(|_| Vec::new()),
            face_masks: vec![0; CS_2 * 6].into_boxed_slice(),
            forward_merged: vec![0; CS_2].into_boxed_slice(),
            right_merged: vec![0; CS].into_boxed_slice(),
        }
    }

    /// Empties the output quads and resets the merge scratch (the face masks are rewritten by
    /// every [`Greedy::cull`]).
    pub(crate) fn clear(&mut self) {
        self.forward_merged.fill(0);
        self.right_merged.fill(0);
        for quads in &mut self.quads {
            quads.clear();
        }
    }

    /// The 1024 visible-face columns of `face` (`Face` index) after [`Greedy::cull`]. Faces 0
    /// and 1 (±Y): row `x + y * 32`, bit `z`. Faces 2 and 3 (±X): row `y + x * 32`, bit `z`.
    /// Faces 4 and 5 (±Z): row `x + y * 32`, bit `z + 1`.
    pub(crate) fn face_rows(&self, face: usize) -> &[u64] {
        &self.face_masks[face * CS_2..(face + 1) * CS_2]
    }

    /// Hidden face culling (bgm `fast_face_culling`, unchanged): fills the face masks from the
    /// opaque and transparent column masks; transparent voxels also compare their neighbors'
    /// ids (`voxels`), so two transparent voxels of one material hide the faces between them.
    pub(crate) fn cull(&mut self, voxels: &[u16], opaque_mask: &[u64], trans_mask: &[u64]) {
        for a in 1..(CS_P - 1) {
            let a_ = a * CS_P;

            for b in 1..(CS_P - 1) {
                // Column-wise opaque step
                let ab = a_ + b;
                let opaque_col = opaque_mask[ab] & P_MASK;
                let unpadded_opaque_col = opaque_col >> 1;
                let xy_row = (b - 1) + (a - 1) * CS;
                let yx_row = (a - 1) + (b - 1) * CS;
                let up_faces = xy_row;
                let down_faces = xy_row + CS_2;
                let right_faces = yx_row + 2 * CS_2;
                let left_faces = yx_row + 3 * CS_2;
                let front_faces = xy_row + 4 * CS_2;
                let back_faces = xy_row + 5 * CS_2;
                let not_front_col = !opaque_mask[ab + CS_P] >> 1;
                let not_back_col = !opaque_mask[ab - CS_P] >> 1;
                let not_right_col = !opaque_mask[ab + 1] >> 1;
                let not_left_col = !opaque_mask[ab - 1] >> 1;
                let not_col_up = !(opaque_mask[ab] >> 1);
                let not_col_down = !(opaque_mask[ab] << 1);
                self.face_masks[up_faces] = unpadded_opaque_col & not_front_col;
                self.face_masks[down_faces] = unpadded_opaque_col & not_back_col;

                self.face_masks[right_faces] = unpadded_opaque_col & not_right_col;
                self.face_masks[left_faces] = unpadded_opaque_col & not_left_col;

                self.face_masks[front_faces] = opaque_col & not_col_up;
                self.face_masks[back_faces] = opaque_col & not_col_down;

                // check if there's transparent blocks in this column
                let mut bits_here = trans_mask[ab] & P_MASK;
                if bits_here == 0 {
                    continue;
                }
                // Block-wise transparent step
                // The transparent step is slower than the opaque step
                // because we need to check if neighboring transparent blocks are different (we don't care about that for opaque blocks)
                let ab_ = ab * CS_P;
                while bits_here != 0 {
                    let c = bits_here.trailing_zeros() as usize;
                    let c_mask = 1 << c;
                    let unpadded_c_mask = c_mask >> 1;
                    bits_here &= !(c_mask);
                    let abc = ab_ + c;
                    let v1 = voxels[abc];
                    self.face_masks[up_faces] |= not_front_col
                        & unpadded_c_mask
                        & u64::from(v1 != voxels[abc + CS_P2]) << (c - 1);
                    self.face_masks[down_faces] |= not_back_col
                        & unpadded_c_mask
                        & u64::from(v1 != voxels[abc - CS_P2]) << (c - 1);

                    self.face_masks[right_faces] |= not_right_col
                        & unpadded_c_mask
                        & u64::from(v1 != voxels[abc + CS_P]) << (c - 1);
                    self.face_masks[left_faces] |= not_left_col
                        & unpadded_c_mask
                        & u64::from(v1 != voxels[abc - CS_P]) << (c - 1);

                    self.face_masks[front_faces] |=
                        not_col_up & c_mask & u64::from(v1 != voxels[abc + 1]) << c;
                    self.face_masks[back_faces] |=
                        not_col_down & c_mask & u64::from(v1 != voxels[abc - 1]) << c;
                }
            }
        }
    }

    /// Greedy merging of one face (bgm `face_merging`, one face at a time): two unit faces
    /// merge when their keys (`keys`, padded layout) are equal. Appends to `quads[face]`.
    pub(crate) fn merge_face<K: Copy + Eq + Into<u32>>(&mut self, face: usize, keys: &[K]) {
        if face < 4 {
            self.merge_face_0_3(face, keys);
        } else {
            self.merge_face_4_5(face, keys);
        }
    }

    /// Faces 0 to 3 (±Y, ±X).
    fn merge_face_0_3<K: Copy + Eq + Into<u32>>(&mut self, face: usize, keys: &[K]) {
        let axis = face / 2;
        let out = &mut self.quads[face];

        for layer in 0..CS {
            let bits_location = layer * CS + face * CS_2;

            for forward in 0..CS {
                let mut bits_here = self.face_masks[forward + bits_location];
                if bits_here == 0 {
                    continue;
                }

                let bits_next = if forward + 1 < CS {
                    self.face_masks[(forward + 1) + bits_location]
                } else {
                    0
                };

                let mut right_merged = 1;
                while bits_here != 0 {
                    let bit_pos = bits_here.trailing_zeros() as usize;

                    let v_type = keys[get_axis_index(axis, forward + 1, bit_pos + 1, layer + 1)];

                    if (bits_next >> bit_pos & 1) != 0
                        && v_type == keys[get_axis_index(axis, forward + 2, bit_pos + 1, layer + 1)]
                    {
                        self.forward_merged[bit_pos] += 1;
                        bits_here &= !(1 << bit_pos);
                        continue;
                    }

                    for right in (bit_pos + 1)..CS {
                        if (bits_here >> right & 1) == 0
                            || self.forward_merged[bit_pos] != self.forward_merged[right]
                            || v_type
                                != keys[get_axis_index(axis, forward + 1, right + 1, layer + 1)]
                        {
                            break;
                        }
                        self.forward_merged[right] = 0;
                        right_merged += 1;
                    }
                    bits_here &= !((1 << (bit_pos + right_merged)) - 1);

                    let mesh_front = forward - self.forward_merged[bit_pos] as usize;
                    let mesh_left = bit_pos;

                    let mesh_width = right_merged;
                    let mesh_length = (self.forward_merged[bit_pos] + 1) as usize;

                    self.forward_merged[bit_pos] = 0;
                    right_merged = 1;

                    // Canonical: ±Y (x = forward, y = layer, z = bit) span x (u) by z (v); ±X
                    // (x = layer, y = forward, z = bit) span z (u) by y (v).
                    let quad = if axis == 0 {
                        canonical(
                            [mesh_front, layer, mesh_left],
                            mesh_length,
                            mesh_width,
                            v_type.into(),
                        )
                    } else {
                        canonical(
                            [layer, mesh_front, mesh_left],
                            mesh_width,
                            mesh_length,
                            v_type.into(),
                        )
                    };
                    out.push(quad);
                }
            }
        }
    }

    /// Faces 4 and 5 (±Z).
    fn merge_face_4_5<K: Copy + Eq + Into<u32>>(&mut self, face: usize, keys: &[K]) {
        let axis = face / 2;
        let out = &mut self.quads[face];

        for forward in 0..CS {
            let bits_location = forward * CS + face * CS_2;
            let bits_forward_location = (forward + 1) * CS + face * CS_2;

            for right in 0..CS {
                let mut bits_here = self.face_masks[right + bits_location];
                if bits_here == 0 {
                    continue;
                }

                let bits_forward = if forward < CS - 1 {
                    self.face_masks[right + bits_forward_location]
                } else {
                    0
                };
                let bits_right = if right < CS - 1 {
                    self.face_masks[right + 1 + bits_location]
                } else {
                    0
                };
                let right_cs = right * CS;

                while bits_here != 0 {
                    let bit_pos = bits_here.trailing_zeros() as usize;

                    bits_here &= !(1 << bit_pos);

                    let v_type = keys[get_axis_index(axis, right + 1, forward + 1, bit_pos)];
                    let forward_merge_i = right_cs + (bit_pos - 1);
                    let right_merged_ref = &mut self.right_merged[bit_pos - 1];

                    if *right_merged_ref == 0
                        && (bits_forward >> bit_pos & 1) != 0
                        && v_type == keys[get_axis_index(axis, right + 1, forward + 2, bit_pos)]
                    {
                        self.forward_merged[forward_merge_i] += 1;
                        continue;
                    }

                    if (bits_right >> bit_pos & 1) != 0
                        && self.forward_merged[forward_merge_i]
                            == self.forward_merged[(right_cs + CS) + (bit_pos - 1)]
                        && v_type == keys[get_axis_index(axis, right + 2, forward + 1, bit_pos)]
                    {
                        self.forward_merged[forward_merge_i] = 0;
                        *right_merged_ref += 1;
                        continue;
                    }

                    let mesh_left = right - *right_merged_ref as usize;
                    let mesh_front = forward - self.forward_merged[forward_merge_i] as usize;

                    let mesh_width = 1 + *right_merged_ref as usize;
                    let mesh_length = 1 + self.forward_merged[forward_merge_i] as usize;

                    self.forward_merged[forward_merge_i] = 0;
                    *right_merged_ref = 0;

                    // Canonical: x = right, y = forward, z = bit - 1; span x (u) by y (v).
                    out.push(canonical(
                        [mesh_left, mesh_front, bit_pos - 1],
                        mesh_width,
                        mesh_length,
                        v_type.into(),
                    ));
                }
            }
        }
    }
}

/// A canonical quad: `key << 32` and the `PackedQuad::geo` bits of `pos`, `w - 1` and `h - 1`.
#[inline]
fn canonical(pos: [usize; 3], w: usize, h: usize, key: u32) -> u64 {
    debug_assert!(pos.iter().all(|&c| c < CS) && (1..=CS).contains(&w) && (1..=CS).contains(&h));
    let geo = pos[0] as u32
        | (pos[1] as u32) << GEO_Y_SHIFT
        | (pos[2] as u32) << GEO_Z_SHIFT
        | (w as u32 - 1) << GEO_W_SHIFT
        | (h as u32 - 1) << GEO_H_SHIFT;
    u64::from(key) << 32 | u64::from(geo)
}

/// Padded index of the merge coordinates `(a, b, c)` of `axis` (bgm `get_axis_index`).
#[inline]
fn get_axis_index(axis: usize, a: usize, b: usize, c: usize) -> usize {
    match axis {
        0 => b + (a * CS_P) + (c * CS_P2),
        1 => b + (c * CS_P) + (a * CS_P2),
        _ => c + (a * CS_P) + (b * CS_P2),
    }
}
