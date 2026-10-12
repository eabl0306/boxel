//! [`UiTester`]: a headless app with the UI, to click widgets by their label and press keys in
//! tests (the engine's and a game's).

use crate::atlas::UiAtlas;
use crate::memory::{Hit, UiState};
use crate::nav::InputCapture;
use crate::paint::UiDraw;
use crate::plugin::UiPlugin;
use crate::raster;
use crate::scale::GuiScale;
use boxel_app::{App, Update};
use boxel_ecs::prelude::*;
use boxel_ecs::system::ScheduleSystem;
use boxel_math::{IVec2, UVec2, Vec2};
use boxel_window::{
    ButtonState, CursorMoved, InputPlugin, KeyCode, KeyboardInput, MouseButton, MouseButtonInput,
    WindowState,
};

/// A headless app with `InputPlugin`, `UiPlugin` and a window state of a given size (no window,
/// no renderer). Add the systems that declare the UI with [`UiTester::add_systems`], then drive
/// frames with input.
///
/// Widgets answer one frame after they are declared (hover uses last frame's rectangles), so
/// run a [`UiTester::frame`] before the first click.
pub struct UiTester {
    /// The app (add more plugins and resources here).
    pub app: App,
}

impl UiTester {
    /// A tester with a window of `physical` pixels.
    pub fn new(physical: UVec2) -> Self {
        let mut app = App::new();
        app.add_plugins((InputPlugin, UiPlugin))
            .insert_resource(WindowState {
                physical_size: physical,
                scale_factor: 1.0,
                focused: true,
                ..WindowState::default()
            });
        Self { app }
    }

    /// Adds systems to `Update` (where screens declare their widgets).
    pub fn add_systems<M>(
        &mut self,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.app.add_systems(Update, systems);
        self
    }

    /// Runs one frame.
    pub fn frame(&mut self) -> &mut Self {
        self.app.update();
        self
    }

    /// Runs `n` frames.
    pub fn frames(&mut self, n: usize) -> &mut Self {
        for _ in 0..n {
            self.app.update();
        }
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

    /// A key event (seen at the next frame).
    pub fn key(&mut self, key: KeyCode, state: ButtonState, repeat: bool) -> &mut Self {
        self.app
            .world_mut()
            .write_message(KeyboardInput { key, state, repeat });
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

    /// The paint list rasterized on the CPU over `clear` (sRGB RGBA8, window-sized).
    pub fn raster(&self, clear: [u8; 4]) -> Vec<u8> {
        let draw = self.draw();
        let atlas = self.app.world().resource::<UiAtlas>();
        raster::rasterize(draw.quads(), atlas, draw.target_size(), clear)
    }
}
