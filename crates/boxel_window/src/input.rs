use crate::window::WindowFocused;
use boxel_app::{App, Plugin, PreUpdate};
use boxel_ecs::prelude::*;
use boxel_math::Vec2;
use std::collections::HashSet;
use std::hash::Hash;
use winit::event::MouseButton;
use winit::keyboard::KeyCode;

/// Press state of a button/key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ButtonState {
    /// Went down.
    Pressed,
    /// Went up.
    Released,
}

/// Keyboard event (physical key).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyboardInput {
    /// Physical key.
    pub key: KeyCode,
    /// Down or up.
    pub state: ButtonState,
    /// OS key repeat.
    pub repeat: bool,
}

/// Mouse button event.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseButtonInput {
    /// Button.
    pub button: MouseButton,
    /// Down or up.
    pub state: ButtonState,
}

/// Raw mouse motion (device event, unaccelerated), only while the window has focus.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct MouseMotion {
    /// Delta in device units.
    pub delta: Vec2,
}

/// Scroll (lines; pixel deltas are divided by 32).
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct MouseWheel {
    /// Delta in lines.
    pub delta: Vec2,
}

/// Cursor position in physical pixels from the window's top-left.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct CursorMoved {
    /// Position.
    pub position: Vec2,
}

/// Where the cursor is: the position of the latest [`CursorMoved`] message, in physical pixels
/// from the window's top-left. `None` until the cursor first moves over the window. UIs read it
/// for hover and clicks; it keeps its last value while the cursor is locked (`CursorGrab::Locked`
/// sends `MouseMotion`, not `CursorMoved`) or outside the window.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct CursorPosition(pub Option<Vec2>);

/// Pressed/just-pressed/just-released sets. `just_*` are cleared by the input systems at the
/// start of each frame's `PreUpdate` before new events are applied.
#[derive(Resource, Debug, Clone)]
pub struct ButtonInput<T: Copy + Eq + Hash + Send + Sync + 'static> {
    pressed: HashSet<T>,
    just_pressed: HashSet<T>,
    just_released: HashSet<T>,
}

impl<T: Copy + Eq + Hash + Send + Sync + 'static> Default for ButtonInput<T> {
    fn default() -> Self {
        Self {
            pressed: HashSet::new(),
            just_pressed: HashSet::new(),
            just_released: HashSet::new(),
        }
    }
}

impl<T: Copy + Eq + Hash + Send + Sync + 'static> ButtonInput<T> {
    /// Registers a press (`just_pressed` only if it was not already held).
    pub fn press(&mut self, input: T) {
        if self.pressed.insert(input) {
            self.just_pressed.insert(input);
        }
    }
    /// Registers a release (`just_released` only if it was held).
    pub fn release(&mut self, input: T) {
        if self.pressed.remove(&input) {
            self.just_released.insert(input);
        }
    }
    /// Currently held.
    pub fn pressed(&self, input: T) -> bool {
        self.pressed.contains(&input)
    }
    /// Pressed this frame.
    pub fn just_pressed(&self, input: T) -> bool {
        self.just_pressed.contains(&input)
    }
    /// Released this frame.
    pub fn just_released(&self, input: T) -> bool {
        self.just_released.contains(&input)
    }
    /// Any of `inputs` held.
    pub fn any_pressed(&self, inputs: impl IntoIterator<Item = T>) -> bool {
        inputs.into_iter().any(|input| self.pressed(input))
    }
    /// All held inputs.
    pub fn get_pressed(&self) -> impl Iterator<Item = &T> {
        self.pressed.iter()
    }
    /// Clears `just_pressed` and `just_released`.
    pub fn clear_just(&mut self) {
        self.just_pressed.clear();
        self.just_released.clear();
    }
    /// Releases everything (focus loss): every held input moves to `just_released`.
    /// `just_pressed` is kept (an input pressed and released in the same frame reports both).
    pub fn reset_all(&mut self) {
        self.just_released.extend(self.pressed.drain());
    }
}

/// Sum of [`MouseMotion`] deltas this frame.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct AccumulatedMouseMotion {
    /// Delta.
    pub delta: Vec2,
}

