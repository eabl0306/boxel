//! The full-bright flag (ADR 0036, [`BlockRegistryBuilder::set_full_bright`]): off by default,
//! set per state, fixed for fluids by their emission, and kept out of every other per-state
//! table. The flag that keeps flowing fluids out ([`StateFlags::KEEPS_FLUIDS_OUT`], #110): on
//! every waterloggable state, and on every state of a declared block. On the `test:` blocks
//! ([`crate::testing`]) and a few more after them.
use super::*;
use crate::testing;

/// The emission of `test:ember` and of `test:ember_ore[lit=true]`: a weak, warm light.
const EMBER: [u8; 3] = [3, 1, 0];

fn by(r: &BlockRegistry, name: &str) -> BlockState {
    r.parse_state(name).expect(name)
}

fn block(b: &BlockRegistryBuilder, name: &str) -> BlockId {
    b.block_by_name(name).expect(name)
}

/// `testing::builder()` plus `test:ember` (an opaque cube that emits [`EMBER`], with
/// `test:solid`'s layers), `test:ember_ore` (the same, emitting only when `lit=true`) and
/// `test:paint` (a cube that emits nothing): none of them marked.
fn builder() -> BlockRegistryBuilder {
    let mut b = testing::builder();
    let solid = b.by_name("test:solid").expect("test:solid");
    let solid = b.states[usize::from(solid.0)].clone();
    let def = |name: &str, emission| BlockDef {
        name: name.to_string(),
        emission,
        ..solid.clone()
    };
    b.add_block(def("test:ember", EMBER)).expect("test:ember");
    b.add_block_with_states(
        def("test:ember_ore", [0; 3]),
        vec![Property::boolean("lit")],
        |values, def| {
            if values.get("lit") == Some("true") {
                def.emission = EMBER;
            }
        },
    )
    .expect("test:ember_ore");
    b.add_block(def("test:paint", [0; 3])).expect("test:paint");
    b
}

/// Every state of `r`.
fn states(r: &BlockRegistry) -> impl Iterator<Item = BlockState> {
    (0..r.len() as u16).map(BlockState)
}

#[test]
fn full_bright_is_off_by_default_and_lives_outside_the_flags() {
    let plain = builder().build();
    let ember = by(&plain, "test:ember");
    assert!(!plain.emission(ember).is_dark());
    assert!(plain.flags(ember).contains(StateFlags::EMISSIVE));
    assert!(!plain.is_full_bright(ember));
    assert!(!plain.is_full_bright(by(&plain, "test:solid")));
    assert!(!plain.is_full_bright(BlockState::AIR));
    assert!(!plain.is_full_bright(BlockState(u16::MAX)));
    // Marking blocks changes no flag, emission or material of any state.
    let mut b = builder();
    for name in ["test:ember", "test:ember_ore", "test:paint", "test:solid"] {
        b.set_full_bright(block(&b, name), |_| true).expect(name);
    }
    let marked = b.build();
    assert_eq!(marked.len(), plain.len());
    for s in states(&plain) {
        assert_eq!(marked.flags(s), plain.flags(s), "{s:?}");
        assert_eq!(marked.emission(s), plain.emission(s), "{s:?}");
        assert_eq!(marked.material(s), plain.material(s), "{s:?}");
    }
    assert!(marked.is_full_bright(ember));
}

#[test]
fn full_bright_is_set_per_state_and_a_later_call_replaces() {
    let mut b = builder();
    let ore = block(&b, "test:ember_ore");
    b.set_full_bright(ore, |v| v.get("lit") == Some("true"))
        .expect("test:ember_ore");
    let r = b.build();
    let (lit, unlit) = (
        by(&r, "test:ember_ore[lit=true]"),
        by(&r, "test:ember_ore[lit=false]"),
    );
    assert!(r.is_full_bright(lit) && !r.is_full_bright(unlit));
    assert!(!r.emission(lit).is_dark() && r.emission(unlit).is_dark());
    // Of the unmarked blocks, only the lit ore (the test set marks its lamp, and its hot fluid
    // emits).
    let marked: Vec<BlockState> = states(&r)
        .filter(|&s| r.is_full_bright(s) && !["test:lamp", "test:hot"].contains(&r.name(s)))
        .collect();
    assert_eq!(marked, [lit]);

    let mut b = builder();
    let ore = block(&b, "test:ember_ore");
    b.set_full_bright(ore, |_| true).expect("test:ember_ore");
    b.set_full_bright(ore, |_| false).expect("test:ember_ore");
    let r = b.build();
    assert!(!r.is_full_bright(by(&r, "test:ember_ore[lit=true]")));
    assert!(!r.is_full_bright(by(&r, "test:ember_ore[lit=false]")));
}

