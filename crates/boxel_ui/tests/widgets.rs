//! The list, the text field and the tabs through `UiPlugin`, headless (`InputPlugin`, a
//! `WindowState`, the tester's clock): the wheel, the scrollbar, selection and double clicks,
//! row widgets and the keyboard, typed text in order with the keys, the caret, selection and
//! the input method, tabs with the arrows and Ctrl+Tab, and a golden raster of all three.

use boxel_ecs::prelude::*;
use boxel_math::{IVec2, UVec2};
use boxel_ui::color::{argb, rgb};
use boxel_ui::raster::{fingerprint, paint_runs, rasterize, runs};
use boxel_ui::testing::UiTester;
use boxel_ui::text::measure;
use boxel_ui::{
    BitmapFont, DOUBLE_CLICK, InputCapture, Modal, SCROLLBAR_WIDTH, Text, Ui, UiAtlas, UiLayer,
    UiQuad, UiRect,
};
use boxel_window::{ButtonInput, ButtonState, ImeOptions, KeyCode, KeyboardInput, TextInput};
use std::time::Duration;

// ---- the list

/// 200 by 100 GUI pixels at (20, 20): five 20-pixel rows show.
const LIST: UiRect = UiRect::new(20, 20, 200, 100);

#[derive(Resource, Default)]
struct Worlds {
    rows: usize,
    selected: Option<usize>,
    /// Each row drawn this frame and its top.
    drawn: Vec<(usize, i32)>,
    clicked: u32,
    changed: u32,
    opened: Vec<Option<usize>>,
    /// Rows get a "test:delete" button.
    buttons: bool,
    deleted: Vec<usize>,
    /// A modal popup over the list.
    popup: bool,
}

fn worlds(mut ui: Ui, mut worlds: ResMut<Worlds>) {
    let w = &mut *worlds;
    let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
    w.drawn.clear();
    let (drawn, deleted, buttons) = (&mut w.drawn, &mut w.deleted, w.buttons);
    let r = p
        .list(
            LIST,
            "test:worlds",
            20,
            w.rows,
            &mut w.selected,
            |p, row| {
                drawn.push((row.index, row.rect.y));
                let name = format!("test:{}", row.index);
                p.text(row.rect.x + 2, row.rect.y + 6, &name, Text::default());
                let delete = UiRect::new(row.rect.right() - 40, row.rect.y + 2, 36, 16);
                if buttons && p.button(delete, "test:delete").clicked {
                    deleted.push(row.index);
                }
            },
        )
        .response();
    w.clicked += u32::from(r.clicked);
    w.changed += u32::from(r.changed);
    if r.double_clicked {
        w.opened.push(w.selected);
    }
    p.button(UiRect::new(20, 130, 100, 20), "test:done");
    if w.popup {
        let mut popup = ui.layer(UiLayer::Popup, Modal::Yes);
        popup.button(UiRect::new(300, 200, 100, 20), "test:ok");
    }
}

fn list_tester(rows: usize) -> UiTester {
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.app.insert_resource(Worlds {
        rows,
        ..Worlds::default()
    });
    t.add_systems(worlds);
    t.frame();
    t
}

fn worlds_of(t: &UiTester) -> &Worlds {
    t.app.world().resource::<Worlds>()
}

/// How far the list is scrolled, from where its first drawn row is.
fn scroll(t: &UiTester) -> i32 {
    let (index, y) = worlds_of(t).drawn[0];
    LIST.y - y + index as i32 * 20
}

/// Moves to `(x, y)`, presses and releases (a frame each).
fn press_at(t: &mut UiTester, x: i32, y: i32) {
    t.move_to(IVec2::new(x, y))
        .press()
        .frame()
        .release()
        .frame();
}

#[test]
fn the_wheel_scrolls_half_a_row_per_line_and_stops_at_the_ends() {
    let mut t = list_tester(20);
    t.move_to(IVec2::new(60, 60)).frame();
    assert_eq!(scroll(&t), 0);
    t.scroll(-1.0).frame();
    assert_eq!(scroll(&t), 10);
    t.scroll(-3.0).frame();
    assert_eq!(scroll(&t), 40);
    assert!(worlds_of(&t).drawn.contains(&(2, 20)));
    // 20 rows of 20 in a view of 100: at most 300. The row just above the view is declared
    // too (it draws nothing).
    t.scroll(-100.0).frame();
    assert_eq!(scroll(&t), 300);
    assert_eq!(worlds_of(&t).drawn.first(), Some(&(14, 0)));
    assert_eq!(worlds_of(&t).drawn.last(), Some(&(19, 100)));
    t.scroll(100.0).frame();
    assert_eq!(scroll(&t), 0);
    // A touchpad's small steps add up instead of getting lost.
    for _ in 0..4 {
        t.scroll(-0.05).frame();
    }
    assert_eq!(scroll(&t), 2);

    // Away from the list, or under a modal popup, the wheel does not reach it.
    t.move_to(IVec2::new(300, 60)).scroll(-1.0).frame();
    assert_eq!(scroll(&t), 2);
    t.app.world_mut().resource_mut::<Worlds>().popup = true;
    t.move_to(IVec2::new(60, 60)).frame().scroll(-1.0).frame();
    assert_eq!(scroll(&t), 2);
    // Over a row's widget, it still scrolls the list.
    t.app.world_mut().resource_mut::<Worlds>().popup = false;
    t.app.world_mut().resource_mut::<Worlds>().buttons = true;
    t.move_to(IVec2::new(LIST.x + 160, 30)).frame().frame();
    assert_eq!(t.focused(), None);
    assert_eq!(
        t.state().hit(t.state().hovered().unwrap()).unwrap().label,
        "test:delete"
    );
    t.scroll(-1.0).frame();
    assert_eq!(scroll(&t), 12);
}

