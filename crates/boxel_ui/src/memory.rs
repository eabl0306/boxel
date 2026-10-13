//! What the UI remembers between frames: each layer's quads and hit rectangles, and the widget
//! state keyed by [`WidgetId`] (hover, focus, the pressed or dragged widget, per-widget data).

use crate::layout::UiRect;
use crate::paint::{UiLayer, UiQuad};
use boxel_ecs::prelude::*;
use boxel_math::IVec2;
use boxel_window::{ImeCursorArea, ImeOptions, TextInput};
use std::any::Any;
use std::collections::HashMap;
use std::time::Duration;

/// A widget's identity: a hash of its layer, the id scopes around it and its label (or an
/// explicit id), plus how many widgets with the same hash came before it in the frame. Stable
/// while a screen declares the same widgets in the same order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WidgetId(pub u64);

impl WidgetId {
    /// FNV-1a 64 of `bytes` continued from this id.
    pub(crate) fn mix(self, bytes: &[u8]) -> Self {
        Self(bytes.iter().fold(self.0, |hash, &b| {
            (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        }))
    }

    /// The hash every id starts from (FNV-1a's offset basis).
    pub(crate) const ROOT: Self = Self(0xcbf2_9ce4_8422_2325);
}

/// What kind of widget a hit rectangle belongs to (for keyboard navigation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WidgetKind {
    /// Activated by a click, Enter or Space.
    Button,
    /// Steps through values; Left and Right step it.
    Cycle,
    /// A value along a track; Left and Right step it.
    Slider,
    /// A scrolling list; Up and Down move its selection.
    List,
    /// A scrolling list without a selection (its rows hold widgets): it takes the wheel, not
    /// the focus.
    ScrollList,
    /// A text field; it takes typed text, and Space, Left, Right and Enter edit it.
    TextField,
    /// One tab of a tab bar; Left and Right select the tab next to it.
    Tab,
}

impl WidgetKind {
    /// Whether Tab and the arrows can focus it.
    pub const fn focusable(self) -> bool {
        !matches!(self, Self::ScrollList)
    }

    /// Whether the wheel scrolls it.
    pub const fn scrolls(self) -> bool {
        matches!(self, Self::List | Self::ScrollList)
    }

    /// Whether Left and Right change its value (else they move the focus).
    pub const fn adjustable(self) -> bool {
        matches!(self, Self::Cycle | Self::Slider | Self::Tab)
    }

    /// Whether Up and Down change its value (else they move the focus).
    pub const fn vertical(self) -> bool {
        matches!(self, Self::List)
    }

    /// Whether it takes typed text: while it has the focus, Space, Left, Right and Enter are
    /// its own (Tab, Up and Down still move the focus).
    pub const fn takes_text(self) -> bool {
        matches!(self, Self::TextField)
    }
}

/// A widget's rectangle as declared in a frame, used for hover, clicks and navigation in the
/// next one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The widget.
    pub id: WidgetId,
    /// Where it takes the pointer, in GUI pixels: its rectangle cut to the clip it was declared
    /// in (empty when cut away, and then it cannot take the focus either, unless a list's row
    /// declared it).
    pub rect: UiRect,
    /// Its whole rectangle, before the clip.
    pub full: UiRect,
    /// The list and the row that declared it. Scrolled out of view, it can still take the
    /// keyboard focus, and the list then scrolls to show it.
    pub row: Option<(WidgetId, usize)>,
    /// Disabled widgets get hover (for tooltips) but no clicks or focus.
    pub enabled: bool,
    /// Its kind.
    pub kind: WidgetKind,
    /// Its label, as declared (for tests and tools).
    pub label: String,
}

/// One layer's retained content.
#[derive(Debug, Default)]
pub(crate) struct LayerState {
    pub quads: Vec<UiQuad>,
    pub hits: Vec<Hit>,
    pub modal: bool,
    /// The frame the layer was last started in.
    pub drawn: Option<u64>,
    /// How many times each base id was declared in the current frame.
    pub ids: HashMap<WidgetId, u32>,
}

impl LayerState {
    pub fn has_content(&self) -> bool {
        !self.quads.is_empty() || !self.hits.is_empty()
    }

    pub fn clear(&mut self) {
        self.quads.clear();
        self.hits.clear();
        self.modal = false;
        self.ids.clear();
    }
}