#[test]
fn a_fluid_is_full_bright_exactly_when_it_emits() {
    let r = builder().build();
    for level in 0..16 {
        let hot = by(&r, &format!("test:hot[level={level}]"));
        assert!(!r.emission(hot).is_dark(), "level {level}");
        assert!(r.is_full_bright(hot), "test:hot level {level}");
        let water = by(&r, &format!("test:fluid[level={level}]"));
        assert!(!r.is_full_bright(water), "test:fluid level {level}");
    }
    // A fluid cannot be marked; the failed call sets nothing.
    let mut b = builder();
    b.set_full_bright(block(&b, "test:ember"), |_| true)
        .expect("test:ember");
    let before = b.full_bright.clone();
    for name in ["test:hot", "test:fluid"] {
        let fluid = block(&b, name);
        assert_eq!(
            b.set_full_bright(fluid, |_| true),
            Err(RegistryError::InvalidLight(name.to_string()))
        );
        let level = |v: &StateValues<'_>| v.get(FLUID_LEVEL) == Some("3");
        assert_eq!(
            b.set_full_bright(fluid, level),
            Err(RegistryError::InvalidLight(name.to_string()))
        );
        assert_eq!(b.full_bright, before, "{name}");
    }
    // Marking none of its states is no mark, and changes nothing either.
    b.set_full_bright(block(&b, "test:fluid"), |_| false)
        .expect("test:fluid");
    let unknown = BlockId(b.blocks.len() as u16);
    assert_eq!(
        b.set_full_bright(unknown, |_| true),
        Err(RegistryError::UnknownBlock(unknown.0))
    );
    let r = b.build();
    assert!(r.is_full_bright(by(&r, "test:ember")));
    assert!(r.is_full_bright(by(&r, "test:hot[level=3]")));
    assert!(!r.is_full_bright(by(&r, "test:fluid[level=3]")));
}

#[test]
fn a_state_that_emits_nothing_may_be_full_bright() {
    let mut b = builder();
    b.set_full_bright(block(&b, "test:paint"), |_| true)
        .expect("test:paint");
    let r = b.build();
    let paint = by(&r, "test:paint");
    assert!(r.is_full_bright(paint));
    assert!(!r.flags(paint).contains(StateFlags::EMISSIVE));
    assert!(r.emission(paint).is_dark());
}

/// Every state that can be waterlogged keeps flowing fluids out, dry or wet, whether it collides
/// or not; a plant, a cube, a fluid and a full cube with the property (a double slab) do not.
#[test]
fn waterloggable_states_keep_fluids_out() {
    let mut b = builder();
    let solid = b.by_name("test:solid").expect("test:solid");
    let full = BlockDef {
        name: "test:double".to_string(),
        ..b.states[usize::from(solid.0)].clone()
    };
    b.add_block_with_states(full, vec![Property::boolean(WATERLOGGED)], |_, _| {})
        .expect("test:double");
    let r = b.build();
    let mut kept = 0;
    for s in states(&r) {
        let name = r.name(s);
        let keeps = ["test:slab", "test:boxes"].contains(&name);
        assert_eq!(r.keeps_fluids_out(s), keeps, "{}", r.state_name(s));
        assert_eq!(r.keeps_fluids_out(s), r.can_be_waterlogged(s), "{name}");
        kept += usize::from(keeps);
    }
    assert_eq!(kept, 4, "two states each");
    let plant = by(&r, "test:plant");
    assert!(!r.blocks_motion(plant) && !r.keeps_fluids_out(plant));
    assert!(!r.keeps_fluids_out(by(&r, "test:double[waterlogged=true]")));
}

/// A declared block keeps flowing fluids out in every state and nothing else of it changes: it
/// still does not stop movement.
#[test]
fn a_declared_block_keeps_fluids_out_in_every_state() {
    let plain = builder().build();
    let mut b = builder();
    for name in ["test:plant", "test:ember_ore"] {
        b.set_keeps_fluids_out(block(&b, name)).expect(name);
    }
    let marked = b.build();
    assert_eq!(marked.len(), plain.len());
    for s in states(&plain) {
        let declared = ["test:plant", "test:ember_ore"].contains(&plain.name(s));
        let keeps = declared || plain.can_be_waterlogged(s);
        assert_eq!(marked.keeps_fluids_out(s), keeps, "{}", plain.state_name(s));
        assert_eq!(
            marked.flags(s) - StateFlags::KEEPS_FLUIDS_OUT,
            plain.flags(s) - StateFlags::KEEPS_FLUIDS_OUT,
            "{}",
            plain.state_name(s)
        );
        assert_eq!(marked.blocks_motion(s), plain.blocks_motion(s));
    }
    for name in [
        "test:plant",
        "test:ember_ore[lit=true]",
        "test:ember_ore[lit=false]",
    ] {
        assert!(marked.keeps_fluids_out(by(&marked, name)), "{name}");
    }
    assert!(!marked.blocks_motion(by(&marked, "test:plant")));
}

#[test]
fn keeping_fluids_out_needs_a_known_block() {
    let mut b = builder();
    let unknown = BlockId(b.blocks.len() as u16);
    assert_eq!(
        b.set_keeps_fluids_out(unknown),
        Err(RegistryError::UnknownBlock(unknown.0))
    );
    assert!(
        b.keeps_fluids_out.is_empty(),
        "the failed call sets nothing"
    );
}
