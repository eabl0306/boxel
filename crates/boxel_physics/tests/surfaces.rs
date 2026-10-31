//! Block surfaces (ADR 0047): slippery and slow ground, weak jumps, cells that hold a body back,
//! climbing, sliding down a side, blocks that hold only bodies above them (with treads), and
//! bounces. First the pure sweep and scan (with proptests), then bodies in an `App`; everything
//! bit for bit the same far from the origin and at the world's edge.
mod common;
mod sim;

use boxel_app::App;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BlockSurfaces, BodySettings, CharacterController, Climb, Collider, CollisionTable, Contacts,
    Drag, Hold, MoveIntent, StepUp, Surface, SweepConfig, TestWorld, Tread, Treads, Velocity,
    move_box_treading, sense_inside,
};
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, CollisionBox, FaceTextures,
    MAX_BLOCK_COORD, RenderShape, TextureId, WorldPos,
};
use boxel_world::ChunkMap;
use common::{STONE, at, fill, player};
use proptest::prelude::*;
use sim::{STEP, Sim};

const DT: f64 = 1.0 / 64.0;

// The test blocks' surfaces: Minecraft's numbers in the engine's units (ADR 0047).
const ICE_FRICTION: f64 = -9.81;
const SLOW_DRAG: f64 = 8.66;
const WEB: Drag = Drag {
    across: 93.73,
    vertical: 407.76,
};
const SNOW: Drag = Drag {
    across: 17.37,
    vertical: 13.21,
};
const SLIDE: f64 = 2.548;
const BOOTS: u8 = 1;

/// The demo blocks plus the test ones: `test:ice`, `test:slow` (slow ground, weak jumps),
/// `test:web` (no collision, a strong drag), `test:ladder` (a thin box on its +Z side),
/// `test:vine` (no collision), `test:scaffold` (boards and legs, held from above), `test:snow`
/// (a full cube held from above with [`BOOTS`]), `test:honey` (a smaller box whose sides slide)
/// and `test:bouncy`.
fn registry() -> BlockRegistry {
    let mut b = boxel_demo::blocks::builder();
    let def = |name: &str, collision: BlockCollision| BlockDef {
        name: name.to_string(),
        shape: RenderShape::None,
        model: BlockModel::CUBE,
        collision,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(TextureId(0)),
    };
    let boxes = |list: &[([u8; 3], [u8; 3])]| {
        BlockCollision::Boxes(list.iter().map(|&(a, b)| CollisionBox::new(a, b)).collect())
    };
    let mut scaffold = vec![([0, 14, 0], [16, 16, 16])];
    for (x, z) in [(0, 0), (14, 0), (0, 14), (14, 14)] {
        scaffold.push(([x, 0, z], [x + 2, 14, z + 2]));
    }
    for d in [
        def("test:ice", BlockCollision::Full),
        def("test:slow", BlockCollision::Full),
        def("test:web", BlockCollision::None),
        def("test:ladder", boxes(&[([0, 0, 13], [16, 16, 16])])),
        def("test:vine", BlockCollision::None),
        def("test:scaffold", boxes(&scaffold)),
        def("test:snow", BlockCollision::Full),
        def("test:honey", boxes(&[([1, 0, 1], [15, 15, 15])])),
        def("test:bouncy", BlockCollision::Full),
    ] {
        b.add_block(d).expect("test block");
    }
    b.build()
}

