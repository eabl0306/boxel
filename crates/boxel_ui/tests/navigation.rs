//! Mouse and keyboard through `UiPlugin`, headless (`InputPlugin`, a `WindowState`, no window):
//! hover, presses, Tab order, Enter and Space, arrows on sliders and cycles, Shift+click,
//! disabled widgets, key repeat, a popup over a screen, clipped widgets and `InputCapture`.

use boxel_ecs::prelude::*;
use boxel_math::{IVec2, UVec2, Vec2};
use boxel_ui::testing::UiTester;
use boxel_ui::{Column, InputCapture, Modal, Steps, Text, Ui, UiLayer, UiRect};
use boxel_window::{ButtonState, CursorMoved, CursorPosition, KeyCode};

#[derive(Resource)]
struct Menu {
    open: bool,
    modal: Modal,
    clicks: Vec<&'static str>,
    mode: u8,
    fov: i32,
    flag: bool,
}

impl Default for Menu {
    fn default() -> Self {
        Self {
            open: true,
            modal: Modal::Yes,
            clicks: Vec::new(),
            mode: 0,
            fov: 70,
            flag: false,
        }
    }
}

fn screen(mut ui: Ui, mut menu: ResMut<Menu>) {
    if !menu.open {
        return;
    }
    let menu = &mut *menu;
    let mut p = ui.layer(UiLayer::Screen, menu.modal);
    let mut col = Column::new(10, 10, 200, 24);
    if p.button(col.next(), "test:play").clicked {
        menu.clicks.push("play");
    }
    if p.enabled(false).button(col.next(), "test:disabled").clicked {
        menu.clicks.push("disabled");
    }
    p.cycle(
        col.next(),
        "test:mode",
        &mut menu.mode,
        &[(0, "a"), (1, "b"), (2, "c")],
    );
    p.slider(
        col.next(),
        "test:fov",
        &mut menu.fov,
        Steps::Range {
            min: 30,
            max: 110,
            step: 5,
        },
        |v| v.to_string(),
    );
    p.toggle(col.next(), "test:flag", &mut menu.flag);
}

fn tester() -> UiTester {
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.app.init_resource::<Menu>();
    t.add_systems(screen);
    t.frame();
    t
}

fn menu(t: &UiTester) -> &Menu {
    t.app.world().resource::<Menu>()
}

fn menu_mut(t: &mut UiTester) -> Mut<'_, Menu> {
    t.app.world_mut().resource_mut::<Menu>()
}

fn hold_shift(t: &mut UiTester, down: bool) {
    let state = if down {
        ButtonState::Pressed
    } else {
        ButtonState::Released
    };
    t.key(KeyCode::ShiftLeft, state, false).frame();
}

#[test]
fn tab_and_shift_tab_follow_declaration_order_and_skip_disabled() {
    let mut t = tester();
    assert_eq!(t.focused(), None);
    let mut order = Vec::new();
    for _ in 0..5 {
        t.tap(KeyCode::Tab);
        order.push(t.focused().unwrap());
    }
    assert_eq!(
        order,
        [
            "test:play",
            "test:mode",
            "test:fov",
            "test:flag",
            "test:play"
        ]
    );
    hold_shift(&mut t, true);
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:flag"));
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:fov"));
    hold_shift(&mut t, false);
    // Down and Up move like Tab and Shift+Tab.
    t.tap(KeyCode::ArrowDown);
    assert_eq!(t.focused().as_deref(), Some("test:flag"));
    t.tap(KeyCode::ArrowUp);
    assert_eq!(t.focused().as_deref(), Some("test:fov"));
}

#[test]
fn enter_and_space_activate_the_focused_widget_but_not_on_repeat() {
    let mut t = tester();
    t.tap(KeyCode::Tab).tap(KeyCode::Enter);
    assert_eq!(menu(&t).clicks, ["play"]);
    t.tap(KeyCode::Space);
    assert_eq!(menu(&t).clicks, ["play", "play"]);
    // A held key's OS repeats do not activate again.
    t.key(KeyCode::Enter, ButtonState::Pressed, false)
        .frame()
        .key(KeyCode::Enter, ButtonState::Pressed, true)
        .key(KeyCode::Enter, ButtonState::Pressed, true)
        .frame()
        .key(KeyCode::Enter, ButtonState::Released, false)
        .frame();
    assert_eq!(menu(&t).clicks, ["play", "play", "play"]);
    // Enter on a toggle flips it.
    t.tap(KeyCode::Tab).tap(KeyCode::Tab).tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:flag"));
    t.tap(KeyCode::Enter);
    assert!(menu(&t).flag);
}