#[test]
fn the_scrollbar_handle_follows_a_drag_and_a_press_on_its_track() {
    let mut t = list_tester(20);
    // The handle is 32 high (Minecraft's minimum) and travels 68 pixels for 300 of scroll.
    let bar_x = LIST.right() - SCROLLBAR_WIDTH + 2;
    t.move_to(IVec2::new(bar_x, 25)).press().frame();
    t.move_to(IVec2::new(bar_x, 25 + 34)).frame();
    assert_eq!(scroll(&t), 150);
    // Dragging away from the list keeps following until the release.
    t.move_to(IVec2::new(bar_x + 80, 230)).frame();
    assert_eq!(scroll(&t), 300);
    t.move_to(IVec2::new(bar_x, 0)).frame();
    assert_eq!(scroll(&t), 0);
    t.release().frame();
    t.move_to(IVec2::new(bar_x, 80)).frame();
    assert_eq!(scroll(&t), 0);
    // A press on the track brings the handle's middle under the cursor: its top to 84.
    t.move_to(IVec2::new(bar_x, 100)).press().frame();
    assert_eq!(scroll(&t), 282);
    t.release().frame();
    // The scrollbar selects nothing.
    assert_eq!(worlds_of(&t).selected, None);
    assert_eq!(worlds_of(&t).clicked, 0);

    // A list whose rows fit has no scrollbar: a press there does nothing.
    let mut t = list_tester(3);
    press_at(&mut t, bar_x, 25);
    assert_eq!(scroll(&t), 0);
    assert_eq!(worlds_of(&t).selected, None);

    // A press and a release within one frame (a slow frame, a synthetic click) on the track
    // still bring the handle there.
    let mut t = list_tester(20);
    t.move_to(IVec2::new(bar_x, 100)).press().release().frame();
    assert_eq!(scroll(&t), 282);
    t.frame();
    assert_eq!(scroll(&t), 282);
}

#[test]
fn a_press_selects_a_row_and_two_within_250_ms_open_it() {
    let mut t = list_tester(10);
    press_at(&mut t, 60, 65);
    assert_eq!(worlds_of(&t).selected, Some(2));
    assert_eq!((worlds_of(&t).clicked, worlds_of(&t).changed), (1, 1));
    assert_eq!(t.focused().as_deref(), Some("test:worlds"));
    // Two frames later (33 ms): a double click.
    press_at(&mut t, 60, 65);
    assert_eq!(worlds_of(&t).opened, [Some(2)]);
    assert_eq!(worlds_of(&t).changed, 1, "the same row stays selected");
    // A third press starts over.
    press_at(&mut t, 60, 65);
    assert_eq!(worlds_of(&t).opened.len(), 1);
    // Too slow: two single clicks.
    t.wait(DOUBLE_CLICK + Duration::from_millis(50));
    press_at(&mut t, 60, 65);
    assert_eq!(worlds_of(&t).opened.len(), 1);
    // Fast, but on two rows: no double click either.
    t.wait(DOUBLE_CLICK);
    press_at(&mut t, 60, 85);
    press_at(&mut t, 60, 65);
    assert_eq!(worlds_of(&t).opened.len(), 1);
    assert_eq!(worlds_of(&t).selected, Some(2));
    assert_eq!(worlds_of(&t).clicked, 6);
    // Below the last row there is nothing to select.
    let mut t = list_tester(2);
    press_at(&mut t, 60, 90);
    assert_eq!(worlds_of(&t).selected, None);
}

#[test]
fn a_press_picks_the_row_drawn_under_the_cursor_though_the_wheel_turns_in_its_frame() {
    // At the top, a wheel turn up (touchpad momentum) with a press on row 1 (40 to 60).
    let mut t = list_tester(10);
    t.move_to(IVec2::new(60, 45)).frame();
    t.scroll(1.0).press().frame().release().frame();
    assert_eq!(worlds_of(&t).selected, Some(1));
    assert_eq!(scroll(&t), 0);

    // At the bottom, a wheel turn down with a press on the last row's lower half.
    let mut t = list_tester(20);
    t.move_to(IVec2::new(60, 115)).scroll(-100.0).frame();
    assert_eq!(scroll(&t), 300);
    t.scroll(-1.0).press().frame().release().frame();
    assert_eq!(worlds_of(&t).selected, Some(19));

    // In the middle the press takes the row it was over when drawn (row 6 at 100 of scroll),
    // not the one the wheel brings under the cursor in the same frame.
    let mut t = list_tester(20);
    t.move_to(IVec2::new(60, 55)).scroll(-10.0).frame();
    assert_eq!(scroll(&t), 100);
    t.scroll(-1.0).press().frame().release().frame();
    assert_eq!(worlds_of(&t).selected, Some(6));
    assert_eq!(scroll(&t), 110);
}

#[test]
fn up_and_down_move_the_selection_and_scroll_to_it() {
    let mut t = list_tester(20);
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:worlds"));
    t.tap(KeyCode::ArrowDown);
    assert_eq!(worlds_of(&t).selected, Some(0));
    for _ in 0..6 {
        t.tap(KeyCode::ArrowDown);
    }
    // Row 6 (120 to 140) shows at the bottom of the 100-pixel view.
    assert_eq!(worlds_of(&t).selected, Some(6));
    assert_eq!(scroll(&t), 40);
    t.tap(KeyCode::ArrowUp);
    assert_eq!((worlds_of(&t).selected, scroll(&t)), (Some(5), 40));
    // Held: the OS repeats walk the list; it stops at the ends.
    t.key(KeyCode::ArrowUp, ButtonState::Pressed, false);
    for _ in 0..9 {
        t.key(KeyCode::ArrowUp, ButtonState::Pressed, true);
    }
    t.frame()
        .key(KeyCode::ArrowUp, ButtonState::Released, false)
        .frame();
    assert_eq!((worlds_of(&t).selected, scroll(&t)), (Some(0), 0));
    // Enter or Space opens the selection.
    t.tap(KeyCode::Enter).tap(KeyCode::Space);
    assert_eq!(worlds_of(&t).opened, [Some(0), Some(0)]);
    // Tab leaves the list; Up and Down on a button move the focus as usual.
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:done"));
    t.tap(KeyCode::ArrowUp);
    assert_eq!(t.focused().as_deref(), Some("test:worlds"));
    assert_eq!(worlds_of(&t).selected, Some(0));

    // Up with nothing selected starts from the last row.
    let mut t = list_tester(20);
    t.tap(KeyCode::Tab).tap(KeyCode::ArrowUp);
    assert_eq!((worlds_of(&t).selected, scroll(&t)), (Some(19), 300));
}

