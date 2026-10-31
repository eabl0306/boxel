//! Pushes (ADR 0057): cells that move a body's vertical speed toward a limit, once per cell its
//! box overlaps, with a stronger push under an empty cell (Minecraft's bubble column). The scan
//! and the lifts, a per-state surface, then bodies in an `App`: pushed up and out of a column,
//! held to the limits, pulled down, a flyer left alone, and everything bit for bit the same far
//! from the origin and at the world's edge.
mod common;
mod sim;

use boxel_app::App;
use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    BlockSurfaces, CharacterController, CollisionTable, Contacts, Lift, MAX_PUSHES, MoveIntent,
    Push, Surface, SwimIntent, SwimPlugin, Swimmer, TestWorld, Velocity, sense_inside,
};
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, FaceTextures, MAX_BLOCK_COORD,
    Property, RenderShape, TextureId, WATERLOGGED, WorldPos,
};
use boxel_world::ChunkMap;
use common::{STONE, WATER, at, fill, player};
use sim::{STEP, Sim};

const DT: f64 = 1.0 / 64.0;

// Minecraft's bubble column in the engine's units (ADR 0057): per tick 0.06 up to 0.7 inside,
// 0.1 up to 1.8 under air; down 0.03 to 0.3 inside, 0.03 to 0.9 under air. A tick's change of
// `d` blocks per tick is `400 d` blocks per second squared; `l` blocks per tick is `20 l` per
// second.
const UP: Push = Push {
    inside: Lift {
        accel: 24.0,
        limit: 14.0,
    },
    top: Lift {
        accel: 40.0,
        limit: 36.0,
    },
};
const DOWN: Push = Push {
    inside: Lift {
        accel: -12.0,
        limit: -6.0,
    },
    top: Lift {
        accel: -12.0,
        limit: -18.0,
    },
};

/// The demo blocks plus `test:column` (no collision, `drag` false or true, always holding water
/// as a bubble column does).
fn registry() -> BlockRegistry {
    let mut b = boxel_demo::blocks::builder();
    b.add_block_with_states(
        BlockDef {
            name: "test:column".to_string(),
            // Cutout: a cube that holds water (ADR 0038).
            shape: RenderShape::Cutout,
            model: BlockModel::CUBE,
            collision: BlockCollision::None,
            emission: [0; 3],
            light_opacity: 0,
            textures: FaceTextures::all(TextureId(0)),
        },
        vec![
            Property::boolean("drag"),
            Property::new(WATERLOGGED, ["true"]),
        ],
        |_, _| {},
    )
    .expect("column");
    b.build()
}

/// The up push on every column state, the down one on `drag=true` only (set later: it wins).
fn surfaces() -> BlockSurfaces {
    let mut s = BlockSurfaces::default();
    s.set(
        "test:column",
        Surface {
            push: Some(UP),
            ..Surface::PLAIN
        },
    )
    .set(
        "test:column[drag=true]",
        Surface {
            push: Some(DOWN),
            ..Surface::PLAIN
        },
    );
    s
}

fn column(registry: &BlockRegistry, drag: bool) -> BlockState {
    registry
        .parse_state(&format!("test:column[drag={drag}]"))
        .expect("column state")
}

fn table(registry: &BlockRegistry) -> CollisionTable {
    let mut t = CollisionTable::from_registry(registry);
    assert!(surfaces().apply(registry, &mut t).is_empty());
    t
}

// ------------------------------------------------------------------------------ the table

#[test]
fn a_state_surface_sets_only_the_states_it_names() {
    let registry = registry();
    let t = table(&registry);
    let (up, down) = (column(&registry, false), column(&registry, true));
    assert_eq!(t.surface(up).push, Some(UP));
    assert_eq!(t.surface(down).push, Some(DOWN));
    assert!(t.acts_inside(up) && t.acts_inside(down));
    assert_eq!(*t.surface(STONE), Surface::PLAIN);
    // Unknown properties, values and broken names are reported, the rest applied.
    let mut more = surfaces();
    more.set("test:column[tilt=true]", Surface::PLAIN)
        .set("test:column[drag=maybe]", Surface::PLAIN)
        .set("test:column[drag]", Surface::PLAIN)
        .set("test:column]", Surface::PLAIN);
    let mut t = CollisionTable::from_registry(&registry);
    assert_eq!(
        more.apply(&registry, &mut t),
        vec![
            "test:column[tilt=true]",
            "test:column[drag=maybe]",
            "test:column[drag]",
            "test:column]"
        ]
    );
    assert_eq!(t.surface(down).push, Some(DOWN));
}

