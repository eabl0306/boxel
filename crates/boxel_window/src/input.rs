use boxel_app::{App, Plugin};
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
        todo!("WP2")
    }
    /// Registers a release (`just_released` only if it was held).
    pub fn release(&mut self, input: T) {
        todo!("WP2")
    }
    /// Currently held.
    pub fn pressed(&self, input: T) -> bool {
        todo!("WP2")
    }
    /// Pressed this frame.
    pub fn just_pressed(&self, input: T) -> bool {
        todo!("WP2")
    }
    /// Released this frame.
    pub fn just_released(&self, input: T) -> bool {
        todo!("WP2")
    }
    /// Any of `inputs` held.
    pub fn any_pressed(&self, inputs: impl IntoIterator<Item = T>) -> bool {
        todo!("WP2")
    }
    /// All held inputs.
    pub fn get_pressed(&self) -> impl Iterator<Item = &T> {
        self.pressed.iter()
    }
    /// Clears `just_pressed` and `just_released`.
    pub fn clear_just(&mut self) {
        todo!("WP2")
    }
    /// Releases everything (focus loss).
    pub fn reset_all(&mut self) {
        todo!("WP2")
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
    todo!("WP2")
}

/// `clear_just`, then apply [`MouseButtonInput`] messages.
pub fn mouse_button_input_system(
    mut buttons: ResMut<ButtonInput<MouseButton>>,
    mut events: MessageReader<MouseButtonInput>,
) {
    todo!("WP2")
}

/// Reset, then sum [`MouseMotion`] messages.
pub fn accumulate_mouse_motion_system(
    mut acc: ResMut<AccumulatedMouseMotion>,
    mut events: MessageReader<MouseMotion>,
) {
    todo!("WP2")
}

/// Reset, then sum [`MouseWheel`] messages.
pub fn accumulate_mouse_scroll_system(
    mut acc: ResMut<AccumulatedMouseScroll>,
    mut events: MessageReader<MouseWheel>,
) {
    todo!("WP2")
}

/// Registers the input messages and resources and adds the four systems to `PreUpdate`
/// in [`InputSystems`]. Independent of winit's event loop (testable headless).
#[derive(Default)]
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        todo!("WP2")
    }
}