#[test]
fn row_widgets_take_the_pointer_and_the_keyboard_reaches_rows_scrolled_away() {
    let mut t = list_tester(10);
    t.app.world_mut().resource_mut::<Worlds>().buttons = true;
    t.frame();
    // The button of row 1 takes the press; the row is not selected.
    press_at(&mut t, LIST.x + 160, 50);
    assert_eq!(worlds_of(&t).deleted, [1]);
    assert_eq!(worlds_of(&t).selected, None);
    // Every row's button is its own widget: Tab goes through them, also into the rows below
    // the view, and the list scrolls to show the focused one.
    let mut focused = Vec::new();
    for _ in 0..8 {
        t.tap(KeyCode::Tab);
        focused.push(t.focused().unwrap());
    }
    assert_eq!(focused, ["test:delete"; 8]);
    let hit = t.state().hit(t.state().focused().unwrap()).unwrap().clone();
    assert_eq!(hit.rect, hit.full, "the focused button shows whole");
    assert_eq!(hit.rect.bottom(), LIST.bottom());
    assert_eq!(scroll(&t), 98);
    t.tap(KeyCode::Enter);
    assert_eq!(worlds_of(&t).deleted, [1, 9]);
    // From the last row's button, Tab goes on to the button under the list.
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:done"));
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:worlds"));
    // Into the row just above the view (row 3): the list scrolls up to it.
    t.tap(KeyCode::Tab);
    assert_eq!(scroll(&t), 62);
    // The wheel can scroll the focused button away; it keeps the focus without the list
    // jumping back.
    t.move_to(IVec2::new(60, 60)).scroll(-10.0).frame().frame();
    assert_eq!(scroll(&t), 100);
    assert_eq!(t.focused().as_deref(), Some("test:delete"));
}

/// The row of a list whose widget has the keyboard focus.
fn focused_row(t: &UiTester) -> Option<usize> {
    let id = t.state().focused()?;
    t.state().hit(id)?.row.map(|(_, row)| row)
}

/// Asserts the focused widget shows whole (the list scrolled to it).
fn assert_focused_shows(t: &UiTester) {
    let hit = t.state().hit(t.state().focused().unwrap()).unwrap();
    assert_eq!(hit.rect, hit.full, "the focused widget shows whole");
}

#[test]
fn after_the_wheel_scrolls_the_focused_widget_away_tab_goes_on_from_it() {
    let mut t = list_tester(30);
    t.app.world_mut().resource_mut::<Worlds>().buttons = true;
    t.frame();
    // The list, then the buttons of rows 0 to 3.
    for _ in 0..5 {
        t.tap(KeyCode::Tab);
    }
    assert_eq!(focused_row(&t), Some(3));
    let to_the_end = |t: &mut UiTester| {
        t.move_to(IVec2::new(60, 60)).scroll(-100.0).frame().frame();
        assert_eq!(scroll(t), 500);
    };
    // Rows 25 to 29 show; Tab goes on to row 4, not to the first row in view.
    to_the_end(&mut t);
    assert_eq!(focused_row(&t), Some(3));
    t.tap(KeyCode::Tab);
    assert_eq!(focused_row(&t), Some(4));
    assert_focused_shows(&t);
    // And Shift+Tab to the row before, not out of the rows.
    to_the_end(&mut t);
    hold(&mut t, KeyCode::ShiftLeft, true);
    t.tap(KeyCode::Tab);
    assert_eq!(focused_row(&t), Some(3));
    to_the_end(&mut t);
    t.tap(KeyCode::Tab);
    hold(&mut t, KeyCode::ShiftLeft, false);
    assert_eq!(focused_row(&t), Some(2));
    assert_focused_shows(&t);
}

/// An options page whose rows each hold a button, but for header rows (a label only) and rows
/// whose button is disabled.
#[derive(Resource, Default)]
struct Sections {
    rows: usize,
    headers: Vec<usize>,
    disabled: Vec<usize>,
    pressed: Vec<usize>,
}

fn sections(mut ui: Ui, mut sections: ResMut<Sections>) {
    let s = &mut *sections;
    let rows = s.rows;
    let (headers, disabled, pressed) = (&s.headers, &s.disabled, &mut s.pressed);
    let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
    p.scroll_list(LIST, "test:sections", 20, rows, |p, row| {
        let r = UiRect::new(row.rect.x + 2, row.rect.y + 2, 120, 16);
        if headers.contains(&row.index) {
            p.label(r, "test:header", Text::default());
        } else if p
            .enabled(!disabled.contains(&row.index))
            .button(r, "test:option")
            .clicked
        {
            pressed.push(row.index);
        }
    });
    p.button(UiRect::new(20, 130, 100, 20), "test:done");
}

#[test]
fn tab_walks_past_rows_with_nothing_to_focus_both_ways() {
    let mut t = UiTester::new(UVec2::new(1280, 720));
    // Rows 0 to 4 show; just below the view, a header (row 5) and a disabled row (row 6).
    t.app.insert_resource(Sections {
        rows: 12,
        headers: vec![5],
        disabled: vec![6],
        ..Sections::default()
    });
    t.add_systems(sections);
    t.frame();
    let mut walk = Vec::new();
    for _ in 0..10 {
        t.tap(KeyCode::Tab);
        walk.push(focused_row(&t));
    }
    assert_eq!(walk, [0, 1, 2, 3, 4, 7, 8, 9, 10, 11].map(Some));
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:done"));
    // Back up from the button under the list, past the same two rows.
    hold(&mut t, KeyCode::ShiftLeft, true);
    let mut walk = Vec::new();
    for _ in 0..7 {
        t.tap(KeyCode::Tab);
        walk.push(focused_row(&t));
    }
    hold(&mut t, KeyCode::ShiftLeft, false);
    assert_eq!(walk, [11, 10, 9, 8, 7, 4, 3].map(Some));
    assert_focused_shows(&t);
    t.tap(KeyCode::Enter);
    assert_eq!(t.app.world().resource::<Sections>().pressed, [3]);
}