#[test]
fn a_lift_moves_the_speed_toward_its_limit() {
    let up = UP.inside;
    assert_eq!(up.apply(0.0, DT), 24.0 * DT);
    assert_eq!(up.apply(13.9, DT), 14.0, "stops at the limit");
    assert_eq!(up.apply(30.0, DT), 14.0, "a faster body is brought back");
    let down = DOWN.inside;
    assert_eq!(down.apply(0.0, DT), -12.0 * DT);
    assert_eq!(down.apply(-20.0, DT), -6.0);
    assert_eq!(Lift::NONE.apply(-3.0, DT), -3.0);
    // NaN and infinities are cleaned.
    let s = Surface {
        push: Some(Push {
            inside: Lift {
                accel: f64::NAN,
                limit: f64::INFINITY,
            },
            top: UP.top,
        }),
        ..Surface::PLAIN
    }
    .sanitized();
    assert_eq!(s.push.map(|p| p.inside), Some(Lift::NONE));
}

#[test]
fn the_scan_counts_each_push_cell_and_pushes_from_the_top_under_air() {
    let registry = registry();
    let t = table(&registry);
    let base = I64Vec3::new(-(1 << 40), 5, 1 << 31);
    let up = column(&registry, false);
    let mut world = TestWorld::new();
    // A column y 0..=3 with water over it at x 0, under air at x 2, under stone at x 4.
    for x in [0, 2, 4] {
        fill(&mut world, base, [x, 0, 0], [x, 3, 0], up);
    }
    fill(&mut world, base, [0, 4, 0], [0, 4, 0], WATER);
    fill(&mut world, base, [4, 4, 0], [4, 4, 0], STONE);
    let scan = |x: i64, y: f64| {
        let pos = WorldPos::from_block(at(base, x, 0, 0), DVec3::new(0.5, y, 0.5));
        let mut src = &world;
        sense_inside(&mut src, &t, pos, player(), 1e-7, false, None)
    };
    // Low in the column: three cells inside.
    let low = scan(0, 0.5);
    assert!(low.pushed());
    assert_eq!(low.pushes[0], (UP.inside, 3));
    assert_eq!(low.pushes[1].1, 0);
    // At the top under water: still inside. Under air: two inside, then the top one.
    assert_eq!(scan(0, 2.5).pushes[0], (UP.inside, 2));
    let top = scan(2, 2.5);
    assert_eq!(top.pushes[..2], [(UP.inside, 1), (UP.top, 1)]);
    assert_eq!(scan(4, 2.5).pushes[0], (UP.inside, 2), "under stone");
    // Applied in order, once per cell: inside first, then the top's.
    let v = top.push(0.0, DT);
    assert_eq!(v, UP.top.apply(UP.inside.apply(0.0, DT), DT));
    // Out of the column: nothing.
    assert!(!scan(0, 5.5).pushed());
    assert_eq!(MAX_PUSHES, 4);
}

// ------------------------------------------------------------------------------ bodies

fn with_surfaces(app: &mut App) {
    app.insert_resource(surfaces());
}

fn sim(base: I64Vec3) -> Sim {
    Sim::with_registry(base, STEP, registry(), with_surfaces)
}

/// Places `state` in every block of `min..=max` (relative to the base) and waits for the edits.
fn build(sim: &mut Sim, min: [i64; 3], max: [i64; 3], state: BlockState) {
    for x in min[0]..=max[0] {
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                sim.edit(sim.block(x, y, z), state);
            }
        }
    }
    let last = sim.block(max[0], max[1], max[2]);
    while sim.app.world().resource::<ChunkMap>().block_at(last) != Some(state) {
        sim.frame(STEP);
    }
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

