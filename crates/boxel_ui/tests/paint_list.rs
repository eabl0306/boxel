//! The composed paint list through `UiPlugin`: generations, the retained HUD, the GUI scale and
//! a golden raster of a fixed screen in the procedural skin.

use boxel_ecs::prelude::*;
use boxel_math::{IVec2, UVec2};
use boxel_ui::color::{argb, rgb};
use boxel_ui::raster::{fingerprint, paint_runs, rasterize, runs};
use boxel_ui::testing::UiTester;
use boxel_ui::{
    Column, GuiScale, GuiScaleSetting, Modal, SkinSlot, Steps, Text, Ui, UiAtlas, UiLayer, UiRect,
};
use boxel_window::KeyCode;

fn menu(mut ui: Ui) {
    let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
    let mut col = Column::new(10, 10, 200, 24);
    p.button(col.next(), "test:a");
    p.button(col.next(), "test:b");
}

#[test]
fn an_idle_menu_keeps_its_generation() {
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.add_systems(menu);
    t.frames(2);
    let generation = t.draw().generation();
    assert!(!t.draw().is_empty());
    t.frames(100);
    assert_eq!(t.draw().generation(), generation, "100 idle frames");
    // Hovering a button changes its sprite: a new generation, then idle again.
    let b = t.hit("test:b").unwrap().rect;
    t.move_to(IVec2::new(b.x + 1, b.y + 1)).frame();
    let hovered = t.draw().generation();
    assert_ne!(hovered, generation);
    t.frames(10);
    assert_eq!(t.draw().generation(), hovered);
}

#[test]
fn a_procedural_button_is_nine_quads_and_its_label() {
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.add_systems(|mut ui: Ui| {
        ui.layer(UiLayer::Screen, Modal::Yes)
            .button(UiRect::new(0, 0, 150, 20), "II");
    });
    t.frame();
    // 9 sprite pieces, then the two glyphs twice (shadow first).
    assert_eq!(t.draw().quads().len(), 9 + 4);
    let quads = t.draw().quads();
    assert_eq!(quads[9].tint, [0x3F, 0x3F, 0x3F, 255], "shadow");
    assert_eq!(quads[11].tint, [255; 4], "text");
    assert_eq!(
        quads[11].min[0] + 3,
        quads[9].min[0],
        "shadow 1 GUI pixel right"
    );
}

#[test]
fn the_hud_is_retained_and_cleared_by_a_scale_change() {
    #[derive(Resource, Default)]
    struct Redraw(bool);
    fn hud(mut ui: Ui, mut redraw: ResMut<Redraw>) {
        if std::mem::take(&mut redraw.0) {
            ui.layer(UiLayer::Hud, Modal::No)
                .rect(UiRect::new(1, 1, 5, 5), rgb(0xFF0000));
        }
    }
    let mut t = UiTester::new(UVec2::new(1280, 720));
    t.app.insert_resource(Redraw(true));
    t.add_systems(hud);
    t.frame();
    let generation = t.draw().generation();
    assert_eq!(t.draw().quads().len(), 1);
    t.frames(50);
    assert_eq!(t.draw().quads().len(), 1, "kept without a redraw");
    assert_eq!(t.draw().generation(), generation);

    // A new scale empties it until the game draws it again.
    t.app.world_mut().resource_mut::<GuiScale>().setting = GuiScaleSetting::Fixed(2);
    t.frame();
    assert_eq!(t.scale().scale(), 2);
    assert!(t.draw().is_empty());
    t.app.insert_resource(Redraw(true));
    t.frame();
    let quad = t.draw().quads()[0];
    assert_eq!((quad.min, quad.max), ([2, 2], [12, 12]));

    // A resize changes the GUI area (the HUD's positions depend on it): emptied too, and the
    // target follows.
    t.resize(UVec2::new(1300, 720)).frame();
    assert!(t.draw().is_empty());
    assert_eq!(t.draw().target_size(), UVec2::new(1300, 720));
    // Below 640x480 a fixed 2 no longer fits.
    t.resize(UVec2::new(500, 400)).frame();
    assert_eq!(t.scale().scale(), 1);
    assert!(t.draw().is_empty());
}