fn surfaces() -> BlockSurfaces {
    let mut s = BlockSurfaces::default();
    let ladder = Climb::default();
    s.set(
        "test:ice",
        Surface {
            friction: ICE_FRICTION,
            ..Surface::PLAIN
        },
    )
    .set(
        "test:slow",
        Surface {
            drag: SLOW_DRAG,
            jump: 0.5,
            ..Surface::PLAIN
        },
    )
    .set(
        "test:web",
        Surface {
            inside: Some(WEB),
            ..Surface::PLAIN
        },
    )
    .set(
        "test:ladder",
        Surface {
            climb: Some(ladder),
            ..Surface::PLAIN
        },
    )
    .set(
        "test:vine",
        Surface {
            climb: Some(ladder),
            ..Surface::PLAIN
        },
    )
    .set(
        "test:scaffold",
        Surface {
            climb: Some(Climb {
                sneak_holds: false,
                ..ladder
            }),
            hold: Some(Hold::default()),
            ..Surface::PLAIN
        },
    )
    .set(
        "test:snow",
        Surface {
            inside: Some(SNOW),
            climb: Some(Climb {
                up: 3.528,
                max: f64::INFINITY,
                sneak_holds: false,
                treads: BOOTS,
            }),
            hold: Some(Hold { treads: BOOTS }),
            ..Surface::PLAIN
        },
    )
    .set(
        "test:honey",
        Surface {
            drag: SLOW_DRAG,
            jump: 0.5,
            slide: Some(SLIDE),
            ..Surface::PLAIN
        },
    )
    .set(
        "test:bouncy",
        Surface {
            bounce: Some(1.0),
            ..Surface::PLAIN
        },
    );
    s
}

fn state(registry: &BlockRegistry, name: &str) -> BlockState {
    registry.by_name(name).expect(name)
}

fn table(registry: &BlockRegistry) -> CollisionTable {
    let mut t = CollisionTable::from_registry(registry);
    assert!(surfaces().apply(registry, &mut t).is_empty());
    t
}

// ------------------------------------------------------------------------------ the table

#[test]
fn plain_surfaces_leave_the_table_as_it_was() {
    let registry = registry();
    let plain = CollisionTable::from_registry(&registry);
    let mut t = plain.clone();
    t.set_surfaces(|_| Some(Surface::PLAIN));
    assert_eq!(t, plain);
    assert!(!t.has_surfaces());
    let mut t = table(&registry);
    assert!(t.has_surfaces() && t.has_ground_surfaces() && t.has_inside_surfaces());
    assert!(t.has_holds());
    assert!(t.acts_inside(state(&registry, "test:web")));
    assert!(!t.acts_inside(state(&registry, "test:ice")));
    assert_eq!(
        t.surface(state(&registry, "test:ice")).friction,
        ICE_FRICTION
    );
    assert_eq!(*t.surface(STONE), Surface::PLAIN);
    // Unknown names are reported, the rest applied.
    let mut more = surfaces();
    more.set("test:nothing", Surface::PLAIN);
    assert_eq!(more.apply(&registry, &mut t), vec!["test:nothing"]);
    // NaN and negative values are cleaned.
    t.set_surfaces(|_| {
        Some(Surface {
            friction: f64::NAN,
            drag: -1.0,
            jump: f64::INFINITY,
            bounce: Some(2.0),
            ..Surface::PLAIN
        })
    });
    let s = t.surface(STONE);
    assert_eq!(
        (s.friction, s.drag, s.jump, s.bounce),
        (0.0, 0.0, 1.0, Some(1.0))
    );
}

// ------------------------------------------------------------------------------ the sweep

fn sweep(
    world: &TestWorld,
    t: &CollisionTable,
    start: WorldPos,
    d: DVec3,
    tread: Tread,
) -> WorldPos {
    let mut src = world;
    move_box_treading(
        &mut src,
        t,
        start,
        player(),
        d,
        &[],
        &SweepConfig::default(),
        StepUp::NONE,
        tread,
    )
    .expect("loaded")
    .end
}