#[test]
fn only_the_rows_that_show_are_drawn_and_a_lost_selection_is_cleared() {
    let mut t = list_tester(1000);
    let indices = |t: &UiTester| worlds_of(t).drawn.iter().map(|r| r.0).collect::<Vec<_>>();
    // The five that show and the one just below.
    assert_eq!(indices(&t), [0, 1, 2, 3, 4, 5]);
    t.move_to(IVec2::new(60, 60)).scroll(-1.0).frame();
    assert_eq!(indices(&t), [0, 1, 2, 3, 4, 5, 6]);
    t.scroll(-10.0).frame();
    assert_eq!(indices(&t), [4, 5, 6, 7, 8, 9, 10, 11]);
    // Nothing a row draws leaves the list: every quad of the screen's rows lies inside it.
    let scale = t.scale();
    let top = scale.to_physical(IVec2::new(LIST.x, LIST.y));
    let bottom = scale.to_physical(IVec2::new(LIST.right(), LIST.bottom()));
    for quad in t.state().quads(UiLayer::Screen) {
        if quad.max[1] <= scale.to_physical(IVec2::new(0, 125)).y {
            assert!(quad.min[1] >= top.y && quad.max[1] <= bottom.y, "{quad:?}");
            assert!(quad.min[0] >= top.x && quad.max[0] <= bottom.x, "{quad:?}");
        }
    }

    t.app.world_mut().resource_mut::<Worlds>().selected = Some(700);
    t.frame();
    let changed = worlds_of(&t).changed;
    t.app.world_mut().resource_mut::<Worlds>().rows = 500;
    t.frame();
    assert_eq!(worlds_of(&t).selected, None);
    assert_eq!(worlds_of(&t).changed, changed + 1);
}

#[derive(Resource, Default)]
struct Options {
    pressed: Vec<usize>,
    drawn: Vec<(usize, i32)>,
    selected: bool,
}

fn options(mut ui: Ui, mut options: ResMut<Options>) {
    let o = &mut *options;
    o.drawn.clear();
    let (drawn, pressed, selected) = (&mut o.drawn, &mut o.pressed, &mut o.selected);
    let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
    p.scroll_list(LIST, "test:options", 25, 8, |p, row| {
        drawn.push((row.index, row.rect.y));
        *selected |= row.selected;
        let [left, _] = UiRect::new(row.rect.x, row.rect.y + 2, 180, 20).split(10);
        if p.button(left, "test:option").clicked {
            pressed.push(row.index);
        }
    });
}

fn options_of(t: &UiTester) -> &Options {
    t.app.world().resource::<Options>()
}

#[test]
fn a_scroll_list_has_no_selection_and_its_widgets_take_the_keyboard() {
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.app.init_resource::<Options>();
    t.add_systems(options);
    t.frame();
    // A press beside the row's button does nothing and takes no focus.
    press_at(&mut t, LIST.x + 150, 30);
    assert_eq!(t.focused(), None);
    assert!(options_of(&t).pressed.is_empty());
    // Tab goes straight to the first row's button (the list is no stop), then down the rows.
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:option"));
    for _ in 0..4 {
        t.tap(KeyCode::ArrowDown);
    }
    t.tap(KeyCode::Enter);
    assert_eq!(options_of(&t).pressed, [4]);
    // Row 4's button (102 to 122 down the rows) shows at the bottom of the view.
    let (index, y) = options_of(&t).drawn[0];
    assert_eq!(LIST.y - y + index as i32 * 25, 22);
    // The wheel scrolls it (8 rows of 25 in 100: at most 100), and it never selects.
    t.move_to(IVec2::new(60, 60)).scroll(-10.0).frame();
    let (index, y) = options_of(&t).drawn[0];
    assert_eq!(LIST.y - y + index as i32 * 25, 100);
    assert!(!options_of(&t).selected);
}

// ---- the text field

#[derive(Resource, Default)]
struct Form {
    name: String,
    seed: String,
    submitted: Vec<&'static str>,
    created: u32,
}

/// The name field: 120 wide at (20, 20), its text from x = 24.
const NAME: UiRect = UiRect::new(20, 20, 120, 20);

fn form(mut ui: Ui, mut form: ResMut<Form>) {
    let f = &mut *form;
    let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
    if p.text_field(NAME, "test:name", &mut f.name, 16, "test:hint")
        .submitted
    {
        f.submitted.push("name");
    }
    let seed = UiRect::new(20, 50, 120, 20);
    if p.text_field(seed, "test:seed", &mut f.seed, 8, "")
        .submitted
    {
        f.submitted.push("seed");
    }
    if p.button(UiRect::new(20, 80, 120, 20), "test:create")
        .clicked
    {
        f.created += 1;
    }
}

fn form_tester() -> UiTester {
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.app.init_resource::<Form>();
    t.add_systems(form);
    t.frame();
    t
}

fn form_of(t: &UiTester) -> &Form {
    t.app.world().resource::<Form>()
}

fn hold(t: &mut UiTester, key: KeyCode, down: bool) {
    let state = if down {
        ButtonState::Pressed
    } else {
        ButtonState::Released
    };
    t.key(key, state, false).frame();
}

