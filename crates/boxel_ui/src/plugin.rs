//! [`UiPlugin`] and the two systems that frame a UI frame.

use crate::atlas::UiAtlas;
use crate::font::default_font;
use crate::memory::UiState;
use crate::nav::{InputCapture, NavInput, NavKey, resolve};
use crate::paint::{UiDraw, UiLayer, compose};
use crate::scale::GuiScale;
use crate::skin::{UiSkin, UiStyle};
use boxel_app::{App, Last, Plugin, PreUpdate};
use boxel_ecs::prelude::*;
use boxel_math::UVec2;
use boxel_window::{
    ButtonInput, ButtonState, CursorPosition, InputSystems, KeyCode, KeyboardInput, MouseButton,
    WindowState,
};

/// The UI's systems.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiSystems {
    /// `PreUpdate`, after `InputSystems`: the GUI scale, hover, clicks, focus and keys against
    /// last frame's rectangles, [`InputCapture`]. A game that changes screens does it before
    /// this set, so a click that closed a menu is not seen again.
    Begin,
    /// `Last`: the layers composed into [`UiDraw`] (only when something changed).
    End,
}

/// The engine's UI: GUI scale, a retained paint list ([`UiDraw`] with its [`UiAtlas`]),
/// sprites, bitmap text and widgets declared through [`crate::Ui`]. Not part of
/// `DefaultPlugins`: a game adds it with `InputPlugin`. Without a renderer that draws
/// [`UiDraw`] nothing shows; the paint list and the input work headless.
///
/// It inserts the procedural [`UiSkin`] and the default [`crate::BitmapFont`] in a fresh atlas;
/// replace them after adding the plugin.
#[derive(Default)]
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        let mut atlas = UiAtlas::default();
        let skin = UiSkin::procedural(&mut atlas)
            .unwrap_or_else(|err| unreachable!("the default atlas fits the skin: {err}"));
        let font = default_font(&mut atlas)
            .unwrap_or_else(|err| unreachable!("the default atlas fits the font: {err}"));
        app.insert_resource(atlas)
            .insert_resource(skin)
            .insert_resource(font)
            .init_resource::<UiStyle>()
            .init_resource::<GuiScale>()
            .init_resource::<UiState>()
            .init_resource::<InputCapture>()
            .init_resource::<UiDraw>()
            .configure_sets(PreUpdate, UiSystems::Begin.after(InputSystems))
            .add_systems(PreUpdate, ui_begin.in_set(UiSystems::Begin))
            .add_systems(Last, ui_end.in_set(UiSystems::End));
    }
}

/// Updates the scale, resolves this frame's input against last frame's rectangles and empties
/// the per-frame layers for the new declarations.
fn ui_begin(
    mut state: ResMut<UiState>,
    mut scale: ResMut<GuiScale>,
    mut capture: ResMut<InputCapture>,
    window: Option<Res<WindowState>>,
    cursor: Res<CursorPosition>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut keyboard: MessageReader<KeyboardInput>,
) {
    let physical = window.map_or(UVec2::ZERO, |w| w.physical_size);
    let computed = scale.computed(physical);
    if computed != *scale {
        *scale = computed;
        state.clear(UiLayer::Hud);
    }

    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let input = NavInput {
        cursor: cursor.0.map(|p| scale.to_gui(p)),
        press: mouse.just_pressed(MouseButton::Left),
        held: mouse.pressed(MouseButton::Left),
        shift,
        keys: keyboard
            .read()
            .filter(|e| e.state == ButtonState::Pressed)
            .filter_map(|e| NavKey::from_key(e.key, shift).map(|k| (k, e.repeat)))
            .collect(),
    };
    capture.set_if_neq(resolve(&mut state, &input));

    let state = &mut *state;
    state.frame += 1;
    for layer in UiLayer::ALL.into_iter().filter(|l| !l.retained()) {
        state.clear(layer);
    }
}

/// Composes the layers into [`UiDraw`] when something changed.
fn ui_end(mut state: ResMut<UiState>, scale: Res<GuiScale>, mut draw: ResMut<UiDraw>) {
    let target = scale.physical_size();
    if !state.dirty && draw.target_size() == target {
        return;
    }
    let quads = compose(
        state.layers.iter().map(|layer| layer.quads.as_slice()),
        target,
    );
    state.bypass_change_detection().dirty = false;
    if draw.bypass_change_detection().update(quads, target) {
        draw.set_changed();
    }
}
