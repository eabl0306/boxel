//! `boxel_ui`: an optional UI for games on boxel (ADR 0033): menus and a HUD drawn with sprites
//! and bitmap text at an integer GUI scale, the same pixels on every GPU tier.
//!
//! The engine retains and the game declares:
//!
//! - **The game declares.** Screens are ordinary systems that take [`Ui`], open a layer with
//!   [`Ui::layer`] and declare their widgets every frame they are open (`p.button(rect,
//!   "Play").clicked`). Layouts are fixed GUI-pixel rectangles ([`Column`], [`Grid2`],
//!   [`UiRect::split`]), no solver, no entity per widget. Which screens exist, their words and
//!   what they change are the game's.
//! - **The engine retains.** Widget state between frames (focus, the pressed or dragged
//!   widget), one quad list per [`UiLayer`] (the HUD keeps its quads until it is drawn again),
//!   the composed [`UiDraw`] whose generation moves only when its bytes change, and one
//!   [`UiAtlas`] page. A still HUD and an idle menu cost no upload.
//!
//! Frame: `UiSystems::Begin` (`PreUpdate`, after input) resolves hover, clicks and keys
//! against the previous frame's rectangles and writes [`InputCapture`]; `Update` declares;
//! `UiSystems::End` (`Last`) composes [`UiDraw`]. A renderer draws [`UiDraw`] with the atlas,
//! one texture read per pixel by integer arithmetic ([`UiQuad::texel_at`]); [`raster`] is the
//! CPU reference of that drawing.
//!
//! The default look is procedural ([`UiSkin::procedural`]) and the default font is `font8x8`
//! (public domain, ASCII and Latin-1); a game maps its own sprites to the skin's slots and
//! builds its own [`BitmapFont`] with [`FontBuilder`]. With the feature `pack` they can come
//! from a Minecraft resource pack read by `boxel_pack`'s `GuiPack`: `UiAtlas::insert_pack_sprite`
//! keeps a GUI sprite's `gui.scaling` and `FontBuilder::pack_font` adds a pack font's bitmap and
//! space providers. [`UiPlugin`] is not in `DefaultPlugins`.

mod atlas;
pub mod color;
mod font;
pub mod font8x8;
mod layout;
mod memory;
mod nav;
#[cfg(feature = "pack")]
mod pack;
mod paint;
mod plugin;
pub mod raster;
mod scale;
mod skin;
mod sprite;
pub mod testing;
pub mod text;
mod ui;
mod widgets;

pub use atlas::{AtlasError, DEFAULT_ATLAS_SIZE, MAX_ATLAS_SIZE, SpriteId, UiAtlas};
pub use font::{
    BitmapFont, Derive, FontBuilder, GLYPH_HEIGHT, Glyph, GlyphBitmap, LINE_HEIGHT, SPACE_ADVANCE,
    default_font,
};
pub use layout::{Column, Grid2, UiRect};
pub use memory::{Hit, UiState, WidgetId, WidgetKind};
pub use nav::InputCapture;
pub use paint::{UiDraw, UiLayer, UiQuad};
pub use plugin::{UiPlugin, UiSystems};
pub use scale::{GuiScale, GuiScaleSetting, MIN_GUI_SIZE};
pub use skin::{SkinSlot, UiSkin, UiStyle};
pub use sprite::{Piece, Scaling, Sprite, pieces};
pub use text::Text;
pub use ui::{Modal, Painter, Resp, Response, Ui};
pub use widgets::{CARET_BLINK, DOUBLE_CLICK, ListRow, SCROLLBAR_GAP, SCROLLBAR_WIDTH, Steps};

/// Common UI items (re-exported by `boxel::prelude` with the `ui` feature).
pub mod prelude {
    pub use crate::{
        Column, Grid2, GuiScale, GuiScaleSetting, InputCapture, ListRow, Modal, Steps, Text, Ui,
        UiLayer, UiPlugin, UiRect, UiSystems,
    };
}
