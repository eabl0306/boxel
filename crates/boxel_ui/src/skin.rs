//! The skin: one sprite per widget slot and state, and the colors widgets draw with. The
//! engine's default is procedural (drawn by code, no image file); a game points slots at its
//! own sprites (from a pack's GUI sprites, for example).

use crate::atlas::{AtlasError, SpriteId, UiAtlas};
use crate::color::{Rgba, argb, rgb};
use crate::font::GlyphBitmap;
use crate::sprite::Scaling;
use boxel_ecs::prelude::*;

/// A skin slot: a widget part in one state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkinSlot {
    /// A button.
    Button,
    /// A button under the cursor or focused.
    ButtonHovered,
    /// A disabled button.
    ButtonDisabled,
    /// A slider's track.
    SliderTrack,
    /// A slider's track under the cursor or focused.
    SliderTrackHovered,
    /// A slider's handle.
    SliderHandle,
    /// A slider's handle under the cursor or focused.
    SliderHandleHovered,
    /// A text field.
    TextField,
    /// A focused text field.
    TextFieldFocused,
    /// An unchecked checkbox.
    Checkbox,
    /// An unchecked checkbox under the cursor.
    CheckboxHovered,
    /// A checked checkbox.
    CheckboxChecked,
    /// A checked checkbox under the cursor.
    CheckboxCheckedHovered,
    /// A tab.
    Tab,
    /// A tab under the cursor.
    TabHovered,
    /// The selected tab.
    TabSelected,
    /// The selected tab under the cursor.
    TabSelectedHovered,
    /// A list's scrollbar handle.
    Scroller,
    /// A list's scrollbar track.
    ScrollerBackground,
    /// The tiled background behind menus without a world.
    MenuBackground,
    /// The background of a list.
    ListBackground,
    /// The line under a screen's header.
    HeaderSeparator,
    /// The line above a screen's footer.
    FooterSeparator,
}

impl SkinSlot {
    /// Every slot.
    pub const ALL: [Self; 23] = [
        Self::Button,
        Self::ButtonHovered,
        Self::ButtonDisabled,
        Self::SliderTrack,
        Self::SliderTrackHovered,
        Self::SliderHandle,
        Self::SliderHandleHovered,
        Self::TextField,
        Self::TextFieldFocused,
        Self::Checkbox,
        Self::CheckboxHovered,
        Self::CheckboxChecked,
        Self::CheckboxCheckedHovered,
        Self::Tab,
        Self::TabHovered,
        Self::TabSelected,
        Self::TabSelectedHovered,
        Self::Scroller,
        Self::ScrollerBackground,
        Self::MenuBackground,
        Self::ListBackground,
        Self::HeaderSeparator,
        Self::FooterSeparator,
    ];
}

/// One sprite per [`SkinSlot`], all in the [`UiAtlas`]. `UiPlugin` inserts
/// [`UiSkin::procedural`]; change a slot with [`UiSkin::set`].
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct UiSkin {
    sprites: [SpriteId; SkinSlot::ALL.len()],
}

impl UiSkin {
    /// The sprite of a slot.
    pub fn get(&self, slot: SkinSlot) -> SpriteId {
        self.sprites[slot as usize]
    }

    /// Points a slot at another sprite of the same atlas.
    pub fn set(&mut self, slot: SkinSlot, sprite: SpriteId) {
        self.sprites[slot as usize] = sprite;
    }

    /// The engine's skin, drawn into `atlas` by code: a black outline, a light top-left and a
    /// dark bottom-right bevel and a flat fill, lighter when hovered. Its edges and centers are
    /// flat and stretched, so a button is 9 quads at any size.
    pub fn procedural(atlas: &mut UiAtlas) -> Result<Self, AtlasError> {
        let mut add = |slot: SkinSlot| -> Result<SpriteId, AtlasError> {
            let (image, scaling) = procedural_sprite(slot);
            atlas.add_sprite(image.w, image.h, &image.rgba, scaling)
        };
        let mut sprites = [None; SkinSlot::ALL.len()];
        for slot in SkinSlot::ALL {
            sprites[slot as usize] = Some(add(slot)?);
        }
        Ok(Self {
            sprites: sprites.map(|s| s.unwrap_or_else(|| unreachable!("every slot drawn"))),
        })
    }
}