#[test]
fn a_held_block_holds_only_bodies_above_it_that_do_not_descend() {
    let registry = registry();
    let t = table(&registry);
    let scaffold = state(&registry, "test:scaffold");
    let snow = state(&registry, "test:snow");
    for base in [
        I64Vec3::ZERO,
        I64Vec3::new(1 << 31, -(1 << 31), 1 << 40),
        I64Vec3::splat(MAX_BLOCK_COORD - 64),
    ] {
        let mut world = TestWorld::new();
        fill(&mut world, base, [0, 0, 0], [0, 0, 0], scaffold);
        fill(&mut world, base, [2, 0, 0], [2, 0, 0], snow);
        let p = |x: f64, y: f64| WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(x, y, 0.5));
        let down = DVec3::new(0.0, -0.5, 0.0);
        let walker = Tread::default();
        let booted = Tread {
            treads: BOOTS,
            ..walker
        };
        let sneaking = Tread {
            descending: true,
            ..booted
        };
        // On top of the scaffold: held unless sneaking.
        assert_eq!(sweep(&world, &t, p(0.5, 1.25), down, walker), p(0.5, 1.0));
        assert_eq!(
            sweep(&world, &t, p(0.5, 1.25), down, sneaking),
            p(0.5, 0.75)
        );
        // Inside it (below its top): it is not there.
        assert_eq!(sweep(&world, &t, p(0.5, 0.75), down, walker), p(0.5, 0.25));
        // From the side, at its level: no wall.
        let across = DVec3::new(1.0, 0.0, 0.0);
        assert_eq!(sweep(&world, &t, p(-1.0, 0.0), across, walker), p(0.0, 0.0));
        // The snow holds only booted bodies, and not while they sneak.
        assert_eq!(sweep(&world, &t, p(2.5, 1.25), down, walker), p(2.5, 0.75));
        assert_eq!(sweep(&world, &t, p(2.5, 1.25), down, booted), p(2.5, 1.0));
        assert_eq!(
            sweep(&world, &t, p(2.5, 1.25), down, sneaking),
            p(2.5, 0.75)
        );
        // Within the tolerance under the top still counts as above.
        assert_eq!(
            sweep(&world, &t, p(2.5, 1.0 - 1e-6), down, booted),
            p(2.5, 1.0)
        );
    }
}

proptest! {
    /// A sweep through held full cubes is the sweep through a world where the ones the body
    /// starts above are stone and the rest air: a descending body sees only air. The same bit
    /// for bit at any base.
    #[test]
    fn held_cubes_are_solid_below_and_air_above(
        seed in any::<u64>(),
        start in prop::array::uniform3(0.5f64..5.5),
        d in prop::array::uniform3(-3.0f64..3.0),
        descending in any::<bool>(),
        far in 0usize..3,
    ) {
        let registry = registry();
        let t = table(&registry);
        let snow = state(&registry, "test:snow");
        let base = [
            I64Vec3::ZERO,
            I64Vec3::new(1 << 40, -(1 << 31), (1 << 31) + 5),
            I64Vec3::splat(MAX_BLOCK_COORD - 64),
        ][far];
        let start = WorldPos::from_block(at(base, 0, 0, 0), DVec3::from_array(start));
        let bottom = (start.block.y - base.y) as f64 + start.frac.y + player().min.y;
        let mut held = TestWorld::new();
        let mut plain = TestWorld::new();
        let mut rng = common::Rng(seed);
        for y in -2..9 {
            for x in -2..9 {
                for z in -2..9 {
                    if rng.unit() < 0.3 {
                        held.set(at(base, x, y, z), snow);
                        if !descending && bottom > (y + 1) as f64 - 1e-5 {
                            plain.set(at(base, x, y, z), STONE);
                        }
                    }
                }
            }
        }
        let tread = Tread { descending, treads: BOOTS };
        let d = DVec3::from_array(d);
        let a = sweep(&held, &t, start, d, tread);
        let b = sweep(&plain, &t, start, d, Tread::default());
        prop_assert_eq!(a, b);
    }

    /// The scan of the cells a box is in gives the same at any base.
    #[test]
    fn the_scan_is_the_same_far_away(
        seed in any::<u64>(),
        frac in prop::array::uniform3(0.0f64..1.0),
        on_ground in any::<bool>(),
    ) {
        let registry = registry();
        let t = table(&registry);
        let kinds = ["test:web", "test:honey", "test:ladder", "test:snow", "boxel:stone"]
            .map(|n| state(&registry, n));
        let scan = |base: I64Vec3| {
            let mut world = TestWorld::new();
            let mut rng = common::Rng(seed);
            for y in -1..3 {
                for x in -1..2 {
                    for z in -1..2 {
                        let k = (rng.next() % 8) as usize;
                        if let Some(&s) = kinds.get(k) {
                            world.set(at(base, x, y, z), s);
                        }
                    }
                }
            }
            let pos = WorldPos::from_block(at(base, 0, 0, 0), DVec3::from_array(frac));
            let mut src = &world;
            sense_inside(&mut src, &t, pos, player(), 1e-7, on_ground, Some(BOOTS))
        };
        let near = scan(I64Vec3::ZERO);
        for base in [I64Vec3::new(1 << 31, 7, -(1 << 40)), I64Vec3::splat(-(MAX_BLOCK_COORD - 64))] {
            prop_assert_eq!(scan(base), near);
        }
    }
}