#[test]
fn a_closed_screen_leaves_the_paint_list() {
    #[derive(Resource)]
    struct Open(bool);
    let mut t = UiTester::new(UVec2::new(640, 480));
    t.app.insert_resource(Open(true));
    t.add_systems(menu.run_if(|open: Res<Open>| open.0));
    t.frame();
    assert!(!t.draw().is_empty());
    t.app.insert_resource(Open(false));
    t.frame();
    assert!(t.draw().is_empty());
    assert!(t.state().hits(UiLayer::Screen).is_empty());
}

/// A fixed screen: every widget, text sizes, a clip, a dim, a tooltip and the HUD.
fn golden_screen(mut ui: Ui) {
    {
        let mut hud = ui.layer(UiLayer::Hud, Modal::No);
        hud.rect(UiRect::new(4, 220, 30, 10), rgb(0xAA2222));
        hud.text(40, 221, "HUD 20/20", Text::default());
    }
    let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
    p.dim(argb(0x80101010));
    let w = p.gui_size().x;
    p.text_centered(w / 2, 12, "Boxel UI", Text::title(2));
    let mut col = Column::new(w / 2 - 100, 40, 200, 24);
    p.button(col.next(), "Normal");
    p.button(col.next(), "Focused");
    p.enabled(false)
        .button(col.next(), "Disabled")
        .tooltip("Not yet, maybe later");
    let [l, r] = col.next().split(4);
    let mut mode = 1u8;
    p.cycle(l, "Mode", &mut mode, &[(0, "A"), (1, "B")]);
    let mut value = 25;
    p.slider(r, "Value", &mut value, Steps::range(0, 100), |v| {
        format!("{v}%")
    });
    p.with_clip(UiRect::new(20, 150, 120, 12), |p| {
        p.rect(UiRect::new(10, 145, 200, 30), argb(0x60000000));
        p.text(22, 152, "Clipped: áéíóú ñ ü ¿¡ and on", Text::default());
    });
    p.skin(SkinSlot::MenuBackground, UiRect::new(150, 145, 40, 20));
    p.label(
        UiRect::new(200, 150, 70, 12),
        "A label too long to fit",
        Text::new(rgb(0xFFFF55)),
    );
}

#[test]
fn golden_raster_of_a_fixed_screen() {
    let mut t = UiTester::new(UVec2::new(640, 480));
    t.add_systems(golden_screen);
    t.frame();
    // Focus "Focused" with the keyboard and hover "Disabled" for its tooltip.
    t.tap(KeyCode::Tab).tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("Focused"));
    let disabled = t.hit("Disabled").unwrap().rect;
    t.move_to(IVec2::new(disabled.x + 20, disabled.y + 10))
        .frame()
        .frame();
    assert!(!t.state().quads(UiLayer::Popup).is_empty(), "tooltip shown");

    let image = t.raster([0, 0, 0, 255]);
    if let Ok(dir) = std::env::var("BOXEL_UI_DUMP") {
        std::fs::write(format!("{dir}/golden-640x480.rgba"), &image).unwrap();
    }
    // The runs of the same list paint the same pixels.
    let draw = t.draw();
    let atlas = t.app.world().resource::<UiAtlas>();
    let size = draw.target_size();
    assert_eq!(
        paint_runs(&runs(draw.quads(), atlas), size, [0, 0, 0, 255]),
        rasterize(draw.quads(), atlas, size, [0, 0, 0, 255])
    );
    assert_eq!(fingerprint(&image), GOLDEN, "{:#018x}", fingerprint(&image));
}

/// FNV-1a 64 of the 640x480 raster. A deliberate change of the skin, the font or the raster rule
/// updates it; run with `BOXEL_UI_DUMP=dir` to write the raw RGBA image and look at it.
const GOLDEN: u64 = 0xff21_c4fd_f5a7_06bd;
