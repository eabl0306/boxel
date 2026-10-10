//! G5 of #48: the physics collision table of the built-in content, read through its public
//! per-state queries (bits, collision boxes, outline boxes) and its reach. Frozen before the
//! content moved out of the engine: the literal never changes, only the setup line below may.

#[path = "../../boxel_voxel/tests/golden/mod.rs"]
mod golden;

use boxel_physics::{CollisionShape, CollisionTable};
use boxel_voxel::{BlockRegistry, BlockState};
use std::fmt::Write as _;

/// The registry under test.
fn registry() -> BlockRegistry {
    BlockRegistry::default_blocks()
}

fn shape_text(shape: CollisionShape<'_>) -> String {
    match shape {
        CollisionShape::Empty => "empty".to_string(),
        CollisionShape::Full => "full".to_string(),
        CollisionShape::Boxes(boxes) => {
            let boxes: Vec<String> = boxes.iter().map(|b| format!("{b:?}")).collect();
            format!("[{}]", boxes.join(" "))
        }
        other => format!("{other:?}"),
    }
}

fn dump(r: &BlockRegistry) -> String {
    let t = CollisionTable::from_registry(r);
    let mut d = String::new();
    let _ = writeln!(
        d,
        "len={} reach={:?} boxes={} plane_gap={:?}",
        t.len(),
        t.reach(),
        t.has_boxes(),
        t.plane_gap()
    );
    for i in 0..r.len() {
        let s = BlockState(i as u16);
        let _ = writeln!(
            d,
            "{i} {} full={} tall={} collides={} fluid={} wet={} outline?={} tall_set={} \
             shape={} outline={}",
            r.state_name(s),
            t.is_full(s),
            t.is_tall(s),
            t.collides(s),
            t.is_fluid(s),
            t.is_wet(s),
            t.has_outline(s),
            t.tall_states().contains(s),
            shape_text(t.shape(s)),
            shape_text(t.outline(s)),
        );
    }
    d
}

#[test]
fn the_collision_table_of_the_content_is_frozen() {
    golden::check("collision", &dump(&registry()), 0x371f_dab7_dbe4_0797);
}