#[test]
fn the_scan_finds_the_strongest_drag_the_climb_at_the_feet_and_a_side_to_slide() {
    let registry = registry();
    let t = table(&registry);
    let base = I64Vec3::new(1 << 40, 3, -(1 << 31));
    let mut world = TestWorld::new();
    fill(
        &mut world,
        base,
        [0, 0, 0],
        [0, 0, 0],
        state(&registry, "test:snow"),
    );
    fill(
        &mut world,
        base,
        [0, 1, 0],
        [0, 1, 0],
        state(&registry, "test:web"),
    );
    fill(
        &mut world,
        base,
        [1, 0, 0],
        [1, 2, 0],
        state(&registry, "test:honey"),
    );
    let scan = |x: f64, y: f64, on_ground: bool, climber: Option<u8>| {
        let pos = WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(x, y, 0.5));
        let mut src = &world;
        sense_inside(&mut src, &t, pos, player(), 1e-7, on_ground, climber)
    };
    // In the snow and the web: the web's drag; climbing the snow only with boots.
    let s = scan(0.5, 0.25, false, Some(BOOTS));
    assert_eq!(s.drag, Some(WEB));
    assert_eq!(s.climb.map(|c| c.up), Some(3.528));
    assert_eq!(scan(0.5, 0.25, false, Some(0)).climb, None);
    assert_eq!(scan(0.5, 0.25, false, None).climb, None);
    // Against the honey's side (its box starts 1/16 into the cell), off the ground: slides.
    let beside = 1.0 - 0.3 + 0.0625;
    assert_eq!(scan(beside, 0.5, false, None).slide, Some(SLIDE));
    assert_eq!(scan(beside, 0.5, true, None).slide, None, "on the ground");
    // Above the honey's top (15/16 of the third cell): no slide.
    assert_eq!(scan(beside, 2.95, false, None).slide, None);
    // Not touching the honey's cell.
    assert_eq!(scan(beside - 0.07, 0.5, false, None).slide, None);
}

// ------------------------------------------------------------------------------ bodies

fn with_surfaces(app: &mut App) {
    app.insert_resource(surfaces());
}

fn sim(base: I64Vec3) -> Sim {
    Sim::with_registry(base, STEP, registry(), with_surfaces)
}

/// Places `name` in every block of `min..=max` (relative to the base) and waits for the edits.
fn build(sim: &mut Sim, min: [i64; 3], max: [i64; 3], name: &str) {
    let s = state(&registry(), name);
    for x in min[0]..=max[0] {
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                sim.edit(sim.block(x, y, z), s);
            }
        }
    }
    let last = sim.block(max[0], max[1], max[2]);
    while sim.app.world().resource::<ChunkMap>().block_at(last) != Some(s) {
        sim.frame(STEP);
    }
}

/// A 0.6 x 1.8 walker at `(x, y, z)` (relative to the base).
fn walker(sim: &mut Sim, x: f64, y: f64, z: f64) -> boxel_ecs::prelude::Entity {
    let pos = sim.pos(x, y, z);
    sim.spawn(pos, CharacterController::default())
}

