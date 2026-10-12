//! [`Ui`], the system parameter screens declare widgets with, and its [`Painter`].

use crate::atlas::{SpriteId, UiAtlas};
use crate::color::Rgba;
use crate::font::BitmapFont;
use crate::layout::UiRect;
use crate::memory::{Hit, UiState, WidgetId, WidgetKind};
use crate::paint::{UiLayer, UiQuad};
use crate::scale::GuiScale;
use crate::skin::{SkinSlot, UiSkin, UiStyle};
use crate::sprite::pieces;
use crate::text::{self, Text};
use boxel_ecs::prelude::*;
use boxel_ecs::system::SystemParam;
use boxel_math::IVec2;
use std::ops::Deref;

/// Whether a layer blocks the layers below it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Modal {
    /// Input under this layer's area reaches the layers below.
    #[default]
    No,
    /// While the layer has content it takes the pointer and the keyboard, and Tab and the
    /// arrows move through its widgets (ignored on [`UiLayer::Hud`]).
    Yes,
}

/// What a widget reports this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Response {
    /// Activated: the left button went down on it (as Minecraft's buttons), or Enter or Space
    /// while focused.
    pub clicked: bool,
    /// Its value changed.
    pub changed: bool,
    /// Under the cursor.
    pub hovered: bool,
    /// Has keyboard focus.
    pub focused: bool,
    /// Two clicks in a short time (lists, later).
    pub double_clicked: bool,
}

/// The UI for a system: `fn menu(mut ui: Ui)`, then [`Ui::layer`] to draw.
///
/// Declare widgets in `Update` (between `UiSystems::Begin` and `UiSystems::End`). Hover and
/// clicks are resolved at `Begin` against the previous frame's rectangles, so a widget answers
/// one frame after it first appears.
#[derive(SystemParam)]
pub struct Ui<'w> {
    state: ResMut<'w, UiState>,
    atlas: Res<'w, UiAtlas>,
    skin: Res<'w, UiSkin>,
    font: Res<'w, BitmapFont>,
    style: Res<'w, UiStyle>,
    scale: Res<'w, GuiScale>,
}

impl<'w> Ui<'w> {
    /// Starts drawing `layer`. The first call of a frame for a retained layer
    /// ([`UiLayer::Hud`]) replaces its content; more calls in the frame add to it.
    pub fn layer(&mut self, layer: UiLayer, modal: Modal) -> Painter<'_> {
        self.state.start_layer(layer, modal == Modal::Yes);
        Painter {
            state: &mut self.state,
            atlas: &self.atlas,
            skin: &self.skin,
            font: &self.font,
            style: &self.style,
            scale: &self.scale,
            layer,
            clip: None,
            next_enabled: true,
            next_id: None,
            scopes: Vec::new(),
        }
    }

    /// Empties a layer (needed only for the retained HUD).
    pub fn clear(&mut self, layer: UiLayer) {
        self.state.clear(layer);
    }

    /// The GUI area in GUI pixels.
    pub fn gui_size(&self) -> IVec2 {
        self.scale.gui_size().as_ivec2()
    }

    /// The GUI scale.
    pub fn scale(&self) -> &GuiScale {
        &self.scale
    }
}

/// Draws into one layer and declares widgets. Coordinates are GUI pixels from the top-left of
/// the GUI area ([`Painter::gui_size`]).
pub struct Painter<'a> {
    pub(crate) state: &'a mut UiState,
    pub(crate) atlas: &'a UiAtlas,
    pub(crate) skin: &'a UiSkin,
    pub(crate) font: &'a BitmapFont,
    pub(crate) style: &'a UiStyle,
    pub(crate) scale: &'a GuiScale,
    pub(crate) layer: UiLayer,
    clip: Option<UiRect>,
    next_enabled: bool,
    next_id: Option<WidgetId>,
    scopes: Vec<WidgetId>,
}

/// What a widget returns: its [`Response`] (fields read through it, as in
/// `p.button(r, "Play").clicked`) and [`Resp::tooltip`].
pub struct Resp<'p, 'a> {
    pub(crate) painter: &'p mut Painter<'a>,
    pub(crate) response: Response,
    pub(crate) rect: UiRect,
}

impl Deref for Resp<'_, '_> {
    type Target = Response;
    fn deref(&self) -> &Response {
        &self.response
    }
}

