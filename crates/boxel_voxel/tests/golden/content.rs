//! The dump of G1 (`content_golden.rs`), shared with the demo's golden
//! (`boxel_demo/tests/golden.rs`): everything the registry says about each state, the texture
//! layer names in order, and what placing a block into air or a water source and breaking it
//! leave. Only the accessor helpers below may change after the content moved.

use boxel_voxel::rules::{broken, placed_into};
use boxel_voxel::shapes::half_name;
use boxel_voxel::{
    BlockCollision, BlockModel, BlockRegistry, BlockState, CollisionBox, Face, ModelBox, StateFlags,
};
use std::fmt::Write as _;

/// The boxes `s` is drawn with.
fn boxes_of(r: &BlockRegistry, s: BlockState) -> Vec<ModelBox> {
    r.model_boxes(s).to_vec()
}

/// The shape `s` is picked by.
fn outline_of(r: &BlockRegistry, s: BlockState) -> BlockCollision {
    r.outline(s).clone()
}

/// The flags of `s` as they were frozen: `KEEPS_FLUIDS_OUT` came later (#110) and is left out
/// where it only repeats `WATERLOGGABLE`; anywhere else it shows.
fn flags_of(r: &BlockRegistry, s: BlockState) -> u16 {
    let flags = r.flags(s);
    if flags.contains(StateFlags::WATERLOGGABLE) {
        (flags - StateFlags::KEEPS_FLUIDS_OUT).bits()
    } else {
        flags.bits()
    }
}

/// The model printed by kind: today's stairs, fences, torches, walls and gates are all `boxes`.
fn model_kind(m: BlockModel) -> String {
    match m {
        BlockModel::Cube { axis } => format!("cube({})", axis.name()),
        BlockModel::Slab(half) => format!("slab({})", half_name(half)),
        BlockModel::Cross => "cross".to_string(),
        BlockModel::Fluid => "fluid".to_string(),
        _ => "boxes".to_string(),
    }
}

fn box_text(b: CollisionBox) -> String {
    format!("{:?}-{:?}", b.min, b.max)
}

fn collision_text(c: &BlockCollision) -> String {
    match c {
        BlockCollision::None => "none".to_string(),
        BlockCollision::Full => "full".to_string(),
        BlockCollision::Boxes(boxes) => {
            let boxes: Vec<String> = boxes.iter().map(|&b| box_text(b)).collect();
            format!("[{}]", boxes.join(" "))
        }
    }
}

/// The canonical dump of a registry.
pub fn dump(r: &BlockRegistry) -> String {
    let mut d = String::new();
    let tex = |s: BlockState, f: Face| {
        let id = r.face_texture(s, f);
        r.textures()
            .get(usize::from(id.0))
            .map_or_else(|| format!("#{}", id.0), |t| t.name.clone())
    };
    let _ = writeln!(d, "states {} blocks {}", r.len(), r.block_count());
    for b in r.blocks() {
        let states: Vec<u16> = r.states(b).map(|s| s.0).collect();
        let _ = writeln!(
            d,
            "block {} {} first {:?} count {} default {}",
            b.0,
            r.block_name(b),
            states.first(),
            states.len(),
            r.state_name(r.default_state(b)),
        );
    }
    for i in 0..r.len() {
        let s = BlockState(i as u16);
        let boxes: Vec<String> = boxes_of(r, s)
            .iter()
            .map(|b| format!("{}@{:?}", box_text(b.bounds), b.texture_offset))
            .collect();
        let faces: Vec<String> = Face::ALL.iter().map(|&f| tex(s, f)).collect();
        let _ = writeln!(
            d,
            "{i} {} block={:?} shape={:?} model={} boxes=[{}] outline={} collision={} \
             flags={:#06x} emission={:#06x} opacity={} faces=[{}] material={}",
            r.state_name(s),
            r.block(s).map(|b| b.0),
            r.shape(s),
            model_kind(r.model(s)),
            boxes.join(" "),
            collision_text(&outline_of(r, s)),
            collision_text(r.collision(s)),
            flags_of(r, s),
            r.emission(s).0,
            r.light_opacity(s),
            faces.join(","),
            r.material(s),
        );
    }
    for (i, t) in r.textures().iter().enumerate() {
        let _ = writeln!(d, "texture {i} {}", t.name);
    }
    let water = r.by_name("boxel:water").unwrap_or(BlockState::AIR);
    for i in 0..r.len() {
        let s = BlockState(i as u16);
        let _ = writeln!(
            d,
            "place {i} air={} water={} broken={}",
            r.state_name(placed_into(r, s, BlockState::AIR)),
            r.state_name(placed_into(r, s, water)),
            r.state_name(broken(r, s)),
        );
    }
    d
}