fn intent(sim: &mut Sim, body: boxel_ecs::prelude::Entity, f: impl FnOnce(&mut MoveIntent)) {
    f(&mut sim
        .app
        .world_mut()
        .get_mut::<MoveIntent>(body)
        .expect("intent"));
}

fn feet(sim: &Sim, body: boxel_ecs::prelude::Entity) -> DVec3 {
    let p = sim.position(body);
    (p.block.as_i64vec3() - sim.base).as_dvec3() + p.frac
}

/// What a scenario records each step: the feet (relative to the base) and the velocity, bit
/// for bit.
type Record = Vec<([u64; 3], [u64; 3])>;

fn record(sim: &Sim, body: boxel_ecs::prelude::Entity, out: &mut Record) {
    let f = feet(sim, body);
    let v = sim.get::<Velocity>(body).0;
    out.push((
        f.to_array().map(f64::to_bits),
        v.to_array().map(f64::to_bits),
    ));
}

/// Walks east on `floor` (the ground layer under x 0..62) until the speed levels off, then lets
/// go: the top speed and how far it slid.
fn walk_and_slide(sim: &mut Sim, floor: &str, out: &mut Record) -> (f64, f64) {
    build(sim, [-2, 3, -2], [62, 3, 2], floor);
    let body = walker(sim, 0.5, 4.0, 0.5);
    intent(sim, body, |i| i.wish = Vec2::X);
    for _ in 0..560 {
        sim.steps(1);
        record(sim, body, out);
    }
    let top = sim.get::<Velocity>(body).0.x;
    intent(sim, body, |i| i.wish = Vec2::ZERO);
    let from = feet(sim, body).x;
    for _ in 0..320 {
        sim.steps(1);
        record(sim, body, out);
    }
    let slid = feet(sim, body).x - from;
    sim.app.world_mut().despawn(body);
    build(sim, [-2, 3, -2], [62, 3, 2], "boxel:grass");
    (top, slid)
}

fn ground_scenario(sim: &mut Sim, out: &mut Record) {
    let walk = CharacterController::default().walk_speed;
    let (grass_top, grass_slid) = walk_and_slide(sim, "boxel:grass", out);
    let (ice_top, ice_slid) = walk_and_slide(sim, "test:ice", out);
    let (slow_top, _) = walk_and_slide(sim, "test:slow", out);
    assert!((grass_top - walk).abs() < 1e-6, "{grass_top}");
    // Slippery: the same top speed, a long slide (12 against 2.19 /s of friction).
    assert!((ice_top - walk).abs() < 1e-6, "{ice_top}");
    assert!(ice_slid > 4.0 * grass_slid, "{ice_slid} {grass_slid}");
    // Slow: 12 / (12 + 8.66) of the walking speed (Minecraft's 58%).
    let slow = walk * 12.0 / (12.0 + SLOW_DRAG);
    assert!((slow_top - slow).abs() < 1e-6, "{slow_top} {slow}");
    // A jump off the slow block is half as fast: about a third of the height.
    let peak = |sim: &mut Sim, floor: &str, out: &mut Record| {
        build(sim, [-1, 3, -1], [1, 3, 1], floor);
        let body = walker(sim, 0.5, 4.0, 0.5);
        sim.steps(4);
        intent(sim, body, |i| i.jump = true);
        let mut top = 0.0f64;
        for _ in 0..64 {
            sim.steps(1);
            top = top.max(feet(sim, body).y - 4.0);
            record(sim, body, out);
        }
        sim.app.world_mut().despawn(body);
        build(sim, [-1, 3, -1], [1, 3, 1], "boxel:grass");
        top
    };
    let high = peak(sim, "boxel:grass", out);
    let low = peak(sim, "test:slow", out);
    assert!(high > 1.2 && low > 0.25 && low < 0.45, "{high} {low}");
}