#[test]
fn typing_goes_to_the_focused_field_and_stops_at_its_maximum() {
    let mut t = form_tester();
    assert_eq!(t.ime(), ImeOptions::default());
    t.click("test:name");
    assert_eq!(t.focused().as_deref(), Some("test:name"));
    assert_eq!(
        t.capture(),
        InputCapture {
            pointer: true,
            keyboard: true,
            text: true,
            composing: false
        }
    );
    // The input method is on while a field has the focus, with the caret's place.
    assert!(t.ime().allowed);
    assert!(t.ime().cursor_area.is_some());

    t.type_text("Mi mundo");
    // Space types (it activates buttons, not fields).
    t.tap(KeyCode::Space);
    t.type_text("ñandú");
    assert_eq!(form_of(&t).name, "Mi mundo ñandú");
    t.type_text("xyzw");
    assert_eq!(
        form_of(&t).name,
        "Mi mundo ñandúxy",
        "16 characters at most"
    );
    t.tap(KeyCode::Enter);
    assert_eq!(form_of(&t).submitted, ["name"]);

    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:seed"));
    t.type_text("123456789");
    assert_eq!(form_of(&t).seed, "12345678");
    assert_eq!(form_of(&t).name, "Mi mundo ñandúxy");

    // A button takes the focus: the text and the input method are free again.
    t.click("test:create");
    assert_eq!(form_of(&t).created, 1);
    assert!(!t.capture().text);
    assert_eq!(t.ime(), ImeOptions::default());
    t.type_text("abc");
    assert_eq!(form_of(&t).seed, "12345678");
}

#[test]
fn editing_keys_move_the_caret_select_and_erase_with_repeat() {
    let mut t = form_tester();
    t.app.world_mut().resource_mut::<Form>().name = "abc".to_owned();
    t.frame().tap(KeyCode::Tab).tap(KeyCode::End);
    // A held Backspace erases once per OS repeat.
    t.key(KeyCode::Backspace, ButtonState::Pressed, false)
        .key(KeyCode::Backspace, ButtonState::Pressed, true)
        .frame()
        .key(KeyCode::Backspace, ButtonState::Pressed, true)
        .frame()
        .key(KeyCode::Backspace, ButtonState::Released, false)
        .frame();
    assert_eq!(form_of(&t).name, "");

    // Ctrl jumps and erases words.
    t.type_text("uno dos tres");
    hold(&mut t, KeyCode::ControlLeft, true);
    t.tap(KeyCode::Backspace).tap(KeyCode::ArrowLeft);
    hold(&mut t, KeyCode::ControlLeft, false);
    t.type_text("y ");
    assert_eq!(form_of(&t).name, "uno y dos ");
    // Shift+Home selects to the start; typing replaces the selection.
    hold(&mut t, KeyCode::ShiftLeft, true);
    t.tap(KeyCode::Home);
    hold(&mut t, KeyCode::ShiftLeft, false);
    t.type_text("X");
    assert_eq!(form_of(&t).name, "Xdos ");
    // Ctrl+A and Delete empty it.
    hold(&mut t, KeyCode::ControlLeft, true);
    t.tap(KeyCode::KeyA);
    hold(&mut t, KeyCode::ControlLeft, false);
    t.tap(KeyCode::Delete);
    assert_eq!(form_of(&t).name, "");
    // Left, Right and Delete; typed keys arrive as text (US layout in the tester).
    t.tap(KeyCode::KeyA)
        .tap(KeyCode::KeyB)
        .tap(KeyCode::ArrowLeft);
    t.tap(KeyCode::Delete);
    assert_eq!(form_of(&t).name, "a");
    hold(&mut t, KeyCode::ShiftLeft, true);
    t.tap(KeyCode::KeyC).tap(KeyCode::Digit1);
    hold(&mut t, KeyCode::ShiftLeft, false);
    assert_eq!(form_of(&t).name, "aC!");
    // Enter is no activation: the field reports it, the focus stays.
    t.tap(KeyCode::Enter);
    assert_eq!(form_of(&t).submitted, ["name"]);
    assert_eq!(t.focused().as_deref(), Some("test:name"));
    // Held, it submits once, as a held Enter activates a button once: its OS repeats do not.
    t.key(KeyCode::Enter, ButtonState::Pressed, false)
        .frame()
        .key(KeyCode::Enter, ButtonState::Pressed, true)
        .frame()
        .key(KeyCode::Enter, ButtonState::Pressed, true)
        .key(KeyCode::Enter, ButtonState::Pressed, true)
        .frame()
        .key(KeyCode::Enter, ButtonState::Released, false)
        .frame();
    assert_eq!(form_of(&t).submitted, ["name", "name"]);
}

#[test]
fn the_mouse_places_the_caret_and_a_drag_selects() {
    let mut t = form_tester();
    t.app.world_mut().resource_mut::<Form>().name = "abcdef".to_owned();
    t.frame();
    let w = |t: &UiTester, s: &str| measure(t.app.world().resource::<BitmapFont>(), s, 1) as i32;
    let x = NAME.x + 4;
    let y = NAME.y + 10;
    // A press just past "ab" puts the caret there.
    let at = x + w(&t, "ab") + 1;
    press_at(&mut t, at, y);
    t.type_text("X");
    assert_eq!(form_of(&t).name, "abXcdef");
    // A drag from after "a" to after "abXc" selects "bXc".
    t.move_to(IVec2::new(x + w(&t, "a") + 1, y)).press().frame();
    t.move_to(IVec2::new(x + w(&t, "abXc") + 1, y)).frame();
    t.release().frame();
    t.type_text("-");
    assert_eq!(form_of(&t).name, "a-def");
    // Shift+press extends from the caret (after "a-") to after "a-de".
    let at = x + w(&t, "a-de") + 1;
    hold(&mut t, KeyCode::ShiftLeft, true);
    press_at(&mut t, at, y);
    hold(&mut t, KeyCode::ShiftLeft, false);
    t.type_text("+");
    assert_eq!(form_of(&t).name, "a-+f");
    // Far right of the text: the end.
    press_at(&mut t, NAME.right() - 2, y);
    t.type_text("!");
    assert_eq!(form_of(&t).name, "a-+f!");
}

