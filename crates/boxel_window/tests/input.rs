//! Headless input tests: `ButtonInput` semantics and `InputPlugin` driven by messages (no event
//! loop, no display).
use boxel_app::{App, PreUpdate};
use boxel_ecs::schedule::{LogLevel, ScheduleBuildSettings};
use boxel_math::Vec2;
use boxel_window::{
    AccumulatedMouseMotion, AccumulatedMouseScroll, ButtonInput, ButtonState, InputPlugin, KeyCode,
    KeyboardInput, MouseButton, MouseButtonInput, MouseMotion, MouseWheel, WindowFocused,
};
use std::collections::HashSet;

fn key(key: KeyCode, state: ButtonState) -> KeyboardInput {
    KeyboardInput {
        key,
        state,
        repeat: false,
    }
}

fn app() -> App {
    let mut app = App::new();
    app.add_plugins(InputPlugin);
    app
}

fn keys(app: &App) -> &ButtonInput<KeyCode> {
    app.world().resource::<ButtonInput<KeyCode>>()
}

fn buttons(app: &App) -> &ButtonInput<MouseButton> {
    app.world().resource::<ButtonInput<MouseButton>>()
}

#[test]
fn button_input_press_and_release() {
    let mut input = ButtonInput::<u8>::default();
    input.press(1);
    assert!(input.pressed(1) && input.just_pressed(1) && !input.just_released(1));

    // Pressing a held input again is not a new press.
    input.clear_just();
    input.press(1);
    assert!(input.pressed(1) && !input.just_pressed(1));

    input.release(1);
    assert!(!input.pressed(1) && input.just_released(1));

    // Releasing something that was not held reports nothing.
    input.clear_just();
    input.release(2);
    assert!(!input.just_released(2));

    // A tap inside one frame reports both edges.
    input.press(3);
    input.release(3);
    assert!(input.just_pressed(3) && input.just_released(3) && !input.pressed(3));

    input.clear_just();
    assert!(!input.just_pressed(3) && !input.just_released(3));
}

#[test]
fn button_input_queries_and_reset() {
    let mut input = ButtonInput::<u8>::default();
    input.press(1);
    input.press(2);
    input.clear_just();
    input.press(4);
    assert!(input.any_pressed([7, 2]));
    assert!(!input.any_pressed([7, 8]));
    assert!(!input.any_pressed([]));
    let held: HashSet<u8> = input.get_pressed().copied().collect();
    assert_eq!(held, HashSet::from([1, 2, 4]));

    input.reset_all();
    assert_eq!(input.get_pressed().count(), 0);
    for i in [1, 2, 4] {
        assert!(!input.pressed(i) && input.just_released(i));
    }
    // The press of this frame stays reported.
    assert!(input.just_pressed(4));
}

#[test]
fn keyboard_messages_drive_button_input() {
    let mut app = app();
    app.world_mut()
        .write_message(key(KeyCode::KeyW, ButtonState::Pressed));
    app.update();
    assert!(keys(&app).pressed(KeyCode::KeyW));
    assert!(keys(&app).just_pressed(KeyCode::KeyW));

    // The next frame clears the edge but keeps the key held.
    app.update();
    assert!(keys(&app).pressed(KeyCode::KeyW));
    assert!(!keys(&app).just_pressed(KeyCode::KeyW));

    app.world_mut()
        .write_message(key(KeyCode::KeyW, ButtonState::Released));
    app.update();
    assert!(!keys(&app).pressed(KeyCode::KeyW));
    assert!(keys(&app).just_released(KeyCode::KeyW));
    app.update();
    assert!(!keys(&app).just_released(KeyCode::KeyW));
}

#[test]
fn key_repeats_are_ignored() {
    let mut app = app();
    app.world_mut().write_message(KeyboardInput {
        key: KeyCode::Space,
        state: ButtonState::Pressed,
        repeat: true,
    });
    app.update();
    assert!(!keys(&app).pressed(KeyCode::Space));
    assert!(!keys(&app).just_pressed(KeyCode::Space));
}

#[test]
fn mouse_buttons_and_accumulators() {
    let mut app = app();
    let world = app.world_mut();
    world.write_message(MouseButtonInput {
        button: MouseButton::Left,
        state: ButtonState::Pressed,
    });
    world.write_message(MouseMotion {
        delta: Vec2::new(3.0, -1.0),
    });
    world.write_message(MouseMotion {
        delta: Vec2::new(2.0, 4.0),
    });
    world.write_message(MouseWheel {
        delta: Vec2::new(0.0, 1.5),
    });
    app.update();
    assert!(buttons(&app).just_pressed(MouseButton::Left));
    assert_eq!(
        app.world().resource::<AccumulatedMouseMotion>().delta,
        Vec2::new(5.0, 3.0)
    );
    assert_eq!(
        app.world().resource::<AccumulatedMouseScroll>().delta,
        Vec2::new(0.0, 1.5)
    );

    // Accumulators reset every frame; messages are read once.
    app.update();
    assert_eq!(
        app.world().resource::<AccumulatedMouseMotion>().delta,
        Vec2::ZERO
    );
    assert_eq!(
        app.world().resource::<AccumulatedMouseScroll>().delta,
        Vec2::ZERO
    );
    assert!(buttons(&app).pressed(MouseButton::Left));
    assert!(!buttons(&app).just_pressed(MouseButton::Left));
}

#[test]
fn focus_loss_releases_keys_and_buttons() {
    let mut app = app();
    let world = app.world_mut();
    world.write_message(key(KeyCode::KeyA, ButtonState::Pressed));
    world.write_message(MouseButtonInput {
        button: MouseButton::Right,
        state: ButtonState::Pressed,
    });
    app.update();
    assert!(keys(&app).pressed(KeyCode::KeyA));
    assert!(buttons(&app).pressed(MouseButton::Right));

    // Regaining focus releases nothing.
    app.world_mut().write_message(WindowFocused(true));
    app.update();
    assert!(keys(&app).pressed(KeyCode::KeyA));

    app.world_mut().write_message(WindowFocused(false));
    app.update();
    assert!(!keys(&app).pressed(KeyCode::KeyA));
    assert!(keys(&app).just_released(KeyCode::KeyA));
    assert!(!buttons(&app).pressed(MouseButton::Right));
    assert!(buttons(&app).just_released(MouseButton::Right));

    // The key-up that may follow (or never arrive) changes nothing.
    app.world_mut()
        .write_message(key(KeyCode::KeyA, ButtonState::Released));
    app.update();
    assert!(!keys(&app).pressed(KeyCode::KeyA));
    assert!(!keys(&app).just_released(KeyCode::KeyA));
}

#[test]
fn input_systems_are_not_ambiguous() {
    let mut app = app();
    app.edit_schedule(PreUpdate, |schedule| {
        schedule.set_build_settings(ScheduleBuildSettings {
            ambiguity_detection: LogLevel::Error,
            ..ScheduleBuildSettings::default()
        });
    });
    app.world_mut().write_message(WindowFocused(false));
    app.update();
}