fn web_scenario(sim: &mut Sim, out: &mut Record) {
    build(sim, [0, 4, -1], [8, 4, 1], "test:web");
    // Dropped into the web: it falls at gravity / (0.4 + 407.76), and the fall ends.
    let item = sim.body(
        sim.pos(6.5, 12.0, 0.5),
        (Collider::aabb(DVec3::splat(0.125)), BodySettings::item()),
    );
    let body = walker(sim, 0.5, 10.0, 0.5);
    for _ in 0..96 {
        sim.steps(1);
        record(sim, body, out);
        record(sim, item, out);
    }
    let v = sim.get::<Velocity>(body).0.y;
    assert!(sim.get::<Contacts>(body).surface.stuck());
    assert!((v + 32.0 / (0.4 + WEB.vertical)).abs() < 1e-3, "{v}");
    assert!(
        sim.get::<Velocity>(item).0.y.abs() < 0.05,
        "the item is held back too"
    );
    // Down to the ground, then walking in the web: 12 / (12 + 93.73) of the walking speed.
    for _ in 0..2048 {
        sim.steps(1);
        if sim.get::<Contacts>(body).on_ground {
            break;
        }
    }
    intent(sim, body, |i| i.wish = Vec2::X);
    for _ in 0..64 {
        sim.steps(1);
        record(sim, body, out);
    }
    let walk = CharacterController::default().walk_speed * 12.0 / (12.0 + WEB.across);
    let vx = sim.get::<Velocity>(body).0.x;
    assert!((vx - walk).abs() < 1e-3, "{vx} {walk}");
    sim.app.world_mut().despawn(body);
    sim.app.world_mut().despawn(item);
    build(sim, [0, 4, -1], [8, 4, 1], "boxel:air");
}

fn climb_scenario(sim: &mut Sim, out: &mut Record) {
    // A ladder on a wall (its box on the +Z side of its cells).
    build(sim, [0, 4, 1], [0, 12, 1], "boxel:stone");
    build(sim, [0, 4, 0], [0, 11, 0], "test:ladder");
    let body = walker(sim, 0.5, 4.0, 0.5);
    intent(sim, body, |i| i.wish = Vec2::Y);
    let mut ys = Vec::new();
    for _ in 0..96 {
        sim.steps(1);
        record(sim, body, out);
        ys.push(feet(sim, body).y);
    }
    assert!(sim.get::<Contacts>(body).surface.climbing());
    // Pushing into the wall climbs at 2.352 blocks/s.
    let rise = ys[80] - ys[79];
    assert!((rise - 2.352 * DT).abs() < 1e-9, "{rise}");
    // Sneaking holds on.
    intent(sim, body, |i| {
        i.wish = Vec2::ZERO;
        i.sneak = true;
    });
    sim.steps(8);
    let held = feet(sim, body).y;
    for _ in 0..32 {
        sim.steps(1);
        record(sim, body, out);
    }
    assert_eq!(feet(sim, body).y, held, "held on");
    // Let go: it slides down at 3 blocks/s at most.
    intent(sim, body, |i| i.sneak = false);
    let mut fastest = 0.0f64;
    for _ in 0..64 {
        let y = feet(sim, body).y;
        sim.steps(1);
        record(sim, body, out);
        fastest = fastest.max(y - feet(sim, body).y);
    }
    assert!(fastest <= 3.0 * DT + 1e-12, "{fastest}");
    assert!(fastest > 2.9 * DT, "{fastest}");
    sim.app.world_mut().despawn(body);
    build(sim, [0, 4, 0], [0, 12, 1], "boxel:air");

    // A vine with nothing behind it: holding up climbs.
    build(sim, [3, 4, 0], [3, 9, 0], "test:vine");
    let body = walker(sim, 3.5, 4.0, 0.5);
    intent(sim, body, |i| i.up = true);
    for _ in 0..64 {
        sim.steps(1);
        record(sim, body, out);
    }
    assert!(feet(sim, body).y > 6.0, "{}", feet(sim, body).y);
    sim.app.world_mut().despawn(body);
    build(sim, [3, 4, 0], [3, 9, 0], "boxel:air");
}

