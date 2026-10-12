//! [`UiTester`]: a headless app with the UI, to click widgets by their label and press keys in
//! tests (the engine's and a game's).

use crate::atlas::UiAtlas;
use crate::memory::{Hit, UiState};
use crate::nav::InputCapture;
use crate::paint::UiDraw;
use crate::plugin::UiPlugin;
use crate::raster;
use crate::scale::GuiScale;
use boxel_app::{App, FixedTime, Time, Update};
use boxel_ecs::prelude::*;
use boxel_ecs::system::ScheduleSystem;
use boxel_math::{IVec2, UVec2, Vec2};
use boxel_window::{
    ButtonState, CursorMoved, ImeEvent, ImeOptions, InputPlugin, KeyCode, KeyboardInput,
    MouseButton, MouseButtonInput, MouseWheel, TextInput, WindowState,
};
use std::time::{Duration, Instant};

/// A headless app with `InputPlugin`, `UiPlugin` and a window state of a given size (no window,
/// no renderer). Add the systems that declare the UI with [`UiTester::add_systems`], then drive
/// frames with input.
///
/// Widgets answer one frame after they are declared (hover uses last frame's rectangles), so
/// run a [`UiTester::frame`] before the first click.
///
/// Time is the tester's: it inserts a `Time` and moves it [`UiTester::FRAME_TIME`] each frame
/// (and [`UiTester::wait`] more), so double clicks and the caret's blink are exact. An app that
/// adds `TimePlugin` keeps the real clock instead. It also inserts `ImeOptions`, which the UI
/// turns on while a text field has the focus.
pub struct UiTester {
    /// The app (add more plugins and resources here).
    pub app: App,
    clock: Instant,
    shift: bool,
    ctrl: bool,
}

impl UiTester {
    /// The time between two frames of the tester's clock (60 frames a second).
    pub const FRAME_TIME: Duration = Duration::from_micros(16_667);

    /// A tester with a window of `physical` pixels.
    pub fn new(physical: UVec2) -> Self {
        let mut app = App::new();
        let clock = Instant::now();
        app.add_plugins((InputPlugin, UiPlugin))
            .insert_resource(WindowState {
                physical_size: physical,
                scale_factor: 1.0,
                focused: true,
                ..WindowState::default()
            })
            .insert_resource(Time::new(clock))
            .init_resource::<ImeOptions>();
        Self {
            app,
            clock,
            shift: false,
            ctrl: false,
        }
    }

    /// Adds systems to `Update` (where screens declare their widgets).
    pub fn add_systems<M>(
        &mut self,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.app.add_systems(Update, systems);
        self
    }

    /// Runs one frame, [`UiTester::FRAME_TIME`] after the previous one.
    pub fn frame(&mut self) -> &mut Self {
        self.clock += Self::FRAME_TIME;
        let world = self.app.world_mut();
        // `TimePlugin` (it brings `FixedTime`) moves the clock by itself.
        if !world.contains_resource::<FixedTime>()
            && let Some(mut time) = world.get_resource_mut::<Time>()
        {
            time.update_with_instant(self.clock);
        }
        self.app.update();
        self
    }

    /// Runs `n` frames.
    pub fn frames(&mut self, n: usize) -> &mut Self {
        for _ in 0..n {
            self.frame();
        }
        self
    }

    /// Lets `duration` of time pass before the next frame (on top of its own frame time).
    pub fn wait(&mut self, duration: Duration) -> &mut Self {
        self.clock += duration;
        self
    }

    /// Changes the window size (takes effect at the next frame).
    pub fn resize(&mut self, physical: UVec2) -> &mut Self {
        self.app
            .world_mut()
            .resource_mut::<WindowState>()
            .physical_size = physical;
        self
    }

    /// Moves the cursor to the center of the GUI pixel `gui` (seen at the next frame).
    pub fn move_to(&mut self, gui: IVec2) -> &mut Self {
        let scale = self.scale();
        let n = scale.scale() as f32;
        let position = scale.to_physical(gui).as_vec2() + Vec2::splat(n / 2.0);
        self.app.world_mut().write_message(CursorMoved { position });
        self
    }

    /// The left button goes down (seen at the next frame).
    pub fn press(&mut self) -> &mut Self {
        self.mouse(ButtonState::Pressed)
    }

    /// The left button goes up (seen at the next frame).
    pub fn release(&mut self) -> &mut Self {
        self.mouse(ButtonState::Released)
    }

    fn mouse(&mut self, state: ButtonState) -> &mut Self {
        self.app.world_mut().write_message(MouseButtonInput {
            button: MouseButton::Left,
            state,
        });
        self
    }

    /// Moves to the center of the widget labeled `label` (of the part its clip shows), presses
    /// and runs a frame (the widget sees the click in it), then releases and runs another.
    ///
    /// # Panics
    /// If no widget had that label in the latest frame.
    pub fn click(&mut self, label: &str) -> &mut Self {
        let rect = self
            .hit(label)
            .unwrap_or_else(|| panic!("no widget labeled {label:?}; have {:?}", self.labels()))
            .rect;
        self.move_to(IVec2::new(rect.x + rect.w / 2, rect.y + rect.h / 2))
            .press()
            .frame()
            .release()
            .frame()
    }

    /// Turns the wheel `lines` notches (positive up; seen at the next frame).
    pub fn scroll(&mut self, lines: f32) -> &mut Self {
        self.app.world_mut().write_message(MouseWheel {
            delta: Vec2::new(0.0, lines),
        });
        self
    }