impl Resp<'_, '_> {
    /// Shows `text` in a tooltip while the widget is under the cursor (next to it) or focused
    /// by the keyboard (under the widget). Disabled widgets show theirs too.
    pub fn tooltip(self, text: &str) -> Response {
        let mem = &self.painter.state.memory;
        let anchor = if self.response.hovered {
            mem.cursor.map(|c| (c, true))
        } else if self.response.focused && mem.focus_visible {
            Some((IVec2::new(self.rect.x, self.rect.bottom()), false))
        } else {
            None
        };
        if let Some((at, by_cursor)) = anchor {
            self.painter.draw_tooltip(text, at, by_cursor);
        }
        self.response
    }

    /// The response.
    pub fn response(&self) -> Response {
        self.response
    }
}

/// The input a widget sees this frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Interaction {
    pub enabled: bool,
    pub hovered: bool,
    pub focused: bool,
    /// The left button went down on it.
    pub pressed: bool,
    /// The left button went down on it and is still held.
    pub dragging: bool,
    /// Enter or Space while focused.
    pub activated: bool,
    /// Left and Right steps pressed while it had the focus this frame.
    pub nudge: i32,
    pub shift: bool,
    pub cursor: Option<IVec2>,
}

impl<'a> Painter<'a> {
    /// The GUI area in GUI pixels.
    pub fn gui_size(&self) -> IVec2 {
        self.scale.gui_size().as_ivec2()
    }

    /// The layer being drawn.
    pub fn layer(&self) -> UiLayer {
        self.layer
    }

    /// The next widget is enabled or not (one widget only; then it is enabled again).
    pub fn enabled(&mut self, on: bool) -> &mut Self {
        self.next_enabled = on;
        self
    }

    /// The next widget takes its identity from `id` instead of its label (one widget only).
    pub fn id(&mut self, id: impl std::hash::Hash) -> &mut Self {
        self.next_id = Some(WidgetId::ROOT.mix(&hash_bytes(&id)));
        self
    }

    /// Runs `f` with widget identities scoped by `id`, so the same labels in two scopes (two
    /// rows of a list) are different widgets.
    pub fn scope<R>(&mut self, id: impl std::hash::Hash, f: impl FnOnce(&mut Self) -> R) -> R {
        self.scopes.push(WidgetId::ROOT.mix(&hash_bytes(&id)));
        let out = f(self);
        self.scopes.pop();
        out
    }

    /// Runs `f` with everything it draws cut to `rect` (and to any clip already set). Widgets
    /// and labels inside answer the pointer only where they show, and a widget cut away
    /// entirely cannot take the focus.
    pub fn with_clip<R>(&mut self, rect: UiRect, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.clip;
        self.clip = Some(saved.map_or(rect, |c| c.intersect(&rect)));
        let out = f(self);
        self.clip = saved;
        out
    }

