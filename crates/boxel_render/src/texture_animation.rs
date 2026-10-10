//! Animated block textures (ADR 0028). A layer of the block texture array whose pack texture has
//! an animation keeps, beside the array, the finished layer (tint, overlays and alpha baked in,
//! whole mip chain) of every strip frame its [`Timeline`] shows. Each frame the renderer asks
//! which frame every animated layer shows at the game tick ([`ViewParams::texture_tick`]) and
//! writes the layers that changed over the old pixels of the array with `queue.write_texture`,
//! up to a byte budget. Layer ids, the sampler and the shaders stay as they are, so every tier
//! samples the same texels.
//!
//! [`ViewParams::texture_tick`]: crate::ViewParams::texture_tick

use std::fmt;
use std::sync::Arc;

use boxel_pack::{FrameAt, Timeline, mix_pixel};

/// Bytes of animated layers [`crate::VoxelRenderer`] uploads in one frame unless
/// [`crate::VoxelRenderer::set_texture_upload_budget`] says otherwise. Layers change at most once
/// a game tick (20 a second): 1 MiB is every animated layer of a 128 px pack in one frame, and
/// one 512 px layer (1.4 MB) a frame.
pub const DEFAULT_TEXTURE_UPLOAD_BUDGET: u64 = 1 << 20;

/// What an animated layer shows: one strip frame, or Minecraft's blend of a frame into the next
/// one (`sub` of `time` ticks into the frame). Two equal keys are equal pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameKey {
    /// Strip index of the current frame.
    pub current: u32,
    /// Strip index blended toward (`current` when nothing is blended).
    pub next: u32,
    /// Ticks into the current frame (0 when nothing is blended).
    pub sub: u32,
    /// Ticks of the current frame (1 when nothing is blended).
    pub time: u32,
}

impl FrameKey {
    /// The key of `at`: blended only when the timeline interpolates and the blend is not the
    /// current frame alone (its first tick, or a next frame with the same index).
    fn new(at: FrameAt, interpolate: bool) -> Self {
        if interpolate && at.sub > 0 && at.current != at.next {
            Self {
                current: at.current,
                next: at.next,
                sub: at.sub,
                time: at.time,
            }
        } else {
            Self {
                current: at.current,
                next: at.current,
                sub: 0,
                time: 1,
            }
        }
    }

    /// Whether the key blends two frames.
    pub fn blends(&self) -> bool {
        self.sub > 0
    }
}

/// The animation of one layer of the block texture array.
#[derive(Clone, PartialEq, Eq)]
pub struct LayerAnimation {
    /// Layer index in the array.
    pub layer: u32,
    timeline: Timeline,
    /// The finished layer (level 0 and its mips, as in `BlockTextureData::data`) of each strip
    /// index the timeline shows, sorted by index.
    frames: Vec<(u32, Vec<u8>)>,
}

impl fmt::Debug for LayerAnimation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LayerAnimation")
            .field("layer", &self.layer)
            .field("period", &self.timeline.period())
            .field("interpolate", &self.timeline.interpolate())
            .field("frames", &self.frames.len())
            .finish()
    }
}

impl LayerAnimation {
    /// `frames`: the finished layer of every index of `timeline.indices()`, all of one size.
    pub(crate) fn new(layer: u32, timeline: Timeline, mut frames: Vec<(u32, Vec<u8>)>) -> Self {
        frames.sort_by_key(|(index, _)| *index);
        debug_assert_eq!(
            frames.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            timeline.indices()
        );
        debug_assert!(frames.windows(2).all(|w| w[0].1.len() == w[1].1.len()));
        Self {
            layer,
            timeline,
            frames,
        }
    }

    /// The timeline that picks the frames.
    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    /// Bytes of one layer with its mip chain.
    pub fn layer_bytes(&self) -> usize {
        self.frames.first().map_or(0, |(_, bytes)| bytes.len())
    }

    /// Bytes the stored frames take.
    pub fn memory(&self) -> usize {
        self.frames.len() * self.layer_bytes()
    }

    /// What the layer shows `tick` game ticks after the textures loaded.
    pub fn key(&self, tick: u64) -> FrameKey {
        FrameKey::new(self.timeline.at(tick), self.timeline.interpolate())
    }

    /// The finished layer of strip index `index` (one the timeline shows).
    pub fn frame(&self, index: u32) -> &[u8] {
        let n = self
            .frames
            .binary_search_by_key(&index, |(i, _)| *i)
            .unwrap_or(0);
        &self.frames[n].1
    }