/// Sum of [`MouseWheel`] deltas this frame.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct AccumulatedMouseScroll {
    /// Delta.
    pub delta: Vec2,
}

/// Input systems (in `PreUpdate`).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct InputSystems;

/// `clear_just`, then apply [`KeyboardInput`] messages (repeats ignored).
pub fn keyboard_input_system(
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut events: MessageReader<KeyboardInput>,
) {
    keys.clear_just();
    for event in events.read().filter(|event| !event.repeat) {
        match event.state {
            ButtonState::Pressed => keys.press(event.key),
            ButtonState::Released => keys.release(event.key),
        }
    }
}

/// `clear_just`, then apply [`MouseButtonInput`] messages.
pub fn mouse_button_input_system(
    mut buttons: ResMut<ButtonInput<MouseButton>>,
    mut events: MessageReader<MouseButtonInput>,
) {
    buttons.clear_just();
    for event in events.read() {
        match event.state {
            ButtonState::Pressed => buttons.press(event.button),
            ButtonState::Released => buttons.release(event.button),
        }
    }
}

/// Reset, then sum [`MouseMotion`] messages.
pub fn accumulate_mouse_motion_system(
    mut acc: ResMut<AccumulatedMouseMotion>,
    mut events: MessageReader<MouseMotion>,
) {
    let delta = events.read().map(|event| event.delta).sum();
    acc.set_if_neq(AccumulatedMouseMotion { delta });
}

/// Reset, then sum [`MouseWheel`] messages.
pub fn accumulate_mouse_scroll_system(
    mut acc: ResMut<AccumulatedMouseScroll>,
    mut events: MessageReader<MouseWheel>,
) {
    let delta = events.read().map(|event| event.delta).sum();
    acc.set_if_neq(AccumulatedMouseScroll { delta });
}

/// Keeps [`CursorPosition`] at the latest [`CursorMoved`] message of the frame.
pub fn cursor_position_system(
    mut cursor: ResMut<CursorPosition>,
    mut events: MessageReader<CursorMoved>,
) {
    if let Some(last) = events.read().last() {
        cursor.set_if_neq(CursorPosition(Some(last.position)));
    }
}

/// Releases every key and mouse button when the window loses focus (the matching key-up
/// events go to whichever window has focus now). Runs after the button systems so the
/// releases show up as `just_released` this frame.
fn release_on_focus_loss_system(
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut buttons: ResMut<ButtonInput<MouseButton>>,
    mut events: MessageReader<WindowFocused>,
) {
    // Count every message (not `any`) so the reader's cursor consumes all of them.
    let lost = events.read().filter(|focus| !focus.0).count();
    if lost > 0 {
        keys.reset_all();
        buttons.reset_all();
    }
}

/// Registers the input messages and resources and adds the input systems to `PreUpdate`
/// in [`InputSystems`]. Independent of winit's event loop (testable headless).
#[derive(Default)]
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<KeyboardInput>()
            .add_message::<MouseButtonInput>()
            .add_message::<MouseMotion>()
            .add_message::<MouseWheel>()
            .add_message::<CursorMoved>()
            .add_message::<WindowFocused>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<AccumulatedMouseScroll>()
            .init_resource::<CursorPosition>()
            .add_systems(
                PreUpdate,
                (
                    (
                        (keyboard_input_system, mouse_button_input_system),
                        release_on_focus_loss_system,
                    )
                        .chain(),
                    accumulate_mouse_motion_system,
                    accumulate_mouse_scroll_system,
                    cursor_position_system,
                )
                    .in_set(InputSystems),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_position_follows_the_latest_cursor_moved() {
        let mut app = App::new();
        app.add_plugins(InputPlugin);
        app.update();
        assert_eq!(app.world().resource::<CursorPosition>().0, None);

        for x in [10.0, 20.5] {
            app.world_mut().write_message(CursorMoved {
                position: Vec2::new(x, 7.0),
            });
        }
        app.update();
        assert_eq!(
            app.world().resource::<CursorPosition>().0,
            Some(Vec2::new(20.5, 7.0))
        );

        // A frame without movement keeps the last position.
        app.update();
        assert_eq!(
            app.world().resource::<CursorPosition>().0,
            Some(Vec2::new(20.5, 7.0))
        );
    }
}