fn scaffold_scenario(sim: &mut Sim, out: &mut Record) {
    build(sim, [0, 4, 0], [0, 6, 0], "test:scaffold");
    // On top: it stands.
    let body = walker(sim, 0.5, 7.25, 0.5);
    for _ in 0..32 {
        sim.steps(1);
        record(sim, body, out);
    }
    assert_eq!(feet(sim, body).y, 7.0);
    assert!(sim.get::<Contacts>(body).on_ground);
    // Sneaking goes down through it, at 3 blocks/s at most, to the ground.
    intent(sim, body, |i| i.sneak = true);
    for _ in 0..128 {
        let y = feet(sim, body).y;
        sim.steps(1);
        record(sim, body, out);
        assert!(y - feet(sim, body).y <= 3.0 * DT + 1e-12);
    }
    assert_eq!(feet(sim, body).y, 4.0);
    // Holding up climbs; let go above a cell's top and it stands on it.
    intent(sim, body, |i| {
        i.sneak = false;
        i.up = true;
    });
    for _ in 0..40 {
        sim.steps(1);
        record(sim, body, out);
    }
    intent(sim, body, |i| i.up = false);
    for _ in 0..64 {
        sim.steps(1);
        record(sim, body, out);
    }
    let y = feet(sim, body).y;
    assert!(y > 4.5 && y.fract() == 0.0, "{y}");
    sim.app.world_mut().despawn(body);
    // From the side it walks in.
    let body = walker(sim, -1.5, 4.0, 0.5);
    intent(sim, body, |i| i.wish = Vec2::X);
    for _ in 0..48 {
        sim.steps(1);
        record(sim, body, out);
    }
    assert!(feet(sim, body).x > 0.5, "{}", feet(sim, body).x);
    sim.app.world_mut().despawn(body);
    build(sim, [0, 4, 0], [0, 6, 0], "boxel:air");
}

fn snow_scenario(sim: &mut Sim, out: &mut Record) {
    build(sim, [-1, 4, -1], [1, 5, 3], "test:snow");
    // Without boots it sinks, slowly, to the ground.
    let bare = walker(sim, 0.5, 6.0, 0.5);
    // With boots it stands on top.
    let booted = sim.spawn(
        sim.pos(0.5, 6.0, 2.5),
        (CharacterController::default(), Treads(BOOTS)),
    );
    for _ in 0..128 {
        sim.steps(1);
        record(sim, bare, out);
        record(sim, booted, out);
    }
    assert_eq!(feet(sim, bare).y, 4.0, "sank to the ground");
    assert_eq!(feet(sim, booted).y, 6.0, "stands on it");
    assert!(sim.get::<Contacts>(bare).surface.stuck());
    // Booted and sneaking, it goes in.
    intent(sim, booted, |i| i.sneak = true);
    for _ in 0..96 {
        sim.steps(1);
        record(sim, booted, out);
    }
    assert_eq!(feet(sim, booted).y, 4.0, "sneaked in");
    // Booted, holding up, it climbs out and stands on top again.
    intent(sim, booted, |i| {
        i.sneak = false;
        i.up = true;
    });
    for _ in 0..64 {
        sim.steps(1);
        record(sim, booted, out);
    }
    intent(sim, booted, |i| i.up = false);
    for _ in 0..64 {
        sim.steps(1);
        record(sim, booted, out);
    }
    assert_eq!(feet(sim, booted).y, 6.0, "climbed out");
    // Bare, holding up, it does not.
    intent(sim, bare, |i| i.up = true);
    sim.steps(64);
    assert!(feet(sim, bare).y < 5.0, "{}", feet(sim, bare).y);
    sim.app.world_mut().despawn(bare);
    sim.app.world_mut().despawn(booted);
    build(sim, [-1, 4, -1], [1, 5, 3], "boxel:air");
}