/// Input as `UiSystems::Begin` resolved it for this frame.
#[derive(Debug, Default)]
pub(crate) struct Memory {
    /// The GUI pixel under the cursor.
    pub cursor: Option<IVec2>,
    /// The widget under the cursor (from the previous frame's rectangles).
    pub hovered: Option<(UiLayer, WidgetId)>,
    /// The topmost modal layer with content: layers below it get no input.
    pub modal: Option<UiLayer>,
    /// The widget with keyboard focus: always an enabled widget of the `modal` layer that shows.
    pub focused: Option<WidgetId>,
    /// Focus moved by the keyboard (tooltips follow it) rather than by a click.
    pub focus_visible: bool,
    /// The widget the left button went down on this frame.
    pub pressed: Option<WidgetId>,
    /// The widget the left button went down on and is still held.
    pub dragging: Option<WidgetId>,
    /// The focused widget Enter or Space activated this frame.
    pub activated: Option<WidgetId>,
    /// Left (-1) and Right (+1) steps this frame, summed per widget: the focus can move
    /// between two presses of one frame, and each step stays with the widget it was meant for.
    pub nudge: Vec<(WidgetId, i32)>,
    /// Up (-1) and Down (+1) steps for lists, as `nudge`.
    pub nudge_y: Vec<(WidgetId, i32)>,
    /// The wheel this frame (lines, positive up) and the list it scrolls: the last-declared
    /// enabled list under the cursor.
    pub wheel: Option<(WidgetId, f32)>,
    /// Ctrl+Tab (+1) and Ctrl+Shift+Tab (-1) presses, for the first tab bar of the modal layer.
    pub tab_step: i32,
    /// The keyboard moved the focus this frame to a list's row widget: that list and the
    /// widget's whole rectangle in the previous frame, so the list scrolls to show it.
    pub reveal: Option<(WidgetId, UiRect)>,
    /// The list and row of the focused widget, which the list declares even out of view.
    pub focused_row: Option<(WidgetId, usize)>,
    /// Typed text and editing keys of this frame, each with the text field that had the focus
    /// when it was typed.
    pub text: Vec<(WidgetId, TextInput)>,
    /// The input method's composition, shown at the focused text field's caret.
    pub preedit: String,
    /// The focus moved this frame to another text field while a composition was open: the
    /// composition was dropped, and the input method goes off for this frame so the platform
    /// ends it too (its commit would land in the new field).
    pub ime_restart: bool,
    /// Where the focused text field drew its caret this frame, for the input method.
    pub ime_area: Option<ImeCursorArea>,
    /// What the UI last wrote into `ImeOptions`.
    pub ime_written: Option<ImeOptions>,
    /// Real time of this frame (from `Time`), for double clicks and the caret's blink; `None`
    /// without a `Time` resource.
    pub now: Option<Duration>,
    /// Shift was held.
    pub shift: bool,
    /// Ctrl was held.
    pub ctrl: bool,
    /// Per-widget data.
    pub data: HashMap<WidgetId, Box<dyn Any + Send + Sync>>,
}

/// The UI's state between frames: layers, hit rectangles and widget memory. Systems use it
/// through [`crate::Ui`]; tests and tools read it (see [`crate::testing::UiTester`]).
#[derive(Resource, Debug, Default)]
pub struct UiState {
    pub(crate) frame: u64,
    pub(crate) layers: [LayerState; 3],
    pub(crate) memory: Memory,
    /// Something changed since the last composition.
    pub(crate) dirty: bool,
}

impl UiState {
    /// The hit rectangles a layer declared in its latest frame, in declaration order.
    pub fn hits(&self, layer: UiLayer) -> &[Hit] {
        &self.layers[layer.index()].hits
    }

    /// The quads a layer holds.
    pub fn quads(&self, layer: UiLayer) -> &[UiQuad] {
        &self.layers[layer.index()].quads
    }

    /// The widget with keyboard focus.
    pub fn focused(&self) -> Option<WidgetId> {
        self.memory.focused
    }

    /// The widget under the cursor this frame.
    pub fn hovered(&self) -> Option<WidgetId> {
        self.memory.hovered.map(|(_, id)| id)
    }

    /// The hit of a widget, searching the top layer first.
    pub fn hit(&self, id: WidgetId) -> Option<&Hit> {
        UiLayer::ALL
            .iter()
            .rev()
            .find_map(|&l| self.hits(l).iter().find(|h| h.id == id))
    }

    /// The first widget declared with `label`, searching the top layer first.
    pub fn hit_by_label(&self, label: &str) -> Option<&Hit> {
        UiLayer::ALL
            .iter()
            .rev()
            .find_map(|&l| self.hits(l).iter().find(|h| h.label == label))
    }

    /// Empties a layer (its quads, rectangles and modality).
    pub fn clear(&mut self, layer: UiLayer) {
        let state = &mut self.layers[layer.index()];
        self.dirty |= state.has_content();
        state.clear();
    }

    /// Starts drawing `layer` this frame: a retained layer drawn for the first time this frame
    /// loses its old content.
    pub(crate) fn start_layer(&mut self, layer: UiLayer, modal: bool) {
        let frame = self.frame;
        let state = &mut self.layers[layer.index()];
        if state.drawn != Some(frame) {
            if layer.retained() {
                self.dirty |= state.has_content();
                state.clear();
            }
            state.ids.clear();
            state.drawn = Some(frame);
        }
        if modal && !state.modal {
            state.modal = true;
            self.dirty = true;
        }
    }

    /// The id of the next widget with base id `base` in `layer`.
    pub(crate) fn unique_id(&mut self, layer: UiLayer, base: WidgetId) -> WidgetId {
        let count = self.layers[layer.index()].ids.entry(base).or_insert(0);
        let id = if *count == 0 {
            base
        } else {
            base.mix(&count.to_le_bytes())
        };
        *count += 1;
        id
    }
}
