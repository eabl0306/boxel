//! The paint list: quads in physical pixels, grouped in layers and composed into [`UiDraw`].

use crate::color::Rgba;
use boxel_ecs::prelude::*;
use boxel_math::UVec2;
use std::sync::Arc;

/// One textured rectangle of the paint list, in physical pixels.
///
/// The quad maps the atlas rectangle `src` (`src_size` texels) onto the physical rectangle at
/// `origin` of `size` pixels; `min`..`max` is the part actually drawn. Clipping shrinks only
/// `min`..`max`, so a clipped quad shows exactly the pixels the whole quad would show there.
///
/// The texel of physical pixel `(x, y)` is `src + (p - origin) * src_size / size` per axis in
/// integer arithmetic ([`UiQuad::texel_at`]): a pixel's center truncated to the pixel, so every
/// renderer that follows this rule (the CPU raster here, the GPU pass) picks the same texel.
/// The color is that texel times `tint`, both converted to linear light, blended over the target
/// with straight alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UiQuad {
    /// Top-left drawn pixel (inclusive).
    pub min: [i32; 2],
    /// Bottom-right drawn pixel (exclusive).
    pub max: [i32; 2],
    /// Where the full mapping starts.
    pub origin: [i32; 2],
    /// The full mapping's size in physical pixels (never 0 on an axis that draws).
    pub size: [u32; 2],
    /// Source rectangle origin in atlas texels.
    pub src: [u32; 2],
    /// Source rectangle size in atlas texels.
    pub src_size: [u32; 2],
    /// sRGB tint with straight alpha; multiplies the texel.
    pub tint: Rgba,
}

impl UiQuad {
    /// A quad drawing the whole source rectangle over `origin`..`origin + size`.
    pub const fn mapped(
        origin: [i32; 2],
        size: [u32; 2],
        src: [u32; 2],
        src_size: [u32; 2],
        tint: Rgba,
    ) -> Self {
        Self {
            min: origin,
            max: [origin[0] + size[0] as i32, origin[1] + size[1] as i32],
            origin,
            size,
            src,
            src_size,
            tint,
        }
    }

    /// A solid rectangle: the atlas' white texel (0, 0) times `color`.
    pub fn solid(min: [i32; 2], max: [i32; 2], color: Rgba) -> Self {
        let size = [
            (max[0] - min[0]).max(0) as u32,
            (max[1] - min[1]).max(0) as u32,
        ];
        Self::mapped(min, size, [0, 0], [1, 1], color)
    }

    /// The drawn part limited to `min`..`max`, or `None` when nothing is left.
    pub fn clipped(mut self, min: [i32; 2], max: [i32; 2]) -> Option<Self> {
        for axis in 0..2 {
            self.min[axis] = self.min[axis].max(min[axis]);
            self.max[axis] = self.max[axis].min(max[axis]);
            if self.min[axis] >= self.max[axis] {
                return None;
            }
        }
        Some(self)
    }

    /// The atlas texel drawn at physical pixel `(x, y)` (inside `min`..`max`).
    pub fn texel_at(&self, x: i32, y: i32) -> [u32; 2] {
        let pick = |axis: usize, p: i32| {
            let d = (p - self.origin[axis]) as u64;
            self.src[axis]
                + (d * u64::from(self.src_size[axis]) / u64::from(self.size[axis])) as u32
        };
        [pick(0, x), pick(1, y)]
    }
}

/// The paint layers, drawn in this order (later ones on top).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UiLayer {
    /// The in-game HUD. Retained: it keeps its quads until it is drawn again (the first
    /// [`crate::Ui::layer`] call of a frame replaces it) or cleared, so a HUD drawn once costs
    /// nothing on the following frames. It is cleared when the GUI scale changes, so redraw it
    /// on `resource_changed::<GuiScale>`. It never captures input.
    Hud,
    /// Screens (menus). Drawn every frame they are open: a frame that does not draw it leaves
    /// it empty.
    Screen,
    /// Above screens: tooltips and confirmations. Per frame, as [`UiLayer::Screen`].
    Popup,
}

impl UiLayer {
    /// Every layer, bottom first.
    pub const ALL: [Self; 3] = [Self::Hud, Self::Screen, Self::Popup];

