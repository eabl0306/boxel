//! Hover, clicks, focus and keyboard navigation, resolved once per frame at `UiSystems::Begin`
//! from the previous frame's hit rectangles, and the [`InputCapture`] the game reads.

use crate::memory::{Hit, Memory, UiState, WidgetId, WidgetKind};
use crate::paint::UiLayer;
use boxel_ecs::prelude::*;
use boxel_math::IVec2;
use boxel_window::{KeyCode, TextInput};
use std::time::Duration;

/// What the UI takes from the game this frame. Written at `UiSystems::Begin`.
///
/// A modal layer with content (an open screen) takes the pointer and the keyboard; a widget
/// under the cursor in a non-modal layer takes the pointer; the HUD alone takes nothing. The
/// game gates its own input (picking, the player, the hotbar) with these; the UI never consumes
/// engine input resources.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputCapture {
    /// Clicks and the wheel belong to the UI.
    pub pointer: bool,
    /// Keys belong to the UI (Escape stays the game's).
    pub keyboard: bool,
    /// Typed text belongs to a focused text field.
    pub text: bool,
}

/// A navigation key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NavKey {
    /// Tab.
    Next,
    /// Shift+Tab.
    Prev,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Enter or Space.
    Activate,
    /// Ctrl+Tab: the next tab of a tab bar.
    NextTab,
    /// Ctrl+Shift+Tab: the previous tab.
    PrevTab,
}

impl NavKey {
    /// The navigation meaning of a physical key, if any.
    pub fn from_key(key: KeyCode, shift: bool, ctrl: bool) -> Option<Self> {
        Some(match key {
            KeyCode::Tab if ctrl && shift => Self::PrevTab,
            KeyCode::Tab if ctrl => Self::NextTab,
            KeyCode::Tab if shift => Self::Prev,
            KeyCode::Tab => Self::Next,
            KeyCode::ArrowUp => Self::Up,
            KeyCode::ArrowDown => Self::Down,
            KeyCode::ArrowLeft => Self::Left,
            KeyCode::ArrowRight => Self::Right,
            KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space => Self::Activate,
            _ => return None,
        })
    }
}

/// The navigation keys that also come as a [`TextInput::Key`] (text fields), besides their
/// `KeyboardInput` press. They line the two streams up (Space is typed text, so it is not one
/// of them).
fn in_both_streams(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::Tab
            | KeyCode::ArrowUp
            | KeyCode::ArrowDown
            | KeyCode::ArrowLeft
            | KeyCode::ArrowRight
            | KeyCode::Enter
            | KeyCode::NumpadEnter
    )
}

/// The key of a [`TextInput::Key`] that also comes as a `KeyboardInput` press.
fn both_streams_key(event: &TextInput) -> Option<KeyCode> {
    match event {
        TextInput::Key { key, .. } if in_both_streams(*key) => Some(*key),
        _ => None,
    }
}

/// This frame's input, as the begin system read it.
#[derive(Debug, Default)]
pub(crate) struct NavInput {
    /// The GUI pixel under the cursor.
    pub cursor: Option<IVec2>,
    /// The left button went down.
    pub press: bool,
    /// The left button is down.
    pub held: bool,
    /// Shift is held.
    pub shift: bool,
    /// Ctrl is held.
    pub ctrl: bool,
    /// Navigation keys pressed this frame, in order, with their physical key and the OS repeat
    /// flag.
    pub keys: Vec<(KeyCode, NavKey, bool)>,
    /// The wheel this frame, in lines (positive up).
    pub wheel: f32,
    /// Typed text and editing keys this frame, in order.
    pub text: Vec<TextInput>,
    /// The input method's latest composition this frame, if it sent one.
    pub preedit: Option<String>,
    /// Real time of this frame.
    pub now: Option<Duration>,
}