/// A beveled box: outline, light top-left, dark bottom-right, fill.
fn bevel(w: u32, h: u32, outline: Rgba, light: Rgba, dark: Rgba, fill: Rgba) -> GlyphBitmap {
    let mut image = GlyphBitmap::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let color = if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                outline
            } else if x == w - 2 || y == h - 2 {
                dark
            } else if x == 1 || y == 1 {
                light
            } else {
                fill
            };
            image.set(x, y, color);
        }
    }
    image
}

/// A box with a one-pixel outline.
fn framed(w: u32, h: u32, outline: Rgba, fill: Rgba) -> GlyphBitmap {
    let mut image = GlyphBitmap::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let edge = x == 0 || y == 0 || x == w - 1 || y == h - 1;
            image.set(x, y, if edge { outline } else { fill });
        }
    }
    image
}

/// A flat image.
fn flat(w: u32, h: u32, color: Rgba) -> GlyphBitmap {
    framed(w, h, color, color)
}

const fn nine(w: u32, h: u32, border: u32) -> Scaling {
    Scaling::NineSlice {
        w,
        h,
        border: [border; 4],
        stretch_inner: true,
    }
}

fn procedural_sprite(slot: SkinSlot) -> (GlyphBitmap, Scaling) {
    let black = rgb(0x000000);
    let button = |light: u32, dark: u32, fill: u32| {
        (
            bevel(8, 8, black, rgb(light), rgb(dark), rgb(fill)),
            nine(8, 8, 2),
        )
    };
    let checkbox = |outline: u32, checked: bool| {
        let mut image = framed(20, 20, rgb(outline), argb(0xFF000000));
        if checked {
            for y in 6..14 {
                for x in 6..14 {
                    image.set(x, y, rgb(0xFFFFFF));
                }
            }
        }
        (image, Scaling::Stretch)
    };
    match slot {
        SkinSlot::Button | SkinSlot::SliderHandle => button(0xA8A8A8, 0x4C4C4C, 0x707070),
        SkinSlot::ButtonHovered | SkinSlot::SliderHandleHovered => {
            button(0xD0D0E8, 0x60607C, 0x8A8AA8)
        }
        SkinSlot::ButtonDisabled | SkinSlot::SliderTrack => button(0x3C3C3C, 0x1C1C1C, 0x2C2C2C),
        SkinSlot::SliderTrackHovered => button(0x4C4C4C, 0x242424, 0x343434),
        SkinSlot::TextField => (framed(8, 8, rgb(0xA0A0A0), rgb(0x000000)), nine(8, 8, 1)),
        SkinSlot::TextFieldFocused => (framed(8, 8, rgb(0xFFFFFF), rgb(0x000000)), nine(8, 8, 1)),
        SkinSlot::Checkbox => checkbox(0xA0A0A0, false),
        SkinSlot::CheckboxHovered => checkbox(0xFFFFFF, false),
        SkinSlot::CheckboxChecked => checkbox(0xA0A0A0, true),
        SkinSlot::CheckboxCheckedHovered => checkbox(0xFFFFFF, true),
        SkinSlot::Tab => button(0x8C8C8C, 0x3C3C3C, 0x5C5C5C),
        SkinSlot::TabHovered => button(0xB8B8D0, 0x50506C, 0x7A7A98),
        SkinSlot::TabSelected => button(0xC8C8C8, 0x5C5C5C, 0x8C8C8C),
        SkinSlot::TabSelectedHovered => button(0xE0E0F0, 0x70708C, 0x9A9AB8),
        SkinSlot::Scroller => button(0xC0C0C0, 0x808080, 0xA0A0A0),
        SkinSlot::ScrollerBackground => (flat(2, 2, argb(0x80000000)), Scaling::Stretch),
        SkinSlot::MenuBackground => {
            // A 16-pixel tile of two dark grays in a checker of 4-pixel squares.
            let mut image = GlyphBitmap::new(16, 16);
            for y in 0..16 {
                for x in 0..16 {
                    let even = (x / 4 + y / 4) % 2 == 0;
                    image.set(x, y, rgb(if even { 0x2A2A2A } else { 0x262626 }));
                }
            }
            (image, Scaling::Tile { w: 16, h: 16 })
        }
        SkinSlot::ListBackground => (flat(2, 2, argb(0x60000000)), Scaling::Stretch),
        SkinSlot::HeaderSeparator | SkinSlot::FooterSeparator => {
            // A dark and a light line; the header's light line is below, the footer's above.
            let (dark, light) = (argb(0xFF000000), argb(0x40FFFFFF));
            let (top, bottom) = if slot == SkinSlot::HeaderSeparator {
                (dark, light)
            } else {
                (light, dark)
            };
            let mut image = GlyphBitmap::new(2, 2);
            for x in 0..2 {
                image.set(x, 0, top);
                image.set(x, 1, bottom);
            }
            (image, Scaling::Tile { w: 2, h: 2 })
        }
    }
}