/// A column up y 4..=11 under air: a walker at its bottom rises at most 14 blocks/s while no
/// cell of its box is the top one, out of it under the top's push, and falls back in.
fn up_scenario(sim: &mut Sim, out: &mut Record) {
    let up = column(&registry(), false);
    build(sim, [0, 4, 0], [0, 11, 0], up);
    let body = sim.spawn(sim.pos(0.5, 4.0, 0.5), CharacterController::default());
    let (mut highest, mut fastest_inside) = (0.0f64, 0.0f64);
    let mut pushed = false;
    for _ in 0..256 {
        // The pushes of a step come from the cells the box ended the previous one in.
        let before = feet(sim, body).y;
        sim.steps(1);
        record(sim, body, out);
        let (y, v) = (feet(sim, body).y, sim.get::<Velocity>(body).0.y);
        highest = highest.max(y);
        pushed |= sim.get::<Contacts>(body).surface.pushed();
        // Whole inside cells: the box under the top cell (y 11).
        if before + 1.8 < 11.0 {
            fastest_inside = fastest_inside.max(v);
        }
        assert!(v <= UP.top.limit, "{v}");
    }
    assert!(pushed);
    assert!(!sim.get::<Contacts>(body).surface.pulled());
    assert!(fastest_inside <= UP.inside.limit, "{fastest_inside}");
    assert!(fastest_inside > 10.0, "{fastest_inside}");
    // Out over the top of the column (y 12), and back into it: it bobs there.
    assert!(highest > 12.0, "{highest}");
    let y = feet(sim, body).y;
    assert!(y > 4.0 && y < highest, "{y}");
    sim.app.world_mut().despawn(body);
    build(sim, [0, 4, 0], [0, 11, 0], BlockState::AIR);
}

/// A column down y 4..=11 under air: a walker dropped from high above is pulled down at 6
/// blocks/s inside it, and the fall ends.
fn down_scenario(sim: &mut Sim, out: &mut Record) {
    let down = column(&registry(), true);
    build(sim, [3, 4, 0], [3, 11, 0], down);
    let body = sim.spawn(sim.pos(3.5, 30.0, 0.5), CharacterController::default());
    let mut pushed = false;
    for _ in 0..256 {
        let before = feet(sim, body).y;
        sim.steps(1);
        record(sim, body, out);
        let (y, v) = (feet(sim, body).y, sim.get::<Velocity>(body).0.y);
        let contacts = sim.get::<Contacts>(body);
        pushed |= contacts.surface.pushed();
        if before + 1.8 < 11.0 && y > 4.0 && contacts.surface.pushed() {
            assert!(v >= DOWN.inside.limit, "{v}");
        }
    }
    assert!(pushed);
    assert_eq!(feet(sim, body).y, 4.0, "down on the ground");
    assert!(sim.get::<Contacts>(body).surface.pulled());
    // A flyer in the column is not pushed.
    let flyer = sim.spawn(
        sim.pos(3.5, 6.0, 0.5),
        CharacterController {
            flying: true,
            ..CharacterController::default()
        },
    );
    for _ in 0..32 {
        sim.steps(1);
        record(sim, flyer, out);
    }
    assert_eq!(feet(sim, flyer).y, 6.0);
    assert!(!sim.get::<Contacts>(flyer).surface.pushed());
    sim.app.world_mut().despawn(body);
    sim.app.world_mut().despawn(flyer);
    build(sim, [3, 4, 0], [3, 11, 0], BlockState::AIR);
}

fn run(base: I64Vec3) -> Record {
    let mut sim = sim(base);
    let mut out = Record::new();
    up_scenario(&mut sim, &mut out);
    down_scenario(&mut sim, &mut out);
    out
}

#[test]
fn pushes_act_on_bodies_the_same_far_from_the_origin() {
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

/// A swimmer holding up in a column down sinks to its bottom (the pull wins, ADR 0057), and in a
/// column up rises out of it.
#[test]
fn a_swimmer_pulled_down_does_not_swim_up() {
    let registry = registry();
    let mut sim = Sim::with_registry(
        I64Vec3::ZERO,
        STEP,
        registry.clone(),
        (with_surfaces, SwimPlugin),
    );
    build(&mut sim, [0, 4, 0], [0, 11, 0], column(&registry, true));
    build(&mut sim, [3, 4, 0], [3, 11, 0], column(&registry, false));
    let swimmer = |sim: &mut Sim, x: f64| {
        sim.spawn(
            sim.pos(x, 7.0, 0.5),
            (
                Swimmer::default(),
                CharacterController::default(),
                MoveIntent::default(),
                SwimIntent { up: true },
            ),
        )
    };
    let (down, up) = (swimmer(&mut sim, 0.5), swimmer(&mut sim, 3.5));
    let mut highest = 0.0f64;
    for _ in 0..192 {
        sim.steps(1);
        highest = highest.max(feet(&sim, up).y);
    }
    assert!(sim.get::<Swimmer>(down).in_fluid);
    assert_eq!(feet(&sim, down).y, 4.0, "pulled to the bottom");
    assert!(highest > 12.0, "{highest}");
}
