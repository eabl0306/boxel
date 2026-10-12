//! The GUI scale: how many physical pixels one GUI pixel takes, as Minecraft's "GUI Scale".

use boxel_ecs::prelude::*;
use boxel_math::{IVec2, UVec2, Vec2};

/// The smallest GUI area the automatic scale keeps (Minecraft's 320 by 240).
pub const MIN_GUI_SIZE: UVec2 = UVec2::new(320, 240);

/// What the player chose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GuiScaleSetting {
    /// The largest scale that keeps at least [`MIN_GUI_SIZE`] GUI pixels.
    #[default]
    Auto,
    /// This scale, or the automatic one when this one does not fit.
    Fixed(u8),
}

/// The GUI scale. Change [`GuiScale::setting`]; `UiPlugin` computes the rest from the window's
/// physical size at the start of every frame (`UiSystems::Begin`), and the resource only shows
/// a change when the result changes, so `resource_changed::<GuiScale>` tells when to redraw a
/// retained layer.
///
/// The GUI area is the physical size divided by the scale, rounded down, and centered in the
/// window: the few physical pixels left over are split between the borders. Like Minecraft, it
/// ignores the window's DPI scale factor.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuiScale {
    /// The player's choice.
    pub setting: GuiScaleSetting,
    scale: u32,
    physical: UVec2,
}

impl Default for GuiScale {
    fn default() -> Self {
        Self {
            setting: GuiScaleSetting::Auto,
            scale: 1,
            physical: UVec2::ZERO,
        }
    }
}

impl GuiScale {
    /// The largest scale `n` (at least 1) with `physical / n` at least [`MIN_GUI_SIZE`].
    pub fn max_for(physical: UVec2) -> u32 {
        let by_w = physical.x / MIN_GUI_SIZE.x;
        let by_h = physical.y / MIN_GUI_SIZE.y;
        by_w.min(by_h).max(1)
    }

    /// The scale `setting` gives at `physical`: the automatic one, or the fixed one capped by
    /// it.
    pub fn resolve(setting: GuiScaleSetting, physical: UVec2) -> u32 {
        let max = Self::max_for(physical);
        match setting {
            GuiScaleSetting::Auto => max,
            GuiScaleSetting::Fixed(n) => u32::from(n).clamp(1, max),
        }
    }

    /// Recomputes the scale for a window of `physical` pixels. Returns the new value; callers
    /// that keep it in a resource write it only when it differs.
    pub fn computed(&self, physical: UVec2) -> Self {
        Self {
            setting: self.setting,
            scale: Self::resolve(self.setting, physical),
            physical,
        }
    }

    /// Physical pixels per GUI pixel.
    pub const fn scale(&self) -> u32 {
        self.scale
    }

    /// The window size this was computed for, in physical pixels.
    pub const fn physical_size(&self) -> UVec2 {
        self.physical
    }

    /// The GUI area in GUI pixels.
    pub fn gui_size(&self) -> UVec2 {
        self.physical / self.scale
    }

    /// Where GUI pixel (0, 0) starts, in physical pixels (the centering margin).
    pub fn offset(&self) -> IVec2 {
        ((self.physical - self.gui_size() * self.scale) / 2).as_ivec2()
    }

    /// The physical position of the GUI point `p`.
    pub fn to_physical(self, p: IVec2) -> IVec2 {
        self.offset() + p * self.scale as i32
    }

    /// The GUI pixel under the physical position `p` (may lie outside the GUI area).
    pub fn to_gui(self, p: Vec2) -> IVec2 {
        let rel = (p - self.offset().as_vec2()) / self.scale as f32;
        rel.floor().as_ivec2()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_minecraft_table() {
        for (w, h, n) in [
            (1280, 720, 3),
            (1920, 1080, 4),
            (2560, 1440, 6),
            (3840, 2160, 9),
            (320, 240, 1),
            (639, 479, 1),
            (640, 480, 2),
            (200, 100, 1),
            (0, 0, 1),
        ] {
            assert_eq!(
                GuiScale::resolve(GuiScaleSetting::Auto, UVec2::new(w, h)),
                n,
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn fixed_is_capped_by_what_fits() {
        let fhd = UVec2::new(1920, 1080);
        assert_eq!(GuiScale::resolve(GuiScaleSetting::Fixed(2), fhd), 2);
        assert_eq!(GuiScale::resolve(GuiScaleSetting::Fixed(4), fhd), 4);
        assert_eq!(GuiScale::resolve(GuiScaleSetting::Fixed(9), fhd), 4);
        assert_eq!(GuiScale::resolve(GuiScaleSetting::Fixed(0), fhd), 1);
    }

    #[test]
    fn gui_area_is_floored_and_centered() {
        let scale = GuiScale::default().computed(UVec2::new(1281, 722));
        assert_eq!(scale.scale(), 3);
        assert_eq!(scale.gui_size(), UVec2::new(427, 240));
        assert_eq!(scale.offset(), IVec2::new(0, 1));
        assert_eq!(scale.to_physical(IVec2::new(10, 0)), IVec2::new(30, 1));
        assert_eq!(scale.to_gui(Vec2::new(30.5, 1.0)), IVec2::new(10, 0));
        assert_eq!(scale.to_gui(Vec2::new(29.9, 3.9)), IVec2::new(9, 0));
        assert_eq!(scale.to_gui(Vec2::new(0.0, 0.0)), IVec2::new(0, -1));
    }
}