/// Colors and words the widgets use (not sprites). A game sets its own words: the engine's are
/// placeholders.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct UiStyle {
    /// Text of enabled widgets.
    pub text: Rgba,
    /// Text of disabled widgets.
    pub text_disabled: Rgba,
    /// Tooltip background.
    pub tooltip_background: Rgba,
    /// Tooltip border, top half.
    pub tooltip_border_top: Rgba,
    /// Tooltip border, bottom half.
    pub tooltip_border_bottom: Rgba,
    /// Tooltips wrap at this width in GUI pixels.
    pub tooltip_width: u32,
    /// The value a toggle shows when on.
    pub on: String,
    /// The value a toggle shows when off.
    pub off: String,
    /// Between a cycle's or a slider's label and its value (`"Label: value"`).
    pub separator: String,
}

impl Default for UiStyle {
    fn default() -> Self {
        Self {
            text: rgb(0xFFFFFF),
            text_disabled: rgb(0xA0A0A0),
            tooltip_background: argb(0xF0100010),
            tooltip_border_top: argb(0x505000FF),
            tooltip_border_bottom: argb(0x5028007F),
            tooltip_width: 200,
            on: "On".to_owned(),
            off: "Off".to_owned(),
            separator: ": ".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procedural_skin_fills_every_slot() {
        let mut atlas = UiAtlas::new(256);
        let skin = UiSkin::procedural(&mut atlas).unwrap();
        for slot in SkinSlot::ALL {
            let sprite = atlas.sprite(skin.get(slot));
            assert!(sprite.src[2] > 0 && sprite.src[3] > 0, "{slot:?}");
        }
        // The button: black outline, light bevel, flat fill.
        let button = atlas.sprite(skin.get(SkinSlot::Button));
        let [x, y, ..] = button.src;
        assert_eq!(atlas.texel(x, y), [0, 0, 0, 255]);
        assert_eq!(atlas.texel(x + 1, y + 1), rgb(0xA8A8A8));
        assert_eq!(atlas.texel(x + 3, y + 3), rgb(0x707070));
        assert_eq!(atlas.texel(x + 6, y + 6), rgb(0x4C4C4C));
        let mut changed = skin.clone();
        changed.set(SkinSlot::Button, skin.get(SkinSlot::Tab));
        assert_eq!(changed.get(SkinSlot::Button), skin.get(SkinSlot::Tab));
    }
}