#[test]
fn text_and_tab_in_one_frame_reach_the_fields_in_their_order() {
    let mut t = form_tester();
    t.click("test:name");
    // A fast typist: "ab", Tab, "12" before the next frame.
    t.app
        .world_mut()
        .write_message(TextInput::Text("ab".to_owned()));
    t.key(KeyCode::Tab, ButtonState::Pressed, false);
    t.app
        .world_mut()
        .write_message(TextInput::Text("12".to_owned()));
    t.frame()
        .key(KeyCode::Tab, ButtonState::Released, false)
        .frame();
    assert_eq!(form_of(&t).name, "ab");
    assert_eq!(form_of(&t).seed, "12");
    // And back: Shift+Tab, then text, then Enter, all in one frame.
    t.key(KeyCode::ShiftLeft, ButtonState::Pressed, false)
        .frame();
    t.key(KeyCode::Tab, ButtonState::Pressed, false);
    t.app
        .world_mut()
        .write_message(TextInput::Text("c".to_owned()));
    t.key(KeyCode::Enter, ButtonState::Pressed, false);
    t.frame();
    assert_eq!(form_of(&t).name, "abc");
    assert_eq!(form_of(&t).seed, "12");
    assert_eq!(form_of(&t).submitted, ["name"]);
    assert_eq!(t.focused().as_deref(), Some("test:name"));
}

/// A key an input method takes, as Windows reports it: a `KeyboardInput` press and release,
/// and nothing in `TextInput`.
fn ime_key(t: &mut UiTester, key: KeyCode) {
    for state in [ButtonState::Pressed, ButtonState::Released] {
        t.app.world_mut().write_message(KeyboardInput {
            key,
            state,
            repeat: false,
        });
    }
}

#[test]
fn keys_an_input_method_takes_neither_move_the_focus_nor_submit() {
    let mut t = form_tester();
    t.click("test:name");
    t.type_text("a");
    // Down picks a candidate, Enter confirms it: both go to the input method.
    t.preedit("ka").frame();
    ime_key(&mut t, KeyCode::ArrowDown);
    t.frame();
    assert_eq!(t.focused().as_deref(), Some("test:name"));
    ime_key(&mut t, KeyCode::Enter);
    t.preedit("");
    t.app
        .world_mut()
        .write_message(TextInput::Text("か".to_owned()));
    // A real Tab in the same frame still moves on, after the commit.
    t.key(KeyCode::Tab, ButtonState::Pressed, false);
    t.type_text("1");
    assert_eq!(form_of(&t).name, "aか");
    assert_eq!(form_of(&t).seed, "1");
    assert!(form_of(&t).submitted.is_empty());
    assert_eq!(t.focused().as_deref(), Some("test:seed"));
}

#[test]
fn escape_is_the_input_method_s_while_it_composes() {
    /// A game screen that closes on Escape, as `InputCapture` asks.
    #[derive(Resource, Default)]
    struct Closed(u32);
    fn close_on_escape(
        keys: Res<ButtonInput<KeyCode>>,
        capture: Res<InputCapture>,
        mut closed: ResMut<Closed>,
    ) {
        if keys.just_pressed(KeyCode::Escape) && !capture.composing {
            closed.0 += 1;
        }
    }
    let mut t = form_tester();
    t.app.init_resource::<Closed>();
    t.add_systems(close_on_escape);
    t.click("test:name");
    assert!(!t.capture().composing);
    t.preedit("nihon").frame();
    assert!(t.capture().composing);
    // Escape cancels the composition: Windows still reports the press, and the composition
    // ends in the same frame. It is the input method's, not the game's.
    ime_key(&mut t, KeyCode::Escape);
    t.preedit("").frame();
    assert!(t.capture().composing);
    assert_eq!(t.app.world().resource::<Closed>().0, 0);
    // With no composition, Escape is the game's again.
    t.frame().tap(KeyCode::Escape);
    assert!(!t.capture().composing);
    assert_eq!(t.app.world().resource::<Closed>().0, 1);
}

#[test]
fn a_real_press_after_presses_the_input_method_took_moves_the_focus_once() {
    let mut t = form_tester();
    t.click("test:seed");
    t.preedit("ka").frame();
    // One slow frame: Down picks a candidate and Enter confirms it (the input method takes
    // both), then a real Down. The text queue has the commit and that Down's key: it is the
    // real press's, not the first Down's.
    ime_key(&mut t, KeyCode::ArrowDown);
    ime_key(&mut t, KeyCode::Enter);
    t.preedit("");
    t.app
        .world_mut()
        .write_message(TextInput::Text("か".to_owned()));
    t.key(KeyCode::ArrowDown, ButtonState::Pressed, false)
        .frame()
        .key(KeyCode::ArrowDown, ButtonState::Released, false)
        .frame();
    assert_eq!(form_of(&t).seed, "か");
    assert_eq!(t.focused().as_deref(), Some("test:create"));
    assert_eq!(form_of(&t).created, 0, "the taken Enter activates nothing");
    assert!(form_of(&t).submitted.is_empty());

    // A click moves the focus from the field to the button in the frame of a taken press: the
    // input method was still on, so that press does not move the focus on either.
    t.click("test:seed");
    t.preedit("ki").frame();
    let create = t.hit("test:create").unwrap().rect;
    t.move_to(IVec2::new(create.x + 1, create.y + 1)).press();
    ime_key(&mut t, KeyCode::ArrowDown);
    t.frame().release().frame();
    assert_eq!(t.focused().as_deref(), Some("test:create"));
}

/// The underline of a composition shown in the field at `field`, if any (one GUI pixel high,
/// under the glyphs).
fn composition_underline(t: &UiTester, field: UiRect) -> Option<UiQuad> {
    let scale = t.scale();
    let n = scale.scale() as i32;
    let y = scale.to_physical(IVec2::new(0, field.y + 6 + 8)).y;
    t.state()
        .quads(UiLayer::Screen)
        .iter()
        .find(|q| q.min[1] == y && q.max[1] - q.min[1] == n)
        .copied()
}