    /// A key event (seen at the next frame), with what the runner sends a text field: a press
    /// also writes the `TextInput` it types on a US layout (letters, digits, space and the
    /// punctuation keys) or a `TextInput::Key` (every other key, and every key with Ctrl held).
    pub fn key(&mut self, key: KeyCode, state: ButtonState, repeat: bool) -> &mut Self {
        let down = state == ButtonState::Pressed;
        match key {
            KeyCode::ShiftLeft | KeyCode::ShiftRight => self.shift = down,
            KeyCode::ControlLeft | KeyCode::ControlRight => self.ctrl = down,
            _ => {}
        }
        let world = self.app.world_mut();
        world.write_message(KeyboardInput { key, state, repeat });
        if down {
            let typed = (!self.ctrl).then(|| us_layout(key, self.shift)).flatten();
            world.write_message(match typed {
                Some(c) => TextInput::Text(c.to_string()),
                None => TextInput::Key { key, repeat },
            });
        }
        self
    }

    /// Types `text` at once, as an input method commit or a fast typist within one frame
    /// (`TextInput::Text` only), and runs a frame.
    pub fn type_text(&mut self, text: &str) -> &mut Self {
        self.app
            .world_mut()
            .write_message(TextInput::Text(text.to_owned()));
        self.frame()
    }

    /// An input method composition (seen at the next frame; empty ends it).
    pub fn preedit(&mut self, text: &str) -> &mut Self {
        self.app.world_mut().write_message(ImeEvent::Preedit {
            text: text.to_owned(),
            cursor: None,
        });
        self
    }

    /// Presses `key`, runs a frame, releases it and runs another.
    pub fn tap(&mut self, key: KeyCode) -> &mut Self {
        self.key(key, ButtonState::Pressed, false)
            .frame()
            .key(key, ButtonState::Released, false)
            .frame()
    }

    /// The widget labeled `label` in the latest frame, top layer first.
    pub fn hit(&self, label: &str) -> Option<Hit> {
        self.state().hit_by_label(label).cloned()
    }

    /// The labels of every widget in the latest frame, bottom layer first.
    pub fn labels(&self) -> Vec<String> {
        let state = self.state();
        crate::UiLayer::ALL
            .iter()
            .flat_map(|&l| state.hits(l).iter().map(|h| h.label.clone()))
            .collect()
    }

    /// The label of the focused widget.
    pub fn focused(&self) -> Option<String> {
        let state = self.state();
        state
            .focused()
            .and_then(|id| state.hit(id))
            .map(|h| h.label.clone())
    }

    /// The UI state.
    pub fn state(&self) -> &UiState {
        self.app.world().resource::<UiState>()
    }

    /// The GUI scale.
    pub fn scale(&self) -> GuiScale {
        *self.app.world().resource::<GuiScale>()
    }

    /// What the UI captured this frame.
    pub fn capture(&self) -> InputCapture {
        *self.app.world().resource::<InputCapture>()
    }

    /// The composed paint list.
    pub fn draw(&self) -> &UiDraw {
        self.app.world().resource::<UiDraw>()
    }

    /// What the UI asks of the input method.
    pub fn ime(&self) -> ImeOptions {
        *self.app.world().resource::<ImeOptions>()
    }

    /// The paint list rasterized on the CPU over `clear` (sRGB RGBA8, window-sized).
    pub fn raster(&self, clear: [u8; 4]) -> Vec<u8> {
        let draw = self.draw();
        let atlas = self.app.world().resource::<UiAtlas>();
        raster::rasterize(draw.quads(), atlas, draw.target_size(), clear)
    }
}

/// What a key types on a US layout, if it types.
fn us_layout(key: KeyCode, shift: bool) -> Option<char> {
    use KeyCode as K;
    let letters = [
        (K::KeyA, 'a'),
        (K::KeyB, 'b'),
        (K::KeyC, 'c'),
        (K::KeyD, 'd'),
        (K::KeyE, 'e'),
        (K::KeyF, 'f'),
        (K::KeyG, 'g'),
        (K::KeyH, 'h'),
        (K::KeyI, 'i'),
        (K::KeyJ, 'j'),
        (K::KeyK, 'k'),
        (K::KeyL, 'l'),
        (K::KeyM, 'm'),
        (K::KeyN, 'n'),
        (K::KeyO, 'o'),
        (K::KeyP, 'p'),
        (K::KeyQ, 'q'),
        (K::KeyR, 'r'),
        (K::KeyS, 's'),
        (K::KeyT, 't'),
        (K::KeyU, 'u'),
        (K::KeyV, 'v'),
        (K::KeyW, 'w'),
        (K::KeyX, 'x'),
        (K::KeyY, 'y'),
        (K::KeyZ, 'z'),
    ];
    if let Some(&(_, c)) = letters.iter().find(|(k, _)| *k == key) {
        return Some(if shift { c.to_ascii_uppercase() } else { c });
    }
    // (key, plain, with Shift)
    let others = [
        (K::Digit0, '0', ')'),
        (K::Digit1, '1', '!'),
        (K::Digit2, '2', '@'),
        (K::Digit3, '3', '#'),
        (K::Digit4, '4', '$'),
        (K::Digit5, '5', '%'),
        (K::Digit6, '6', '^'),
        (K::Digit7, '7', '&'),
        (K::Digit8, '8', '*'),
        (K::Digit9, '9', '('),
        (K::Space, ' ', ' '),
        (K::Minus, '-', '_'),
        (K::Equal, '=', '+'),
        (K::BracketLeft, '[', '{'),
        (K::BracketRight, ']', '}'),
        (K::Backslash, '\\', '|'),
        (K::Semicolon, ';', ':'),
        (K::Quote, '\'', '"'),
        (K::Backquote, '`', '~'),
        (K::Comma, ',', '<'),
        (K::Period, '.', '>'),
        (K::Slash, '/', '?'),
    ];
    others
        .iter()
        .find(|(k, ..)| *k == key)
        .map(|&(_, plain, shifted)| if shift { shifted } else { plain })
}