#[test]
fn a_press_activates_before_the_release() {
    let mut t = tester();
    let rect = t.hit("test:play").unwrap().rect;
    t.move_to(IVec2::new(rect.x + 3, rect.y + 3))
        .press()
        .frame();
    assert_eq!(menu(&t).clicks, ["play"]);
    // Holding does not click again; neither does the release.
    t.frame().release().frame();
    assert_eq!(menu(&t).clicks, ["play"]);
    // The click focused it, without making the focus visible to tooltips.
    assert_eq!(t.focused().as_deref(), Some("test:play"));
    t.click("test:play");
    assert_eq!(menu(&t).clicks, ["play", "play"]);
}

#[test]
fn disabled_widgets_ignore_clicks_and_focus_but_get_hover() {
    let mut t = tester();
    let hit = t.hit("test:disabled").unwrap();
    assert!(!hit.enabled);
    t.click("test:disabled");
    assert!(menu(&t).clicks.is_empty());
    assert_eq!(t.focused(), None);
    assert_eq!(t.state().hovered(), Some(hit.id));
}

#[test]
fn arrows_step_sliders_and_cycles_and_key_repeat_counts() {
    let mut t = tester();
    t.tap(KeyCode::Tab).tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:mode"));
    t.tap(KeyCode::ArrowRight);
    assert_eq!(menu(&t).mode, 1);
    t.tap(KeyCode::ArrowLeft).tap(KeyCode::ArrowLeft);
    assert_eq!(menu(&t).mode, 2, "Left wraps to the last value");

    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:fov"));
    t.tap(KeyCode::ArrowRight);
    assert_eq!(menu(&t).fov, 75);
    t.tap(KeyCode::ArrowLeft).tap(KeyCode::ArrowLeft);
    assert_eq!(menu(&t).fov, 65);
    // The OS repeats a held arrow: every repeat is one more step.
    t.key(KeyCode::ArrowRight, ButtonState::Pressed, false)
        .key(KeyCode::ArrowRight, ButtonState::Pressed, true)
        .key(KeyCode::ArrowRight, ButtonState::Pressed, true)
        .frame()
        .key(KeyCode::ArrowRight, ButtonState::Released, false)
        .frame();
    assert_eq!(menu(&t).fov, 80);
    // Clamped at the ends.
    for _ in 0..30 {
        t.tap(KeyCode::ArrowRight);
    }
    assert_eq!(menu(&t).fov, 110);
}

#[test]
fn shift_click_steps_a_cycle_back() {
    let mut t = tester();
    t.click("test:mode");
    assert_eq!(menu(&t).mode, 1);
    hold_shift(&mut t, true);
    t.click("test:mode");
    assert_eq!(menu(&t).mode, 0);
    t.click("test:mode");
    assert_eq!(menu(&t).mode, 2);
    hold_shift(&mut t, false);
    t.click("test:mode");
    assert_eq!(menu(&t).mode, 0);
}

#[test]
fn slider_follows_a_press_and_a_drag() {
    let mut t = tester();
    let rect = t.hit("test:fov").unwrap().rect;
    t.move_to(IVec2::new(rect.x, rect.y + 5)).press().frame();
    assert_eq!(menu(&t).fov, 30);
    t.move_to(IVec2::new(rect.right() - 1, rect.y + 5)).frame();
    assert_eq!(menu(&t).fov, 110);
    // The middle of the track is the middle value.
    t.move_to(IVec2::new(rect.x + rect.w / 2, rect.y + 5))
        .frame();
    assert_eq!(menu(&t).fov, 70);
    // Dragging outside the widget keeps following until the release.
    t.move_to(IVec2::new(rect.x - 50, rect.y + 100)).frame();
    assert_eq!(menu(&t).fov, 30);
    t.release().frame();
    t.move_to(IVec2::new(rect.right() - 1, rect.y + 5)).frame();
    assert_eq!(menu(&t).fov, 30);
}

