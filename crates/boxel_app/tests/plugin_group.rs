//! `PluginGroupBuilder::{add_before, add_after}`: ordering, moves, replacement and panics.
use boxel_app::prelude::*;
use boxel_ecs::prelude::*;

/// Build order of the plugins (each plugin pushes its id in `build`).
#[derive(Resource, Default)]
struct Order(Vec<u32>);

fn push(app: &mut App, id: u32) {
    app.world_mut().get_resource_or_init::<Order>().0.push(id);
}

/// Distinct plugin types `P<1>`, `P<2>`, ... that record `N`.
struct P<const N: u32>;
impl<const N: u32> Plugin for P<N> {
    fn build(&self, app: &mut App) {
        push(app, N);
    }
}

/// A plugin with a payload, to see which instance survives a move (records `100 + payload`).
struct V(u32);
impl Plugin for V {
    fn build(&self, app: &mut App) {
        push(app, 100 + self.0);
    }
}

fn build_order(group: PluginGroupBuilder) -> Vec<u32> {
    let mut app = App::empty();
    app.add_plugins(group);
    app.world()
        .get_resource::<Order>()
        .map(|o| o.0.clone())
        .unwrap_or_default()
}

fn one_two_three() -> PluginGroupBuilder {
    PluginGroupBuilder::start()
        .add(P::<1>)
        .add(P::<2>)
        .add(P::<3>)
}

#[test]
fn new_plugins_are_inserted_relative_to_the_target() {
    let group = one_two_three()
        .add_before::<P<2>, _>(P::<4>)
        .add_after::<P<3>, _>(P::<5>)
        .add_after::<P<1>, _>(P::<6>)
        .add_before::<P<1>, _>(P::<7>);
    assert_eq!(build_order(group), [7, 1, 6, 4, 2, 3, 5]);
}

#[test]
fn existing_plugins_are_moved() {
    let cases: [(PluginGroupBuilder, [u32; 3]); 6] = [
        (one_two_three().add_after::<P<3>, _>(P::<1>), [2, 3, 1]),
        (one_two_three().add_before::<P<1>, _>(P::<3>), [3, 1, 2]),
        (one_two_three().add_before::<P<3>, _>(P::<1>), [2, 1, 3]),
        (one_two_three().add_after::<P<1>, _>(P::<3>), [1, 3, 2]),
        (one_two_three().add_after::<P<2>, _>(P::<3>), [1, 2, 3]),
        (one_two_three().add_before::<P<2>, _>(P::<1>), [1, 2, 3]),
    ];
    for (i, (group, expected)) in cases.into_iter().enumerate() {
        assert_eq!(build_order(group), expected, "case {i}");
    }
}

#[test]
fn a_moved_plugin_is_the_new_instance() {
    let group = PluginGroupBuilder::start()
        .add(P::<1>)
        .add(V(1))
        .add(P::<2>)
        .add_after::<P<2>, _>(V(7));
    assert_eq!(build_order(group), [1, 2, 107]);
}

#[test]
fn target_equal_to_the_plugin_replaces_in_place() {
    let before = PluginGroupBuilder::start()
        .add(P::<1>)
        .add(V(1))
        .add(P::<2>)
        .add_before::<V, _>(V(8));
    assert_eq!(build_order(before), [1, 108, 2]);
    let after = PluginGroupBuilder::start()
        .add(P::<1>)
        .add(V(1))
        .add(P::<2>)
        .add_after::<V, _>(V(9));
    assert_eq!(build_order(after), [1, 109, 2]);
}

#[test]
fn a_moved_plugin_keeps_its_enabled_state() {
    let disabled = one_two_three()
        .disable::<P<2>>()
        .add_before::<P<1>, _>(P::<2>);
    assert_eq!(build_order(disabled), [1, 3]);
    let reenabled = one_two_three()
        .disable::<P<2>>()
        .add_before::<P<1>, _>(P::<2>)
        .enable::<P<2>>();
    assert_eq!(build_order(reenabled), [2, 1, 3]);
}

#[test]
fn groups_with_relative_inserts_work_through_the_trait() {
    struct Group;
    impl PluginGroup for Group {
        fn build(self) -> PluginGroupBuilder {
            PluginGroupBuilder::start().add(P::<1>).add(P::<2>)
        }
    }
    let mut app = App::empty();
    app.add_plugins(Group.build().add_after::<P<1>, _>(P::<3>));
    assert_eq!(app.world().resource::<Order>().0, [1, 3, 2]);
}

#[test]
#[should_panic(expected = "P<2> is not in this plugin group")]
fn add_before_panics_without_the_target() {
    let _ = PluginGroupBuilder::start()
        .add(P::<1>)
        .add_before::<P<2>, _>(P::<3>);
}

#[test]
#[should_panic(expected = "P<9> is not in this plugin group")]
fn add_after_panics_without_the_target() {
    let _ = PluginGroupBuilder::start()
        .add(P::<1>)
        .add_after::<P<9>, _>(P::<1>);
}
