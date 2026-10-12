//! What the UI remembers between frames: each layer's quads and hit rectangles, and the widget
//! state keyed by [`WidgetId`] (hover, focus, the pressed or dragged widget, per-widget data).

use crate::layout::UiRect;
use crate::paint::{UiLayer, UiQuad};
use boxel_ecs::prelude::*;
use boxel_math::IVec2;
use std::any::Any;
use std::collections::HashMap;

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
}

impl WidgetKind {
    /// Whether Tab and the arrows can focus it.
    pub const fn focusable(self) -> bool {
        true
    }

    /// Whether Left and Right change its value (else they move the focus).
    pub const fn adjustable(self) -> bool {
        matches!(self, Self::Cycle | Self::Slider)
    }
}

/// A widget's rectangle as declared in a frame, used for hover, clicks and navigation in the
/// next one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The widget.
    pub id: WidgetId,
    /// Where it is, in GUI pixels.
    pub rect: UiRect,
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
    /// The widget with keyboard focus.
    pub focused: Option<WidgetId>,
    /// Focus moved by the keyboard (tooltips follow it) rather than by a click.
    pub focus_visible: bool,
    /// The widget the left button went down on this frame.
    pub pressed: Option<WidgetId>,
    /// The widget the left button went down on and is still held.
    pub dragging: Option<WidgetId>,
    /// The focused widget Enter or Space activated this frame.
    pub activated: Option<WidgetId>,
    /// Left (-1) and Right (+1) steps for the focused widget this frame.
    pub nudge: Option<(WidgetId, i32)>,
    /// Shift was held.
    pub shift: bool,
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