#[test]
fn hover_comes_from_cursor_position_and_last_frame_rectangles() {
    let mut t = tester();
    let play = t.hit("test:play").unwrap();
    // 1280x720 is scale 3 with a 1-pixel margin on the left: physical (46, 46) is GUI (15, 15).
    t.app.world_mut().write_message(CursorMoved {
        position: Vec2::new(46.0, 46.0),
    });
    t.frame();
    assert_eq!(
        t.app.world().resource::<CursorPosition>().0,
        Some(Vec2::new(46.0, 46.0))
    );
    assert_eq!(t.state().hovered(), Some(play.id));
    t.move_to(IVec2::new(300, 200)).frame();
    assert_eq!(t.state().hovered(), None);
}

#[test]
fn a_modal_layer_captures_input_and_the_hud_alone_does_not() {
    #[derive(Resource, Default)]
    struct Hud(u32);
    fn hud(mut ui: Ui, mut hud: ResMut<Hud>) {
        let mut p = ui.layer(UiLayer::Hud, Modal::Yes);
        if p.button(UiRect::new(300, 200, 40, 20), "test:hud").clicked {
            hud.0 += 1;
        }
    }
    let mut t = tester();
    t.app.init_resource::<Hud>();
    t.add_systems(hud);
    t.frame();
    assert_eq!(
        t.capture(),
        InputCapture {
            pointer: true,
            keyboard: true,
            text: false
        }
    );

    // Only the HUD (its modal flag is ignored): nothing captured, even over its widget, and it
    // still gets the click.
    menu_mut(&mut t).open = false;
    t.frame().frame();
    assert_eq!(t.capture(), InputCapture::default());
    t.move_to(IVec2::new(310, 210)).frame();
    assert_eq!(t.capture(), InputCapture::default());
    t.click("test:hud");
    assert_eq!(t.app.world().resource::<Hud>().0, 1);

    // A non-modal screen takes the pointer only over its widgets, and never the keyboard.
    menu_mut(&mut t).open = true;
    menu_mut(&mut t).modal = Modal::No;
    t.frame().frame();
    assert_eq!(t.capture(), InputCapture::default());
    let play = t.hit("test:play").unwrap().rect;
    t.move_to(IVec2::new(play.x + 1, play.y + 1)).frame();
    assert_eq!(
        t.capture(),
        InputCapture {
            pointer: true,
            keyboard: false,
            text: false
        }
    );
    t.tap(KeyCode::Tab);
    assert_eq!(
        t.focused(),
        None,
        "no keyboard navigation without a modal layer"
    );
}