    /// The pixels of `key`: a stored frame, or its blend into the next one written to `scratch`
    /// (Minecraft blends every mip level of both frames, texel by texel, [`mix_pixel`]).
    pub fn pixels<'a>(&'a self, key: FrameKey, scratch: &'a mut Vec<u8>) -> &'a [u8] {
        if !key.blends() {
            return self.frame(key.current);
        }
        let weight = FrameAt {
            current: key.current,
            next: key.next,
            sub: key.sub,
            time: key.time,
        }
        .weight();
        let (a, b) = (self.frame(key.current), self.frame(key.next));
        scratch.clear();
        scratch.extend(
            a.as_chunks::<4>()
                .0
                .iter()
                .zip(b.as_chunks::<4>().0)
                .flat_map(|(a, b)| mix_pixel(weight, *a, *b)),
        );
        scratch
    }
}

/// Counters of the animated layer uploads ([`crate::RenderStats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AnimationCounters {
    /// Layers written (cumulative).
    pub layers: u64,
    /// Their bytes (cumulative).
    pub bytes: u64,
    /// Animated layers the last frame left on an older frame for the budget.
    pub behind: u32,
}

/// The renderer's side: which frame each animated layer shows on the GPU, and the budget.
pub(crate) struct TextureAnimator {
    layers: Arc<[LayerAnimation]>,
    /// Key on the GPU per layer of `layers`; `None` until written (the array starts with
    /// whatever the texture data held).
    shown: Vec<Option<FrameKey>>,
    /// Where the next frame starts looking, so a layer the budget skipped goes first next time.
    cursor: usize,
    budget: u64,
    scratch: Vec<u8>,
    size: u32,
    mip_levels: u32,
    pub counters: AnimationCounters,
}

impl TextureAnimator {
    pub fn new(layers: Arc<[LayerAnimation]>, size: u32, mip_levels: u32) -> Self {
        Self {
            shown: vec![None; layers.len()],
            layers,
            cursor: 0,
            budget: DEFAULT_TEXTURE_UPLOAD_BUDGET,
            scratch: Vec::new(),
            size,
            mip_levels,
            counters: AnimationCounters::default(),
        }
    }

    pub fn set_budget(&mut self, bytes: u64) {
        self.budget = bytes;
    }

    /// The layers (positions in `layers`) to write this frame and their keys: those whose key at
    /// `tick` is not on the GPU, from the cursor on, while their bytes fit the budget (the first
    /// one always does, so every layer catches up). The rest count as behind and the cursor
    /// moves to the first of them.
    pub fn plan(&mut self, tick: u64) -> Vec<(usize, FrameKey)> {
        let n = self.layers.len();
        let mut due = Vec::new();
        let mut used = 0u64;
        let mut first_skipped = None;
        let mut behind = 0;
        for k in 0..n {
            let i = (self.cursor + k) % n;
            let key = self.layers[i].key(tick);
            if self.shown[i] == Some(key) {
                continue;
            }
            let bytes = self.layers[i].layer_bytes() as u64;
            if due.is_empty() || used.saturating_add(bytes) <= self.budget {
                due.push((i, key));
                used += bytes;
            } else {
                behind += 1;
                first_skipped.get_or_insert(i);
            }
        }
        if let Some(i) = first_skipped {
            self.cursor = i;
        }
        self.counters.behind = behind;
        due
    }