#[test]
fn a_composition_ends_when_the_focus_moves_to_another_field() {
    let mut t = form_tester();
    let seed = t.hit("test:seed").unwrap().rect;
    t.click("test:name");
    t.preedit("sekai").frame();
    assert!(composition_underline(&t, NAME).is_some());
    // A press on the seed field: the composition is the name field's, so the seed field does
    // not show it, and the input method goes off for a frame so the platform ends it too
    // (else its commit would land in the seed field).
    t.move_to(IVec2::new(seed.x + 4, seed.y + 10))
        .press()
        .frame();
    assert_eq!(t.focused().as_deref(), Some("test:seed"));
    assert!(composition_underline(&t, seed).is_none());
    assert!(composition_underline(&t, NAME).is_none());
    assert!(!t.ime().allowed);
    t.release().frame();
    assert!(t.ime().allowed);
    assert!(t.ime().cursor_area.is_some());
    // A composition in the seed field shows there and stays while the focus stays.
    t.preedit("ki").frame();
    assert!(composition_underline(&t, seed).is_some());
    t.move_to(IVec2::new(seed.x + 4, seed.y + 10))
        .press()
        .frame()
        .release()
        .frame();
    assert!(composition_underline(&t, seed).is_some());
    assert!(t.ime().allowed);
    // Shift+Tab to the name field ends it the same way.
    t.key(KeyCode::ShiftLeft, ButtonState::Pressed, false)
        .frame()
        .key(KeyCode::Tab, ButtonState::Pressed, false)
        .frame();
    assert_eq!(t.focused().as_deref(), Some("test:name"));
    assert!(composition_underline(&t, NAME).is_none());
    assert!(!t.ime().allowed);
    t.frame();
    assert!(t.ime().allowed);
}

#[test]
fn a_long_text_scrolls_to_keep_the_caret_in_view() {
    let mut t = form_tester();
    t.click("test:name");
    // 16 "M" are 128 GUI pixels; the field shows 112.
    t.type_text("MMMMMMMMMMMMMMMM");
    let caret_x = |t: &UiTester| t.ime().cursor_area.unwrap().position.x;
    let scale = t.scale();
    let text_x = scale.to_physical(IVec2::new(NAME.x + 4, 0)).x;
    assert_eq!(
        caret_x(&t),
        text_x + 112 * 3,
        "the caret at the field's right"
    );
    t.tap(KeyCode::Home);
    assert_eq!(caret_x(&t), text_x);
    t.tap(KeyCode::ArrowRight).tap(KeyCode::End);
    assert_eq!(caret_x(&t), text_x + 112 * 3);
}

#[derive(Resource, Default)]
struct Lone(String);

fn lone_tester() -> UiTester {
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.app.init_resource::<Lone>();
    t.add_systems(|mut ui: Ui, mut lone: ResMut<Lone>| {
        let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
        p.text_field(NAME, "test:lone", &mut lone.0, 16, "II");
    });
    t.frame();
    t
}

/// The screen's quads: the field's 9 sprite pieces and two per glyph (shadow and glyph).
fn quads(t: &UiTester) -> usize {
    t.state().quads(UiLayer::Screen).len()
}

#[test]
fn the_hint_shows_while_empty_and_the_caret_blinks_in_real_time() {
    let mut t = lone_tester();
    assert_eq!(quads(&t), 9 + 4, "the hint");
    // Focused: the hint goes, the caret "_" shows from the press.
    t.click("test:lone");
    assert_eq!(quads(&t), 9 + 2);
    let generation = t.draw().generation();
    t.frames(16);
    assert_eq!(t.draw().generation(), generation, "shown for 300 ms, idle");
    t.frame();
    assert_eq!(quads(&t), 9, "hidden from 300 ms");
    t.frames(18);
    assert_eq!(quads(&t), 9 + 2, "shown again from 600 ms");
    t.frames(10);
    // Typing shows it at once.
    t.type_text("a");
    assert_eq!(quads(&t), 9 + 2 + 2);
    // Without the focus there is no caret, and the text hides the hint.
    t.move_to(IVec2::new(300, 200))
        .press()
        .frame()
        .release()
        .frame();
    assert_eq!(t.focused(), None);
    assert_eq!(quads(&t), 9 + 2);
}

#[test]
fn an_input_method_composes_at_the_caret_and_commits_as_text() {
    let mut t = lone_tester();
    t.click("test:lone");
    t.preedit("ka").frame();
    // The composition and its underline; no caret, and the value is untouched.
    assert_eq!(quads(&t), 9 + 4 + 1);
    assert_eq!(t.app.world().resource::<Lone>().0, "");
    // The input method ends it and commits (the runner sends the commit as text).
    t.preedit("");
    t.type_text("か");
    assert_eq!(t.app.world().resource::<Lone>().0, "か");
    assert_eq!(quads(&t), 9 + 2 + 2);
    // A composition left behind does not show once the field loses the focus.
    t.preedit("ki").frame();
    t.move_to(IVec2::new(300, 200))
        .press()
        .frame()
        .release()
        .frame();
    t.click("test:lone");
    assert_eq!(quads(&t), 9 + 2 + 2);
}

#[test]
fn a_composition_at_the_end_of_a_full_field_scrolls_into_view() {
    let mut t = lone_tester();
    t.click("test:lone");
    // 14 "M" fill the 112 GUI pixels the field shows; the caret is at their end.
    t.type_text("MMMMMMMMMMMMMM");
    let scale = t.scale();
    let n = scale.scale() as i32;
    let text_right = scale.to_physical(IVec2::new(NAME.x + 4 + 112, 0)).x;
    let ka = measure(t.app.world().resource::<BitmapFont>(), "ka", 1) as i32 * n;
    t.preedit("ka").frame();
    let caret_x = t.ime().cursor_area.unwrap().position.x;
    assert!(
        caret_x + ka <= text_right,
        "the composition fits: {caret_x} + {ka} > {text_right}"
    );
    // Its underline shows whole.
    let underline = composition_underline(&t, NAME).unwrap();
    assert_eq!(underline.max[0] - underline.min[0], ka);
    assert_eq!(underline.min[0], caret_x);
}

// ---- the tabs

#[derive(Resource, Default)]
struct Pages {
    tab: usize,
    changed: u32,
}

fn pages_tester() -> UiTester {
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.app.init_resource::<Pages>();
    t.add_systems(|mut ui: Ui, mut pages: ResMut<Pages>| {
        let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
        let labels = ["test:game", "test:world", "test:more"];
        let bar = UiRect::new(20, 10, 240, 24);
        if p.tabs(bar, &mut pages.tab, &labels).changed {
            pages.changed += 1;
        }
        p.button(UiRect::new(20, 60, 100, 20), "test:done");
    });
    t.frame();
    t
}