    /// Whether the layer keeps its content across frames.
    pub const fn retained(self) -> bool {
        matches!(self, Self::Hud)
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// The composed paint list, ready for a renderer: every layer's quads in drawing order, clipped
/// to the target.
///
/// `generation` changes only when the list or the target size really changes, so a renderer
/// copies the `Arc` and uploads only then; an idle menu or a still HUD uploads nothing.
#[derive(Resource, Clone, Debug, Default)]
pub struct UiDraw {
    quads: Arc<Vec<UiQuad>>,
    target: UVec2,
    generation: u64,
}

impl UiDraw {
    /// The quads, bottom first.
    pub fn quads(&self) -> &Arc<Vec<UiQuad>> {
        &self.quads
    }

    /// The target size in physical pixels the quads were composed for.
    pub fn target_size(&self) -> UVec2 {
        self.target
    }

    /// Changes whenever [`UiDraw::quads`] or [`UiDraw::target_size`] change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// No quad to draw.
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }

    /// Replaces the list when it differs. Returns whether it changed.
    pub(crate) fn update(&mut self, quads: Vec<UiQuad>, target: UVec2) -> bool {
        if *self.quads == quads && self.target == target {
            return false;
        }
        self.quads = Arc::new(quads);
        self.target = target;
        self.generation += 1;
        true
    }
}

/// Composes the layers' quads, bottom first, dropping what falls outside the target.
pub(crate) fn compose<'a>(
    layers: impl IntoIterator<Item = &'a [UiQuad]>,
    target: UVec2,
) -> Vec<UiQuad> {
    let max = [target.x as i32, target.y as i32];
    layers
        .into_iter()
        .flatten()
        .filter_map(|quad| quad.clipped([0, 0], max))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texel_mapping_is_integer_and_survives_clipping() {
        // 3 texels stretched over 10 pixels starting at x = 5.
        let quad = UiQuad::mapped([5, 0], [10, 1], [7, 0], [3, 1], [255; 4]);
        let texels: Vec<u32> = (5..15).map(|x| quad.texel_at(x, 0)[0]).collect();
        assert_eq!(texels, [7, 7, 7, 7, 8, 8, 8, 9, 9, 9]);
        let clipped = quad.clipped([9, 0], [12, 1]).unwrap();
        assert_eq!((clipped.min, clipped.max), ([9, 0], [12, 1]));
        for x in 9..12 {
            assert_eq!(clipped.texel_at(x, 0), quad.texel_at(x, 0));
        }
        assert!(quad.clipped([15, 0], [20, 1]).is_none());
    }

    #[test]
    fn generation_moves_only_on_a_change() {
        let mut draw = UiDraw::default();
        let quad = UiQuad::solid([0, 0], [4, 4], [1, 2, 3, 4]);
        assert!(draw.update(vec![quad], UVec2::new(8, 8)));
        assert_eq!(draw.generation(), 1);
        for _ in 0..100 {
            assert!(!draw.update(vec![quad], UVec2::new(8, 8)));
        }
        assert_eq!(draw.generation(), 1);
        assert!(draw.update(vec![quad], UVec2::new(9, 8)));
        assert!(draw.update(Vec::new(), UVec2::new(9, 8)));
        assert_eq!(draw.generation(), 3);
        assert!(draw.is_empty());
    }

    #[test]
    fn compose_keeps_order_and_clips_to_the_target() {
        let a = [UiQuad::solid([-4, -4], [4, 4], [1; 4])];
        let b = [UiQuad::solid([6, 6], [20, 20], [2; 4])];
        let out = compose([&a[..], &b[..]], UVec2::new(10, 10));
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].min, out[0].max), ([0, 0], [4, 4]));
        assert_eq!((out[1].min, out[1].max), ([6, 6], [10, 10]));
        assert!(compose([&b[..]], UVec2::new(5, 5)).is_empty());
    }

    proptest::proptest! {
        /// Every pixel of a mapping reads a texel of its source rectangle.
        #[test]
        fn texels_stay_in_the_source(
            origin in -5000i32..5000, size in 1u32..3000, src in 0u32..4000,
            src_size in 1u32..4000, at in 0u32..3000,
        ) {
            let at = at % size;
            let quad = UiQuad::mapped([origin, 0], [size, 1], [src, 0], [src_size, 1], [255; 4]);
            let texel = quad.texel_at(origin + at as i32, 0)[0];
            proptest::prop_assert!(texel >= src && texel < src + src_size);
        }
    }
}