/// Resolves hover, clicks and keys against the previous frame's rectangles and returns the
/// capture. Does not clear the layers.
pub(crate) fn resolve(state: &mut UiState, input: &NavInput) -> InputCapture {
    let layers = &state.layers;
    let mem = &mut state.memory;
    mem.cursor = input.cursor;
    mem.shift = input.shift;
    mem.ctrl = input.ctrl;
    mem.now = input.now;
    mem.pressed = None;
    mem.activated = None;
    mem.nudge.clear();
    mem.nudge_y.clear();
    mem.wheel = None;
    mem.tab_step = 0;
    mem.reveal = None;
    mem.text.clear();
    mem.ime_area = None;

    // Hover: the last-declared rectangle under the cursor, top layer first; a modal layer
    // with content hides the layers below it (the HUD is never modal). The wheel goes to the
    // last-declared enabled list under the cursor on the same layers, even over its rows'
    // widgets.
    mem.hovered = None;
    mem.modal = None;
    for layer in UiLayer::ALL.into_iter().rev() {
        let state = &layers[layer.index()];
        if let Some(cursor) = input.cursor {
            if mem.hovered.is_none()
                && let Some(hit) = state.hits.iter().rev().find(|h| h.rect.contains(cursor))
            {
                mem.hovered = Some((layer, hit.id));
            }
            if mem.wheel.is_none()
                && input.wheel != 0.0
                && let Some(list) = state
                    .hits
                    .iter()
                    .rev()
                    .find(|h| h.kind.scrolls() && h.enabled && h.rect.contains(cursor))
            {
                mem.wheel = Some((list.id, input.wheel));
            }
        }
        if layer != UiLayer::Hud && state.modal && state.has_content() {
            mem.modal = Some(layer);
            break;
        }
    }
    let find = |id| {
        UiLayer::ALL
            .iter()
            .find_map(|&l| layers[l.index()].hits.iter().find(|h: &&Hit| h.id == id))
    };
    // The keyboard belongs to the enabled widgets of the modal layer that show, in declaration
    // order, and to those a list's rows declared while the list shows (out of view, the list
    // scrolls to them). A focused widget outside them (gone, disabled, clipped away,
    // or under a modal layer opened over it) loses the focus, so Enter cannot activate a
    // widget the player no longer reaches.
    let shows = |id| find(id).is_some_and(|h| !h.rect.is_empty());
    let order: Vec<&Hit> = mem.modal.map_or_else(Vec::new, |layer| {
        layers[layer.index()]
            .hits
            .iter()
            .filter(|h| {
                h.enabled
                    && h.kind.focusable()
                    && (!h.rect.is_empty() || h.row.is_some_and(|(list, _)| shows(list)))
            })
            .collect()
    });
    if mem
        .focused
        .is_some_and(|id| !order.iter().any(|h| h.id == id))
    {
        mem.focused = None;
    }

    // The mouse.
    if input.press {
        let hit = mem.hovered.and_then(|(layer, id)| Some((layer, find(id)?)));
        match hit {
            Some((layer, hit)) if hit.enabled => {
                mem.pressed = Some(hit.id);
                mem.dragging = Some(hit.id);
                // Only the modal layer's widgets take the keyboard.
                let focusable = hit.kind.focusable() && mem.modal == Some(layer);
                mem.focused = focusable.then_some(hit.id);
                mem.focus_visible = false;
            }
            Some(_) => {}
            None => mem.focused = None,
        }
    }
    if !input.held {
        mem.dragging = None;
    }

    // Keys move the focus along `order`, and text goes to the field focused when it was typed.
    // A key that comes in both streams pairs its press with the next `TextInput::Key` of the
    // text stream when that is the same key: the text up to it goes to the field focused before
    // the press. A press without one is a key an input method took (Windows reports them, X11,
    // Wayland and macOS do not) or one winit made up as the window gained focus: it leaves the
    // focus in a text field, so picking a candidate with an arrow does not move it away.
    let field = |id: Option<WidgetId>| {
        id.filter(|&id| order.iter().any(|h| h.id == id && h.kind.takes_text()))
    };
    let deliver = |mem: &mut Memory, events: &[TextInput]| {
        if let Some(id) = field(mem.focused) {
            mem.text.extend(events.iter().map(|e| (id, e.clone())));
        }
    };
    let mut next = 0;
    for &(code, key, repeat) in &input.keys {
        if in_both_streams(code) {
            let pair = input.text[next..]
                .iter()
                .position(|e| both_streams_key(e).is_some())
                .map(|i| next + i)
                .filter(|&i| both_streams_key(&input.text[i]) == Some(code));
            match pair {
                Some(i) => {
                    deliver(mem, &input.text[next..=i]);
                    next = i + 1;
                }
                None if field(mem.focused).is_some() => continue,
                None => {}
            }
        }
        match key {
            NavKey::NextTab => mem.tab_step += 1,
            NavKey::PrevTab => mem.tab_step -= 1,
            _ if order.is_empty() => {}
            _ => navigate(mem, &order, key, repeat),
        }
    }
    deliver(mem, &input.text[next..]);
    mem.focused_row = mem.focused.and_then(find).and_then(|h| h.row);
    let typing = field(mem.focused).is_some();
    if let Some(preedit) = &input.preedit {
        mem.preedit.clone_from(preedit);
    }
    if !typing {
        mem.preedit.clear();
    }

    InputCapture {
        pointer: mem.modal.is_some() || mem.hovered.is_some_and(|(l, _)| l != UiLayer::Hud),
        keyboard: mem.modal.is_some(),
        text: typing,
    }
}