fn pages_of(t: &UiTester) -> (usize, u32) {
    let pages = t.app.world().resource::<Pages>();
    (pages.tab, pages.changed)
}

#[test]
fn tabs_open_on_a_press_and_follow_the_arrows_and_ctrl_tab() {
    let mut t = pages_tester();
    assert_eq!(
        t.hit("test:world").unwrap().rect,
        UiRect::new(100, 10, 80, 24)
    );
    t.click("test:world");
    assert_eq!(pages_of(&t), (1, 1));
    assert_eq!(t.focused().as_deref(), Some("test:world"));
    // Left and Right open the tab next to the focused one and take the focus there.
    t.tap(KeyCode::ArrowRight);
    assert_eq!(pages_of(&t), (2, 2));
    assert_eq!(t.focused().as_deref(), Some("test:more"));
    t.tap(KeyCode::ArrowRight);
    assert_eq!(pages_of(&t), (2, 2), "no tab past the last");
    t.tap(KeyCode::ArrowLeft);
    assert_eq!(pages_of(&t), (1, 3));
    assert_eq!(t.focused().as_deref(), Some("test:world"));
    // Tab only moves the focus; Enter opens the focused tab.
    t.tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("test:more"));
    assert_eq!(pages_of(&t).0, 1);
    t.tap(KeyCode::Enter);
    assert_eq!(pages_of(&t), (2, 4));
    // Ctrl+Tab and Ctrl+Shift+Tab, wrapping; the focus stays.
    hold(&mut t, KeyCode::ControlLeft, true);
    t.tap(KeyCode::Tab);
    assert_eq!(pages_of(&t), (0, 5));
    t.tap(KeyCode::Tab);
    assert_eq!(pages_of(&t), (1, 6));
    hold(&mut t, KeyCode::ShiftLeft, true);
    t.tap(KeyCode::Tab).tap(KeyCode::Tab);
    assert_eq!(pages_of(&t), (2, 8));
    hold(&mut t, KeyCode::ShiftLeft, false);
    hold(&mut t, KeyCode::ControlLeft, false);
    assert_eq!(t.focused().as_deref(), Some("test:more"));
    // Ctrl+Tab works with any widget focused, or none.
    t.click("test:done");
    hold(&mut t, KeyCode::ControlLeft, true);
    t.tap(KeyCode::Tab);
    assert_eq!(pages_of(&t), (0, 9));
}

// ---- the golden raster

#[derive(Resource)]
struct Golden {
    tab: usize,
    name: String,
    seed: String,
    selected: Option<usize>,
}

/// A fixed screen with every new widget: tabs, a focused field with a selection, an empty
/// field with its hint, and a scrolled list with a selected row, its rows cut by the list.
fn golden_screen(mut ui: Ui, mut golden: ResMut<Golden>) {
    let g = &mut *golden;
    let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
    p.dim(argb(0x80101010));
    p.tabs(
        UiRect::new(40, 4, 240, 20),
        &mut g.tab,
        &["Game", "World", "More"],
    );
    p.text_field(
        UiRect::new(60, 30, 200, 20),
        "Name",
        &mut g.name,
        32,
        "Name",
    );
    p.text_field(
        UiRect::new(60, 54, 200, 20),
        "Seed",
        &mut g.seed,
        32,
        "Empty: random",
    );
    let list = UiRect::new(40, 80, 240, 120);
    p.list(list, "Worlds", 24, 12, &mut g.selected, |p, row| {
        let icon = UiRect::new(row.rect.x + 2, row.rect.y + 2, 20, 20);
        p.rect(icon, rgb(0x305080 + 0x0A0400 * row.index as u32));
        let name = format!("World {} \u{f1}", row.index);
        p.text(row.rect.x + 26, row.rect.y + 3, &name, Text::default());
        let line = Text::new(rgb(0x808080));
        p.text(row.rect.x + 26, row.rect.y + 13, "Survival, Normal", line);
    });
}

#[test]
fn golden_raster_of_the_list_the_text_field_and_the_tabs() {
    let mut t = UiTester::new(UVec2::new(640, 480));
    t.app.insert_resource(Golden {
        tab: 1,
        name: "Mundo \u{f1}and\u{fa}".to_owned(),
        seed: String::new(),
        selected: Some(3),
    });
    t.add_systems(golden_screen);
    t.frame();
    // The list scrolled by one wheel line (12 pixels), then "Mun" selected in the name.
    t.move_to(IVec2::new(100, 150)).scroll(-1.0).frame();
    t.click("Name").tap(KeyCode::Home);
    hold(&mut t, KeyCode::ShiftLeft, true);
    t.tap(KeyCode::ArrowRight)
        .tap(KeyCode::ArrowRight)
        .tap(KeyCode::ArrowRight);
    hold(&mut t, KeyCode::ShiftLeft, false);
    assert_eq!(t.focused().as_deref(), Some("Name"));

    let image = t.raster([0, 0, 0, 255]);
    if let Ok(dir) = std::env::var("BOXEL_UI_DUMP") {
        std::fs::write(format!("{dir}/widgets-640x480.rgba"), &image).unwrap();
    }
    let draw = t.draw();
    let atlas = t.app.world().resource::<UiAtlas>();
    let size = draw.target_size();
    assert_eq!(
        paint_runs(&runs(draw.quads(), atlas), size, [0, 0, 0, 255]),
        rasterize(draw.quads(), atlas, size, [0, 0, 0, 255])
    );
    assert_eq!(fingerprint(&image), GOLDEN, "{:#018x}", fingerprint(&image));
}

/// FNV-1a 64 of the 640x480 raster. A deliberate change of the widgets, the skin or the font
/// updates it; run with `BOXEL_UI_DUMP=dir` to write the raw RGBA image and look at it.
const GOLDEN: u64 = 0xb515_6a94_46b7_c225;