    /// Writes the layers [`Self::plan`] picks for `tick` into `texture` (every mip level).
    pub fn update(&mut self, queue: &wgpu::Queue, texture: &wgpu::Texture, tick: u64) {
        if self.layers.is_empty() {
            return;
        }
        for (i, key) in self.plan(tick) {
            let layer = &self.layers[i];
            let pixels = layer.pixels(key, &mut self.scratch);
            let mut offset = 0;
            for mip in 0..self.mip_levels {
                let edge = (self.size >> mip).max(1);
                let len = (edge * edge * 4) as usize;
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer.layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &pixels[offset..offset + len],
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * edge),
                        rows_per_image: Some(edge),
                    },
                    wgpu::Extent3d {
                        width: edge,
                        height: edge,
                        depth_or_array_layers: 1,
                    },
                );
                offset += len;
            }
            self.counters.layers += 1;
            self.counters.bytes += pixels.len() as u64;
            self.shown[i] = Some(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_pack::{Animation, AnimationFrame};

    /// A 2x2 layer (two mips: 2x2 and 1x1, 20 bytes) whose every byte is `value`, alpha `alpha`.
    fn layer_of(value: u8, alpha: u8) -> Vec<u8> {
        (0..5).flat_map(|_| [value, value, value, alpha]).collect()
    }

    fn timeline(frametime: u32, interpolate: bool, frames: &[u32]) -> Timeline {
        Animation {
            frametime,
            interpolate,
            frames: frames
                .iter()
                .map(|&index| AnimationFrame { index, time: None })
                .collect(),
            width: None,
            height: None,
        }
        .timeline(4, &mut Vec::new())
        .expect("animated")
    }

    fn animation(layer: u32, frametime: u32, interpolate: bool) -> LayerAnimation {
        LayerAnimation::new(
            layer,
            timeline(frametime, interpolate, &[]),
            (0..4u8)
                .map(|i| (u32::from(i), layer_of(i * 80, 200 + i)))
                .collect(),
        )
    }

    #[test]
    fn keys_and_pixels_follow_the_timeline() {
        let anim = animation(3, 4, true);
        assert_eq!(anim.layer_bytes(), 20);
        assert_eq!(anim.memory(), 80);
        let mut scratch = Vec::new();
        // tick 0: frame 0 alone
        let key = anim.key(0);
        assert!(!key.blends());
        assert_eq!(anim.pixels(key, &mut scratch), layer_of(0, 200));
        // tick 5: a quarter into frame 1, blending toward frame 2 with weight 0.75
        let key = anim.key(5);
        assert_eq!((key.current, key.next, key.sub, key.time), (1, 2, 1, 4));
        let mixed = (0.75 * 80.0 + 0.25 * 160.0) as u8;
        assert_eq!(anim.pixels(key, &mut scratch), layer_of(mixed, 201));
        // frame 3 blends into frame 0 (the loop)
        let key = anim.key(15);
        assert_eq!((key.current, key.next), (3, 0));
        // without interpolation the frame shows whole and the key only changes with it
        let flat = animation(0, 4, false);
        assert_eq!(flat.key(5), flat.key(4));
        assert_ne!(flat.key(8), flat.key(7));
        assert_eq!(flat.pixels(flat.key(9), &mut scratch), layer_of(160, 202));
    }

    /// A key repeated in the sequence (`0, 0, 1`) does not blend a frame with itself.
    #[test]
    fn a_repeated_frame_does_not_blend() {
        let anim = LayerAnimation::new(
            0,
            timeline(2, true, &[0, 0, 1]),
            vec![(0, layer_of(0, 255)), (1, layer_of(100, 255))],
        );
        assert_eq!(anim.key(1), anim.key(0));
        assert_eq!(anim.key(2), anim.key(0));
        assert!(anim.key(3).blends());
    }

    /// The budget bounds the bytes of a frame (at least one layer passes), skipped layers go
    /// first the next frame, and an unchanged layer is not written again.
    #[test]
    fn uploads_fit_the_budget_and_skipped_layers_go_first() {
        let layers: Arc<[LayerAnimation]> = (0..5).map(|l| animation(l, 2, false)).collect();
        let mut animator = TextureAnimator::new(layers, 2, 2);
        // everything is due once, at 20 bytes a layer
        animator.set_budget(45);
        let plan = |a: &mut TextureAnimator, tick| {
            let due = a.plan(tick);
            for &(i, key) in &due {
                a.shown[i] = Some(key);
            }
            due.into_iter().map(|(i, _)| i).collect::<Vec<_>>()
        };
        assert_eq!(plan(&mut animator, 0), [0, 1]);
        assert_eq!(animator.counters.behind, 3);
        assert_eq!(plan(&mut animator, 0), [2, 3]);
        assert_eq!(plan(&mut animator, 0), [4]);
        assert_eq!(animator.counters.behind, 0);
        // the same tick again: nothing to write
        assert_eq!(plan(&mut animator, 1), Vec::<usize>::new());
        // the next frame of all five: from the cursor (layer 4, the last one skipped)
        assert_eq!(plan(&mut animator, 2), [4, 0]);
        assert_eq!(plan(&mut animator, 2), [1, 2]);
        assert_eq!(plan(&mut animator, 2), [3]);
        // a budget under one layer still writes one a frame
        animator.set_budget(1);
        assert_eq!(plan(&mut animator, 4), [3]);
        assert_eq!(animator.counters.behind, 4);
        // an unlimited budget writes every changed layer at once, from the cursor
        animator.set_budget(u64::MAX);
        assert_eq!(plan(&mut animator, 6), [4, 0, 1, 2, 3]);
    }
}