fn honey_scenario(sim: &mut Sim, out: &mut Record) {
    build(sim, [0, 4, 0], [0, 7, 0], "test:honey");
    // Falling against its +X side: it slides at 2.548 blocks/s.
    let body = walker(sim, 1.0 + 0.3 - 0.0625, 12.0, 0.5);
    let mut slid = false;
    for _ in 0..160 {
        let y = feet(sim, body).y;
        sim.steps(1);
        record(sim, body, out);
        let fell = y - feet(sim, body).y;
        let contacts = sim.get::<Contacts>(body);
        if contacts.surface.sliding() && !contacts.on_ground {
            slid = true;
            assert!((fell - SLIDE * DT).abs() < 1e-9, "{fell}");
        }
    }
    assert!(slid, "it slid");
    assert_eq!(feet(sim, body).y, 4.0);
    sim.app.world_mut().despawn(body);
    build(sim, [0, 4, 0], [0, 7, 0], "boxel:air");
}

fn bounce_scenario(sim: &mut Sim, out: &mut Record) {
    build(sim, [-1, 3, -1], [1, 3, 1], "test:bouncy");
    let body = walker(sim, 0.5, 9.0, 0.5);
    let mut up = 0.0f64;
    for _ in 0..64 {
        sim.steps(1);
        record(sim, body, out);
        up = up.max(sim.get::<Velocity>(body).0.y);
    }
    assert!(up > 10.0, "it bounced: {up}");
    sim.app.world_mut().despawn(body);
    // Sneaking, it lands.
    let body = walker(sim, 0.5, 9.0, 0.5);
    intent(sim, body, |i| i.sneak = true);
    let mut up = 0.0f64;
    for _ in 0..64 {
        sim.steps(1);
        record(sim, body, out);
        up = up.max(sim.get::<Velocity>(body).0.y);
    }
    assert_eq!(up, 0.0);
    sim.app.world_mut().despawn(body);
    build(sim, [-1, 3, -1], [1, 3, 1], "boxel:grass");
}

fn run(base: I64Vec3) -> Record {
    let mut sim = sim(base);
    let mut out = Record::new();
    ground_scenario(&mut sim, &mut out);
    web_scenario(&mut sim, &mut out);
    climb_scenario(&mut sim, &mut out);
    scaffold_scenario(&mut sim, &mut out);
    snow_scenario(&mut sim, &mut out);
    honey_scenario(&mut sim, &mut out);
    bounce_scenario(&mut sim, &mut out);
    out
}

#[test]
fn surfaces_act_on_bodies_the_same_far_from_the_origin() {
    let near = run(I64Vec3::ZERO);
    for base in [
        I64Vec3::new(1 << 31, 0, -(1 << 31)),
        I64Vec3::new(1 << 40, -(1 << 31), 1 << 40),
        I64Vec3::new(
            MAX_BLOCK_COORD - 1024,
            -MAX_BLOCK_COORD + 1024,
            MAX_BLOCK_COORD - 1024,
        ),
    ] {
        assert!(run(base) == near, "base {base}");
    }
}

#[test]
fn a_body_without_surfaces_steps_as_before() {
    // Away from the test blocks, a walker steps as in a world without surfaces.
    let walk = |surfaces: bool| {
        let mut sim = if surfaces {
            Sim::with_registry(I64Vec3::ZERO, STEP, registry(), with_surfaces)
        } else {
            Sim::with_registry(I64Vec3::ZERO, STEP, registry(), ())
        };
        let body = walker(&mut sim, 0.5, 6.0, 0.5);
        intent(&mut sim, body, |i| {
            i.wish = Vec2::new(0.6, 0.8);
            i.jump = true;
        });
        let mut out = Record::new();
        for _ in 0..128 {
            sim.steps(1);
            record(&sim, body, &mut out);
        }
        out
    };
    assert!(walk(true) == walk(false));
}
