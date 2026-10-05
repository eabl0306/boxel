//! Every derive a user writes, expanded through `::boxel::ecs` (contract test, owner WP0).
use boxel::ecs::query::{QueryData, QueryFilter};
use boxel::ecs::schedule::ScheduleLabel;
use boxel::ecs::system::SystemParam;
use boxel::prelude::*;

#[derive(Component, Default, Debug, Clone, Copy)]
struct Pos(f32);
#[derive(Component, Default)]
#[component(storage = "SparseSet")]
struct Tag;
#[derive(Component, Default)]
#[require(Pos)]
struct Req;
#[derive(Resource, Default)]
struct Counter(u32);
#[derive(Message, Clone, Debug)]
struct Hit;
#[derive(Event)]
struct Boom;
#[derive(EntityEvent)]
struct Poke {
    entity: Entity,
}
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
struct MySet;
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
struct MySchedule;
#[derive(Bundle)]
struct PosTag {
    p: Pos,
    t: Tag,
}
#[derive(SystemParam)]
struct Param<'w, 's> {
    c: Res<'w, Counter>,
    q: Query<'w, 's, &'static Pos>,
}
#[derive(QueryData)]
#[query_data(mutable)]
struct MovePos {
    p: &'static mut Pos,
}
#[derive(QueryFilter)]
struct HasPos {
    _w: With<Pos>,
}
#[derive(FromWorld)]
struct Built {
    c: u32,
}

fn uses_param(p: Param) {
    let _ = (p.c.0, p.q.iter().count());
}
fn uses_query_data(mut q: Query<MovePos, HasPos>) {
    for mut x in &mut q {
        x.p.0 += 1.0;
    }
}
// ParamSet needs explicit 'static lifetimes outside a crate named bevy_ecs (E0477 otherwise).
fn param_set(mut ps: ParamSet<(Query<&'static mut Pos>, Query<&'static Pos>)>) {
    for mut p in &mut ps.p0() {
        p.0 += 1.0;
    }
    let _ = ps.p1().iter().count();
}

#[test]
fn derives_resolve_through_the_facade() {
    let mut app = App::new();
    app.init_resource::<Counter>()
        .add_message::<Hit>()
        .add_systems(
            Update,
            (uses_param, uses_query_data, param_set)
                .chain()
                .in_set(MySet),
        );
    let e = app
        .world_mut()
        .spawn((
            PosTag {
                p: Pos(1.0),
                t: Tag,
            },
            Req,
        ))
        .id();
    app.world_mut().add_observer(|_e: On<Boom>| {});
    app.world_mut().trigger(Boom);
    app.world_mut().trigger(Poke { entity: e });
    app.add_schedule(Schedule::new(MySchedule));
    app.update();
    let built = Built::from_world(app.world_mut());
    assert_eq!(built.c, 0);
    assert!((app.world().get::<Pos>(e).expect("pos").0 - 3.0).abs() < 1e-6);
}