/// Applies one navigation key to the focus along `order` (the focusable widgets in declaration
/// order).
fn navigate(mem: &mut Memory, order: &[&Hit], key: NavKey, repeat: bool) {
    let at = mem
        .focused
        .and_then(|f| order.iter().position(|h| h.id == f));
    let focused = at.map(|i| order[i]);
    let kind = focused.map(|h| h.kind);
    let step = match key {
        // A focused text field reads these from `TextInput` (Space types, Enter submits).
        NavKey::Left | NavKey::Right | NavKey::Activate
            if kind.is_some_and(WidgetKind::takes_text) =>
        {
            0
        }
        NavKey::Left | NavKey::Right if kind.is_some_and(WidgetKind::adjustable) => {
            if let Some(hit) = focused {
                add_step(
                    &mut mem.nudge,
                    hit.id,
                    if key == NavKey::Left { -1 } else { 1 },
                );
            }
            0
        }
        NavKey::Up | NavKey::Down if kind.is_some_and(WidgetKind::vertical) => {
            if let Some(hit) = focused {
                add_step(
                    &mut mem.nudge_y,
                    hit.id,
                    if key == NavKey::Up { -1 } else { 1 },
                );
            }
            0
        }
        NavKey::Next | NavKey::Down | NavKey::Right => 1,
        NavKey::Prev | NavKey::Up | NavKey::Left => -1,
        NavKey::Activate => {
            if !repeat && let Some(hit) = focused {
                mem.activated = Some(hit.id);
            }
            0
        }
        NavKey::NextTab | NavKey::PrevTab => 0,
    };
    if step != 0 {
        let len = order.len() as i64;
        let next = match at {
            None if step > 0 => 0,
            None => len - 1,
            Some(i) => (i as i64 + step).rem_euclid(len),
        };
        let hit = order[next as usize];
        mem.focused = Some(hit.id);
        mem.focus_visible = true;
        mem.reveal = hit.row.map(|(list, _)| (list, hit.full));
    }
}

/// Adds `delta` to the steps of `id` (summed per widget).
fn add_step(steps: &mut Vec<(WidgetId, i32)>, id: WidgetId, delta: i32) {
    match steps.iter_mut().find(|(stepped, _)| *stepped == id) {
        Some((_, total)) => *total += delta,
        None => steps.push((id, delta)),
    }
}