    /// Per-widget data kept between frames (scroll offsets, carets), created with
    /// `T::default()` the first time.
    pub fn widget_data<T: Default + Send + Sync + 'static>(&mut self, id: WidgetId) -> &mut T {
        let slot = self
            .state
            .memory
            .data
            .entry(id)
            .or_insert_with(|| Box::new(T::default()));
        if !slot.is::<T>() {
            *slot = Box::new(T::default());
        }
        slot.downcast_mut::<T>()
            .unwrap_or_else(|| unreachable!("the slot was just made a T"))
    }

    // ---- drawing

    /// A solid rectangle.
    pub fn rect(&mut self, rect: UiRect, color: Rgba) {
        self.push_gui(rect, [0, 0, 1, 1], color);
    }

    /// A translucent color over the whole window (behind a screen's widgets).
    pub fn dim(&mut self, color: Rgba) {
        let size = self.scale.physical_size().as_ivec2();
        self.push(UiQuad::solid([0, 0], [size.x, size.y], color));
    }

    /// A sprite of the atlas over `rect`, cut by its scaling.
    pub fn sprite(&mut self, rect: UiRect, sprite: SpriteId) {
        self.sprite_tinted(rect, sprite, crate::color::WHITE);
    }

    /// A sprite multiplied by `tint`.
    pub fn sprite_tinted(&mut self, rect: UiRect, sprite: SpriteId, tint: Rgba) {
        let mut out = Vec::new();
        pieces(&self.atlas.sprite(sprite), rect, &mut out);
        for piece in out {
            self.push_gui(piece.dst, piece.src, tint);
        }
    }

    /// The skin's sprite for `slot` over `rect`.
    pub fn skin(&mut self, slot: SkinSlot, rect: UiRect) {
        self.sprite(rect, self.skin.get(slot));
    }

    /// Text from `x`, `y` (top-left of the first line). Returns its width in GUI pixels.
    pub fn text(&mut self, x: i32, y: i32, text: &str, style: Text) -> i32 {
        let size = style.size.max(1);
        let placed = text::layout(self.font, text, x, y, size);
        if style.shadow {
            let shadow = crate::color::shadow_of(style.color);
            let d = size as i32;
            for g in &placed {
                self.glyph(g, d, size, shadow);
            }
        }
        for g in &placed {
            self.glyph(g, 0, size, style.color);
        }
        text::measure(self.font, text, size) as i32
    }

    /// Text centered on `center_x`.
    pub fn text_centered(&mut self, center_x: i32, y: i32, text: &str, style: Text) {
        let w = self.measure(text, style.size) as i32;
        self.text(center_x - w / 2, y, text, style);
    }

    /// The width of a line in GUI pixels at `size`.
    pub fn measure(&self, text: &str, size: u32) -> u32 {
        text::measure(self.font, text, size)
    }

    /// `text` wrapped to `max_width` GUI pixels.
    pub fn wrap(&self, text: &str, max_width: u32, size: u32) -> Vec<String> {
        text::wrap(self.font, text, max_width, size)
    }

    /// A line of text in `rect`: left-aligned, centered vertically, cut with `"..."` when too
    /// long. Not a widget (no focus, no clicks); it reports hover for tooltips.
    pub fn label(&mut self, rect: UiRect, text: &str, style: Text) -> Resp<'_, 'a> {
        let size = style.size.max(1);
        let line = text::ellipsis(self.font, text, rect.w.max(0) as u32, size);
        let y = rect.y + (rect.h - (crate::font::GLYPH_HEIGHT * size) as i32) / 2;
        self.text(rect.x, y, &line, style);
        let shown = self.clipped(rect);
        let mem = &self.state.memory;
        let reachable = mem.modal.is_none_or(|m| self.layer >= m);
        let hovered = reachable && mem.cursor.is_some_and(|c| shown.contains(c));
        Resp {
            painter: self,
            response: Response {
                hovered,
                ..Response::default()
            },
            rect,
        }
    }

    // ---- internals shared by the widgets

    /// Registers a widget's rectangle and returns what input it gets this frame.
    pub(crate) fn interact(&mut self, rect: UiRect, kind: WidgetKind, label: &str) -> Interaction {
        let enabled = std::mem::replace(&mut self.next_enabled, true);
        let base = self.next_id.take().unwrap_or_else(|| {
            WidgetId::ROOT
                .mix(&[self.layer.index() as u8, kind as u8])
                .mix(label.as_bytes())
        });
        let base = self
            .scopes
            .iter()
            .fold(base, |id, scope| id.mix(&scope.0.to_le_bytes()));
        let id = self.state.unique_id(self.layer, base);
        // The part that shows: a widget's hidden part must not take input from what is drawn
        // there (a scrolled row over a header).
        let rect = self.clipped(rect);
        self.state.layers[self.layer.index()].hits.push(Hit {
            id,
            rect,
            enabled,
            kind,
            label: label.to_owned(),
        });
        self.state.dirty = true;
        let mem = &self.state.memory;
        let hovered = mem.hovered == Some((self.layer, id));
        let focused = enabled && mem.focused == Some(id);
        Interaction {
            enabled,
            hovered,
            focused,
            pressed: enabled && mem.pressed == Some(id),
            dragging: enabled && mem.dragging == Some(id),
            activated: enabled && mem.activated == Some(id),
            nudge: mem
                .nudge
                .iter()
                .find(|&&(nudged, _)| enabled && nudged == id)
                .map_or(0, |&(_, n)| n),
            shift: mem.shift,
            cursor: mem.cursor,
        }
    }

    /// The part of `rect` the clip leaves (empty when it is cut away).
    fn clipped(&self, rect: UiRect) -> UiRect {
        self.clip.map_or(rect, |clip| clip.intersect(&rect))
    }

    /// A widget label centered in `rect` (ellipsis when too long), white or gray.
    pub(crate) fn centered_label(&mut self, rect: UiRect, label: &str, enabled: bool) {
        let max = (rect.w - 6).max(0) as u32;
        let line = text::ellipsis(self.font, label, max, 1);
        let color = if enabled {
            self.style.text
        } else {
            self.style.text_disabled
        };
        let y = rect.y + (rect.h - crate::font::GLYPH_HEIGHT as i32) / 2;
        self.text_centered(rect.x + rect.w / 2, y, &line, Text::new(color));
    }

    fn glyph(&mut self, g: &text::PlacedGlyph, offset: i32, size: u32, color: Rgba) {
        let [w, h] = g.glyph.size;
        let dst = UiRect::new(
            g.x + offset,
            g.y + offset,
            (w * size) as i32,
            (h * size) as i32,
        );
        self.push_gui(dst, g.glyph.src, color);
    }

    /// A quad from a GUI rectangle and an atlas rectangle `[x, y, w, h]`.
    pub(crate) fn push_gui(&mut self, dst: UiRect, src: [u32; 4], tint: Rgba) {
        if dst.is_empty() || src[2] == 0 || src[3] == 0 {
            return;
        }
        let n = self.scale.scale();
        let origin = self.scale.to_physical(IVec2::new(dst.x, dst.y));
        self.push(UiQuad::mapped(
            [origin.x, origin.y],
            [dst.w as u32 * n, dst.h as u32 * n],
            [src[0], src[1]],
            [src[2], src[3]],
            tint,
        ));
    }

    /// Adds a physical quad, cut to the clip.
    pub(crate) fn push(&mut self, quad: UiQuad) {
        let quad = match self.clip {
            None => Some(quad),
            Some(clip) => {
                let min = self.scale.to_physical(IVec2::new(clip.x, clip.y));
                let max = self
                    .scale
                    .to_physical(IVec2::new(clip.right(), clip.bottom()));
                quad.clipped([min.x, min.y], [max.x, max.y])
            }
        };
        if let Some(quad) = quad {
            self.state.layers[self.layer.index()].quads.push(quad);
            self.state.dirty = true;
        }
    }

    /// Draws a tooltip box on the popup layer: next to the cursor at `at`, or under a widget
    /// whose bottom-left is `at`; kept inside the GUI area.
    fn draw_tooltip(&mut self, text: &str, at: IVec2, by_cursor: bool) {
        let lines = self.wrap(text, self.style.tooltip_width, 1);
        let w = lines
            .iter()
            .map(|l| self.measure(l, 1) as i32)
            .max()
            .unwrap_or(0);
        let h = lines.len() as i32 * 10 - 2;
        let gui = self.gui_size();
        let (mut x, mut y) = if by_cursor {
            (at.x + 12, at.y - 12)
        } else {
            (at.x + 4, at.y + 4)
        };
        if x + w + 4 > gui.x {
            x = if by_cursor {
                at.x - 16 - w
            } else {
                gui.x - w - 4
            };
        }
        y = y.min(gui.y - h - 4).max(4);
        x = x.max(4);

        let (saved_layer, saved_clip) = (self.layer, self.clip.take());
        self.state.start_layer(UiLayer::Popup, false);
        self.layer = UiLayer::Popup;
        let style = self.style;
        let bg = style.tooltip_background;
        self.rect(UiRect::new(x - 3, y - 4, w + 6, 1), bg);
        self.rect(UiRect::new(x - 3, y + h + 3, w + 6, 1), bg);
        self.rect(UiRect::new(x - 3, y - 3, w + 6, h + 6), bg);
        self.rect(UiRect::new(x - 4, y - 3, 1, h + 6), bg);
        self.rect(UiRect::new(x + w + 3, y - 3, 1, h + 6), bg);
        let (top, bottom) = (style.tooltip_border_top, style.tooltip_border_bottom);
        let half = (h + 4) / 2;
        for side in [x - 3, x + w + 2] {
            self.rect(UiRect::new(side, y - 2, 1, half), top);
            self.rect(UiRect::new(side, y - 2 + half, 1, h + 4 - half), bottom);
        }
        self.rect(UiRect::new(x - 3, y - 3, w + 6, 1), top);
        self.rect(UiRect::new(x - 3, y + h + 2, w + 6, 1), bottom);
        for (i, line) in lines.iter().enumerate() {
            self.text(x, y + i as i32 * 10, line, Text::new(style.text));
        }
        self.layer = saved_layer;
        self.clip = saved_clip;
    }
}

/// The bytes of a value's hash, for widget ids.
fn hash_bytes(value: &impl std::hash::Hash) -> [u8; 8] {
    use std::hash::Hasher;
    let mut hasher = Fnv(WidgetId::ROOT.0);
    value.hash(&mut hasher);
    hasher.finish().to_le_bytes()
}

/// FNV-1a 64 as a `Hasher`: stable across runs and platforms, unlike the standard one.
struct Fnv(u64);

impl std::hash::Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        self.0 = WidgetId(self.0).mix(bytes).0;
    }
}