#[test]
fn a_modal_popup_takes_the_focus_from_the_screen_under_it() {
    #[derive(Resource, Default)]
    struct Confirm(Vec<&'static str>);
    // Clicking "play" opens a confirmation over the screen, which stays drawn under it.
    fn confirm(mut ui: Ui, menu: Res<Menu>, mut confirm: ResMut<Confirm>) {
        if !menu.clicks.contains(&"play") {
            return;
        }
        let mut p = ui.layer(UiLayer::Popup, Modal::Yes);
        let [yes, no] = UiRect::new(300, 100, 200, 20).split(4);
        if p.button(yes, "test:yes").clicked {
            confirm.0.push("yes");
        }
        if p.button(no, "test:no").clicked {
            confirm.0.push("no");
        }
    }
    let mut t = tester();
    t.app.init_resource::<Confirm>();
    t.add_systems(confirm.after(screen));
    t.tap(KeyCode::Tab).tap(KeyCode::Enter);
    assert_eq!(menu(&t).clicks, ["play"]);
    assert!(t.hit("test:yes").is_some(), "the confirmation is open");
    assert_eq!(
        t.focused(),
        None,
        "the screen under the popup lost the focus"
    );

    // Enter and Space activate nothing until a popup widget has the focus; they never reach
    // the screen.
    t.tap(KeyCode::Enter).tap(KeyCode::Space);
    assert_eq!(menu(&t).clicks, ["play"]);
    assert!(t.app.world().resource::<Confirm>().0.is_empty());
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:yes"));
    t.tap(KeyCode::Tab).tap(KeyCode::Enter);
    assert_eq!(t.app.world().resource::<Confirm>().0, ["no"]);
    assert_eq!(menu(&t).clicks, ["play"]);
}

#[test]
fn arrow_steps_stay_with_the_widget_they_were_pressed_on() {
    let mut t = tester();
    t.tap(KeyCode::Tab).tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:mode"));
    // A slow frame sees Right on the cycle, Tab to the slider and Right again.
    t.key(KeyCode::ArrowRight, ButtonState::Pressed, false)
        .key(KeyCode::Tab, ButtonState::Pressed, false)
        .key(KeyCode::ArrowRight, ButtonState::Pressed, false)
        .frame();
    assert_eq!(t.focused().as_deref(), Some("test:fov"));
    assert_eq!((menu(&t).mode, menu(&t).fov), (1, 75));
    // Left, Tab to the toggle and Left: each moves its own step back.
    t.key(KeyCode::ArrowLeft, ButtonState::Pressed, false)
        .key(KeyCode::Tab, ButtonState::Pressed, false)
        .key(KeyCode::ArrowLeft, ButtonState::Pressed, false)
        .frame();
    assert_eq!(t.focused().as_deref(), Some("test:flag"));
    assert_eq!((menu(&t).fov, menu(&t).flag), (70, true));
}

#[test]
fn widgets_answer_input_only_where_their_clip_shows_them() {
    #[derive(Resource, Default)]
    struct Clipped {
        clicks: Vec<&'static str>,
        label_hovered: bool,
    }
    fn clipped(mut ui: Ui, mut out: ResMut<Clipped>) {
        let out = &mut *out;
        let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
        if p.button(UiRect::new(10, 40, 200, 20), "test:done").clicked {
            out.clicks.push("done");
        }
        // A scrolled list: rows above and below its area are cut away.
        p.with_clip(UiRect::new(10, 0, 200, 50), |p| {
            if p.button(UiRect::new(10, 45, 200, 20), "test:row").clicked {
                out.clicks.push("row");
            }
            if p.button(UiRect::new(10, 100, 200, 20), "test:hidden")
                .clicked
            {
                out.clicks.push("hidden");
            }
            let label = p.label(UiRect::new(10, 120, 200, 20), "test:label", Text::default());
            out.label_hovered = label.hovered;
        });
    }
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.app.init_resource::<Clipped>();
    t.add_systems(clipped);
    t.frame();
    let clicks = |t: &UiTester| t.app.world().resource::<Clipped>().clicks.clone();

    // The rectangles are cut to what shows.
    assert_eq!(t.hit("test:row").unwrap().rect, UiRect::new(10, 45, 200, 5));
    assert!(t.hit("test:hidden").unwrap().rect.is_empty());
    // Under the row's hidden part, the button that shows takes the click.
    t.move_to(IVec2::new(50, 55))
        .press()
        .frame()
        .release()
        .frame();
    assert_eq!(clicks(&t), ["done"]);
    // Where the row shows over it, the row (declared later) takes it.
    t.move_to(IVec2::new(50, 47))
        .press()
        .frame()
        .release()
        .frame();
    assert_eq!(clicks(&t), ["done", "row"]);
    // A row clipped away entirely gets no hover, click or focus; neither does a label.
    t.move_to(IVec2::new(50, 110))
        .press()
        .frame()
        .release()
        .frame();
    assert_eq!(t.state().hovered(), None);
    assert_eq!(clicks(&t), ["done", "row"]);
    t.move_to(IVec2::new(50, 125)).frame();
    assert!(!t.app.world().resource::<Clipped>().label_hovered);
    let mut order = Vec::new();
    for _ in 0..3 {
        t.tap(KeyCode::Tab);
        order.push(t.focused().unwrap());
    }
    assert_eq!(order, ["test:done", "test:row", "test:done"]);
}

#[test]
fn a_modal_screen_hides_the_hud_from_the_pointer() {
    #[derive(Resource, Default)]
    struct Hud(u32);
    fn hud(mut ui: Ui, mut hud: ResMut<Hud>) {
        let mut p = ui.layer(UiLayer::Hud, Modal::No);
        if p.button(UiRect::new(300, 200, 40, 20), "test:under")
            .clicked
        {
            hud.0 += 1;
        }
    }
    let mut t = tester();
    t.app.init_resource::<Hud>();
    t.add_systems(hud);
    t.frame();
    // Away from the screen's widgets, the modal screen still hides the HUD's button.
    t.move_to(IVec2::new(310, 210))
        .press()
        .frame()
        .release()
        .frame();
    assert_eq!(t.state().hovered(), None);
    assert_eq!(t.app.world().resource::<Hud>().0, 0);
    assert!(menu(&t).clicks.is_empty());
    menu_mut(&mut t).open = false;
    t.frame().frame().press().frame().release().frame();
    assert_eq!(t.app.world().resource::<Hud>().0, 1);
}
