//! Hover, clicks, focus and keyboard navigation, resolved once per frame at `UiSystems::Begin`
//! from the previous frame's hit rectangles, and the [`InputCapture`] the game reads.

use crate::memory::{Hit, UiState};
use crate::paint::UiLayer;
use boxel_ecs::prelude::*;
use boxel_math::IVec2;
use boxel_window::KeyCode;

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
}

impl NavKey {
    /// The navigation meaning of a physical key, if any.
    pub fn from_key(key: KeyCode, shift: bool) -> Option<Self> {
        Some(match key {
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
    /// Navigation keys pressed this frame, in order, with the OS repeat flag.
    pub keys: Vec<(NavKey, bool)>,
}

/// Resolves hover, clicks and keys against the previous frame's rectangles and returns the
/// capture. Does not clear the layers.
pub(crate) fn resolve(state: &mut UiState, input: &NavInput) -> InputCapture {
    let layers = &state.layers;
    let mem = &mut state.memory;
    mem.cursor = input.cursor;
    mem.shift = input.shift;
    mem.pressed = None;
    mem.activated = None;
    mem.nudge.clear();

    // Hover: the last-declared rectangle under the cursor, top layer first; a modal layer
    // with content hides the layers below it (the HUD is never modal).
    mem.hovered = None;
    mem.modal = None;
    for layer in UiLayer::ALL.into_iter().rev() {
        let state = &layers[layer.index()];
        if mem.hovered.is_none()
            && let Some(cursor) = input.cursor
            && let Some(hit) = state.hits.iter().rev().find(|h| h.rect.contains(cursor))
        {
            mem.hovered = Some((layer, hit.id));
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
    // order. A focused widget outside them (gone, disabled, clipped away, or under a modal
    // layer opened over it) loses the focus, so Enter cannot activate a widget the player no
    // longer reaches.
    let order: Vec<&Hit> = mem.modal.map_or_else(Vec::new, |layer| {
        layers[layer.index()]
            .hits
            .iter()
            .filter(|h| h.enabled && h.kind.focusable() && !h.rect.is_empty())
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

    // Keys move the focus along `order`.
    for &(key, repeat) in &input.keys {
        if order.is_empty() {
            break;
        }
        let at = mem
            .focused
            .and_then(|f| order.iter().position(|h| h.id == f));
        let adjustable = at.is_some_and(|i| order[i].kind.adjustable());
        let step = match key {
            NavKey::Left | NavKey::Right if adjustable => {
                let delta = if key == NavKey::Left { -1 } else { 1 };
                if let Some(i) = at {
                    let id = order[i].id;
                    match mem.nudge.iter_mut().find(|(nudged, _)| *nudged == id) {
                        Some((_, total)) => *total += delta,
                        None => mem.nudge.push((id, delta)),
                    }
                }
                0
            }
            NavKey::Next | NavKey::Down | NavKey::Right => 1,
            NavKey::Prev | NavKey::Up | NavKey::Left => -1,
            NavKey::Activate => {
                if !repeat && let Some(i) = at {
                    mem.activated = Some(order[i].id);
                }
                0
            }
        };
        if step != 0 {
            let len = order.len() as i64;
            let next = match at {
                None if step > 0 => 0,
                None => len - 1,
                Some(i) => (i as i64 + step).rem_euclid(len),
            };
            mem.focused = Some(order[next as usize].id);
            mem.focus_visible = true;
        }
    }

    InputCapture {
        pointer: mem.modal.is_some() || mem.hovered.is_some_and(|(l, _)| l != UiLayer::Hud),
        keyboard: mem.modal.is_some(),
        text: false,
    }
}
