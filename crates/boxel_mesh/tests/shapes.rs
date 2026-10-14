//! The per-block path of the non-cube blocks (ADR 0018, ADR 0020, ADR 0027): models made of
//! boxes (slabs, stairs, fences, torches, walls, fence gates), cross plants and turned cubes,
//! against brute-force rules written here, sixteenth by sixteenth, independent of the mesher.

mod common;

use boxel_demo::shapes::{
    FenceSides, StairShape, TORCH_BOX, WallSide, WallSides, fence_boxes, fence_gate_boxes,
    stair_boxes, wall_boxes, wall_torch_box,
};
use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, FaceShading, MaterialTable, MeshOptions,
    Mesher, PackedQuad, PaddedChunk, VoxelClass, layout, reference_box_face_shading_at,
    reference_lit_emitter_shading, reference_visible_faces,
};
use boxel_voxel::{
    Axis, BlockModel, BoxModelId, Face, Facing, LightValue, ModelBox, RenderShape, SlabHalf,
};
use common::{Fill, LIGHT_FILLS, random_light, random_padded, set_material};
use std::collections::BTreeSet;

/// Materials: 1..=2 opaque cubes, 3 cutout cube, 4 translucent cube, 5 bottom slab, 6 top
/// slab, 7 a cross (cutout), 8 a cube turned to X, 9 a cube turned to Z, 10 a translucent
/// bottom slab, 11 and 12 stairs (two of each), 13 and 14 fences, 15 a torch and 16 a wall
/// torch (cutout), 17 and 18 walls (a post with four low sides, a tall straight run), 19 and 20
/// fence gates (closed between walls, open). Layer of (material, face) is `material * 6 +
/// face`.
const BOTTOM: u16 = 5;
const TOP: u16 = 6;
const CROSS: u16 = 7;
const LOG_X: u16 = 8;
const LOG_Z: u16 = 9;
const GLASS_SLAB: u16 = 10;
const STAIRS: u16 = 11;
const STAIRS_TOP: u16 = 12;
const FENCE: u16 = 13;
const FENCE_END: u16 = 14;
const TORCH: u16 = 15;
const WALL_TORCH: u16 = 16;
const WALL: u16 = 17;
const WALL_RUN: u16 = 18;
const GATE: u16 = 19;
const GATE_OPEN: u16 = 20;
const ALL: [u16; 20] = [
    1, 2, 3, 4, BOTTOM, TOP, CROSS, LOG_X, LOG_Z, GLASS_SLAB, STAIRS, STAIRS_TOP, FENCE, FENCE_END,
    TORCH, WALL_TORCH, WALL, WALL_RUN, GATE, GATE_OPEN,
];

/// The model of a material of [`table`]: the materials made of boxes of the demo's shapes are
/// each their own box model (a local id, its material).
fn model_of(m: u16) -> BlockModel {
    match m {
        BOTTOM | GLASS_SLAB => BlockModel::Slab(SlabHalf::Bottom),
        TOP => BlockModel::Slab(SlabHalf::Top),
        CROSS => BlockModel::Cross,
        LOG_X => BlockModel::Cube { axis: Axis::X },
        LOG_Z => BlockModel::Cube { axis: Axis::Z },
        STAIRS..=GATE_OPEN => BlockModel::Boxes(BoxModelId(m)),
        _ => BlockModel::CUBE,
    }
}

/// The boxes of a material of [`table`] (none for cubes and crosses).
fn boxes_of(m: u16) -> Vec<ModelBox> {
    match m {
        BOTTOM | GLASS_SLAB => vec![ModelBox::slab(SlabHalf::Bottom)],
        TOP => vec![ModelBox::slab(SlabHalf::Top)],
        STAIRS => stair_boxes(Facing::East, SlabHalf::Bottom, StairShape::Straight),
        STAIRS_TOP => stair_boxes(Facing::North, SlabHalf::Top, StairShape::InnerLeft),
        FENCE => fence_boxes(FenceSides(0b1111)),
        FENCE_END => fence_boxes(FenceSides::NONE.with(Facing::West, true)),
        TORCH => vec![TORCH_BOX],
        WALL_TORCH => vec![wall_torch_box(Facing::South)],
        WALL => wall_boxes(
            true,
            Facing::ALL
                .into_iter()
                .fold(WallSides::NONE, |s, f| s.with(f, WallSide::Low)),
        ),
        WALL_RUN => wall_boxes(
            false,
            WallSides::NONE
                .with(Facing::North, WallSide::Tall)
                .with(Facing::South, WallSide::Tall),
        ),
        GATE => fence_gate_boxes(Facing::East, false, true),
        GATE_OPEN => fence_gate_boxes(Facing::North, true, false),
        _ => Vec::new(),
    }
}

fn table() -> MaterialTable {
    let shapes: Vec<RenderShape> = (0..=GATE_OPEN)
        .map(|m| match m {
            0 => RenderShape::None,
            3 | CROSS | TORCH | WALL_TORCH => RenderShape::Cutout,
            4 | GLASS_SLAB => RenderShape::Translucent,
            _ => RenderShape::Opaque,
        })
        .collect();
    let layers = (0..shapes.len() as u16)
        .map(|m| Face::ALL.map(|f| m * 6 + f.index() as u16))
        .collect();
    let mut t = MaterialTable::from_parts(shapes, layers);
    for m in ALL {
        t.set_model(m, model_of(m), &boxes_of(m));
    }
    t
}

/// A sixteenth `(u, v)` of a face's plane, in the face's axes.
fn inside(rect: [u8; 4], u: u8, v: u8) -> bool {
    (rect[0]..rect[1]).contains(&u) && (rect[2]..rect[3]).contains(&v)
}

/// Plane (sixteenths along the normal) and rectangle (`[u0, u1, v0, v1]`) of `face` of `b`.
fn face_of(b: ModelBox, face: Face) -> (u8, [u8; 4]) {
    let (n, u, v) = face.axes();
    let (min, max) = (b.bounds.min, b.bounds.max);
    let plane = if face.index().is_multiple_of(2) {
        max[n]
    } else {
        min[n]
    };
    (plane, [min[u], max[u], min[v], max[v]])
}

/// True if every sixteenth of `rect` is in one of `faces`.
fn covered(rect: [u8; 4], faces: &[[u8; 4]]) -> bool {
    (rect[0]..rect[1]).all(|u| (rect[2]..rect[3]).all(|v| faces.iter().any(|f| inside(*f, u, v))))
}

/// One expected or meshed quad of the per-block path: position, face, code, layer, bucket,
/// shading and, for box faces, the box.
type Special = (
    [u32; 3],
    Face,
    u8,
    u16,
    usize,
    FaceShading,
    Option<ModelBox>,
);

/// Sort key of a [`Special`].
fn key(s: &Special) -> ([u32; 3], usize, u8, Option<([u8; 3], [u8; 3], [i8; 3])>) {
    (
        s.0,
        s.1.index(),
        s.2,
        s.6.map(|b| (b.bounds.min, b.bounds.max, b.texture_offset)),
    )
}

/// The quads the per-block path must give, from the rules of ADR 0018 and 0020.
fn expected_specials(
    padded: &PaddedChunk,
    table: &MaterialTable,
    light: Option<&boxel_light::PaddedLight>,
    policy: AoPolicy,
    level: boxel_mesh::LightQuantization,
) -> Vec<Special> {
    let elide = policy == AoPolicy::BakedLitOnly;
    let mut out = Vec::new();
    for y in 0..32 {
        for x in 0..32 {
            for z in 0..32 {
                let m = padded.get(x, y, z);
                if !ALL.contains(&m) {
                    continue;
                }
                let p = [x as u32, y as u32, z as u32];
                let boxes = boxes_of(m);
                for &b in &boxes {
                    for face in Face::ALL {
                        let (plane, rect) = face_of(b, face);
                        // Covered by the opposite faces of the model's other boxes on the plane.
                        let others: Vec<[u8; 4]> = boxes
                            .iter()
                            .filter(|&&o| o != b)
                            .map(|&o| face_of(o, face.opposite()))
                            .filter(|&(p, _)| p == plane)
                            .map(|(_, r)| r)
                            .collect();
                        if covered(rect, &others) {
                            continue;
                        }
                        let border = plane
                            == if face.index().is_multiple_of(2) {
                                16
                            } else {
                                0
                            };
                        let n = face.normal();
                        let nm = padded.get(x + n.x, y + n.y, z + n.z);
                        let hidden = border
                            && (table.class(nm) == VoxelClass::Opaque
                                || (ALL.contains(&nm)
                                    && (nm == m || table.shape(nm) == RenderShape::Opaque)
                                    && covered(
                                        rect,
                                        &boxes_of(nm)
                                            .iter()
                                            .map(|&o| face_of(o, face.opposite()))
                                            .filter(|&(p, _)| p == 16 - plane)
                                            .map(|(_, r)| r)
                                            .collect::<Vec<_>>(),
                                    )));
                        if hidden {
                            continue;
                        }
                        // A lit emitter's face is flat, at the light of the voxel whose layer
                        // shades it (the one in front, or its own against an opaque cube).
                        let emission = table.lit_emission(m);
                        let shading = if emission.is_dark() {
                            reference_box_face_shading_at(padded, light, policy, p, face)
                        } else {
                            let front = [x + n.x, y + n.y, z + n.z];
                            let opaque = table.class(nm) == VoxelClass::Opaque;
                            let cell = if opaque { [x, y, z] } else { front };
                            reference_lit_emitter_shading(light, cell, emission)
                        }
                        .quantized(level, elide);
                        out.push((
                            p,
                            face,
                            layout::SHAPE_BOX,
                            m * 6 + face.index() as u16,
                            table.bucket(m, face),
                            shading,
                            Some(b),
                        ));
                    }
                }
                if m == CROSS {
                    let own = light.map_or(LightValue::FULL_SKY, |l| l.get(x, y, z));
                    let own = own.max(table.lit_emission(m));
                    for face in [Face::PosX, Face::PosZ] {
                        let shading = FaceShading {
                            ao: 0xFF,
                            light: [own; 4],
                        }
                        .quantized(level, false);
                        out.push((
                            p,
                            face,
                            layout::SHAPE_CROSS,
                            m * 6 + Face::PosX.index() as u16,
                            CUTOUT_BUCKET,
                            shading,
                            None,
                        ));
                    }
                }
            }
        }
    }
    out.sort_by_key(key);
    out
}

/// Splits a mesh into its greedy unit faces (checking the turned codes of the logs) and its
/// per-block quads (box faces resolved through the table's model boxes).
fn split_mesh(
    mesh: &ChunkMesh,
    table: &MaterialTable,
) -> (BTreeSet<([u32; 3], Face)>, Vec<Special>) {
    let mut cubes = BTreeSet::new();
    let mut specials = Vec::new();
    for bucket in 0..BUCKET_COUNT {
        for &quad in mesh.bucket(bucket) {
            let m = quad.layer() / 6;
            let shading = FaceShading {
                ao: quad.ao_byte(),
                light: mesh.corner_light(quad),
            };
            match quad.shape() {
                layout::SHAPE_CUBE | layout::SHAPE_TURNED => {
                    let turned = quad.shape() == layout::SHAPE_TURNED;
                    assert_eq!(turned, model_of(m).face_turned(quad.face()), "{quad:?}");
                    for unit in quad.unit_faces() {
                        assert!(cubes.insert(unit), "overlap at {unit:?}");
                    }
                }
                layout::SHAPE_BOX => {
                    let b = table.model_boxes()[usize::from(quad.box_index())];
                    specials.push((
                        quad.pos(),
                        quad.face(),
                        quad.shape(),
                        quad.layer(),
                        bucket,
                        shading,
                        Some(b),
                    ));
                }
                code => {
                    assert_eq!((quad.w(), quad.h()), (1, 1), "{quad:?}");
                    specials.push((
                        quad.pos(),
                        quad.face(),
                        code,
                        quad.layer(),
                        bucket,
                        shading,
                        None,
                    ));
                }
            }
        }
    }
    specials.sort_by_key(key);
    (cubes, specials)
}

fn mesh(
    padded: &PaddedChunk,
    table: &MaterialTable,
    light: Option<&boxel_light::PaddedLight>,
    ao: AoPolicy,
) -> ChunkMesh {
    let mut out = ChunkMesh::default();
    Mesher::new().mesh(padded, light, table, MeshOptions::with_ao(ao), &mut out);
    out
}

#[test]
fn random_chunks_with_shapes_match_the_rules() {
    random_chunks_match_the_rules(&table(), 0..48);
}

/// The same with lit emitters (ADR 0036) among the cubes, the cross and the boxes: their faces
/// are flat, at their light raised to their emission.
#[test]
fn random_chunks_with_lit_emitters_match_the_rules() {
    let mut table = table();
    for (m, rgb) in [
        (2, [9, 4, 0]),
        (CROSS, [3, 1, 0]),
        (STAIRS, [15, 6, 2]),
        (TORCH, [14, 11, 6]),
        (GLASS_SLAB, [1, 1, 1]),
    ] {
        table.set_lit_emission(m, LightValue::from_rgb(rgb));
    }
    random_chunks_match_the_rules(&table, 100..124);
}

/// Random chunks of every shape of `table`, one per seed, under every light and AO policy:
/// the greedy faces equal the oracle and the per-block quads the rules.
fn random_chunks_match_the_rules(table: &MaterialTable, seeds: std::ops::Range<u64>) {
    let fills = [Fill::Noise, Fill::Boxes, Fill::Terrain, Fill::Lattice];
    let policies = [AoPolicy::Off, AoPolicy::Baked, AoPolicy::BakedLitOnly];
    let mut specials_seen = 0;
    let count = seeds.end - seeds.start;
    for seed in seeds {
        let fill = fills[seed as usize % fills.len()];
        // Shapes first in the palette: Terrain puts the first entries on top.
        let palette: Vec<u16> = if seed % 2 == 0 {
            vec![
                BOTTOM, STAIRS, CROSS, FENCE, TOP, WALL, 1, TORCH, LOG_X, 2, GATE, STAIRS_TOP,
                GLASS_SLAB, LOG_Z, WALL_RUN, FENCE_END, 3, WALL_TORCH, GATE_OPEN, 4,
            ]
        } else {
            ALL.to_vec()
        };
        let padded = random_padded(table, fill, seed, 20 + (seed as u32 * 7) % 60, &palette);
        let light_fill = LIGHT_FILLS[seed as usize % LIGHT_FILLS.len()];
        let light = random_light(light_fill, seed);
        let policy = policies[seed as usize % 3];
        let mesh = mesh(&padded, table, light.as_ref(), policy);
        let (cubes, specials) = split_mesh(&mesh, table);
        assert_eq!(cubes, reference_visible_faces(&padded), "seed {seed}");
        let expected = expected_specials(
            &padded,
            table,
            light.as_ref(),
            policy,
            mesh.light_quantization,
        );
        assert_eq!(specials.len(), expected.len(), "seed {seed} ({fill:?})");
        for (got, want) in specials.iter().zip(&expected) {
            assert_eq!(
                got, want,
                "seed {seed} ({fill:?}, {light_fill:?}, {policy:?})"
            );
        }
        specials_seen += specials.len();
        assert_eq!(
            mesh.bucket_counts.iter().sum::<u32>(),
            mesh.total_quads(),
            "seed {seed}"
        );
    }
    assert!(
        specials_seen as u64 > 1_000 * count,
        "the fills exercise the per-block path ({specials_seen})"
    );
}

/// The box faces of the quads at `p`: (face, box).
fn box_faces(specials: &[Special], p: [u32; 3]) -> Vec<(Face, ModelBox)> {
    specials
        .iter()
        .filter(|s| s.0 == p && s.2 == layout::SHAPE_BOX)
        .map(|s| (s.1, s.6.unwrap()))
        .collect()
}

#[test]
fn slabs_hide_and_show_the_faces_they_should() {
    let table = table();
    let mut padded = PaddedChunk::new();
    let put = |padded: &mut PaddedChunk, p: [i32; 3], m| set_material(padded, &table, p, m);
    // A stone floor at y = 0, two bottom slabs side by side on it, a top slab next to them and a
    // glass block next to the top slab.
    for x in 0..8 {
        for z in 0..8 {
            put(&mut padded, [x, 0, z], 1);
        }
    }
    put(&mut padded, [2, 1, 2], BOTTOM);
    put(&mut padded, [3, 1, 2], BOTTOM);
    put(&mut padded, [4, 1, 2], TOP);
    put(&mut padded, [5, 1, 2], 4);
    let mesh = mesh(&padded, &table, None, AoPolicy::Off);
    let (cubes, specials) = split_mesh(&mesh, &table);
    let faces =
        |p: [u32; 3]| -> Vec<Face> { box_faces(&specials, p).iter().map(|f| f.0).collect() };
    // The left slab: no bottom (the floor), no +X (the other bottom slab).
    assert_eq!(
        faces([2, 1, 2]),
        [Face::PosY, Face::NegX, Face::PosZ, Face::NegZ]
    );
    // The middle slab: no bottom, no -X; its +X faces the top slab's other half: drawn.
    assert_eq!(
        faces([3, 1, 2]),
        [Face::PosY, Face::PosX, Face::PosZ, Face::NegZ]
    );
    // The top slab: all six (its bottom is inside its cell, glass hides nothing).
    assert_eq!(faces([4, 1, 2]).len(), 6);
    // The floor under the slabs still draws its top (hidden by depth), and the glass its -X.
    assert!(cubes.contains(&([2, 0, 2], Face::PosY)));
    assert!(cubes.contains(&([5, 1, 2], Face::NegX)));
    // Boxes and buckets: opaque slab faces go to their face bucket.
    for s in &specials {
        let want = boxes_of(if s.0 == [4, 1, 2] { TOP } else { BOTTOM })[0];
        assert_eq!(s.6, Some(want));
        assert_eq!(s.4, s.1.index());
    }
}

#[test]
fn stairs_draw_their_boxes_and_hide_like_minecraft() {
    let table = table();
    let mut padded = PaddedChunk::new();
    let put = |padded: &mut PaddedChunk, p: [i32; 3], m| set_material(padded, &table, p, m);
    // Stairs facing east on a floor, a stone block at their back and a bottom slab in front.
    for x in 0..8 {
        for z in 0..8 {
            put(&mut padded, [x, 0, z], 1);
        }
    }
    put(&mut padded, [3, 1, 3], STAIRS);
    put(&mut padded, [4, 1, 3], 1);
    put(&mut padded, [2, 1, 3], BOTTOM);
    let mesh = mesh(&padded, &table, None, AoPolicy::Off);
    let (_, specials) = split_mesh(&mesh, &table);
    let slab = ModelBox::new([0, 0, 0], [16, 8, 16]);
    let step = ModelBox::new([8, 8, 0], [16, 16, 16]);
    let mut got = box_faces(&specials, [3, 1, 3]);
    got.sort_by_key(|(f, b)| (f.index(), b.bounds.min));
    // The slab: its top (under the step too, as Minecraft draws it) and its two sides; its
    // bottom lies on the floor, its back on the stone, its front on the bottom slab's side.
    // The step: its top, its riser and its two sides; its bottom lies on the slab, its back on
    // the stone.
    let want = vec![
        (Face::PosY, slab),
        (Face::PosY, step),
        (Face::NegX, step),
        (Face::PosZ, slab),
        (Face::PosZ, step),
        (Face::NegZ, slab),
        (Face::NegZ, step),
    ];
    assert_eq!(got, want);
    // The bottom slab's +X half face is hidden by the stairs' full lower half.
    assert!(
        !box_faces(&specials, [2, 1, 3])
            .iter()
            .any(|(f, _)| *f == Face::PosX)
    );
}

#[test]
fn fences_and_torches_hide_only_what_they_cover() {
    let table = table();
    let mut padded = PaddedChunk::new();
    let put = |padded: &mut PaddedChunk, p: [i32; 3], m| set_material(padded, &table, p, m);
    // Two connected fences (east one an end post open to the west), a torch on stone.
    put(&mut padded, [5, 5, 5], FENCE);
    put(&mut padded, [6, 5, 5], FENCE_END);
    put(&mut padded, [9, 4, 9], 1);
    put(&mut padded, [9, 5, 9], TORCH);
    let mesh = mesh(&padded, &table, None, AoPolicy::Off);
    let (_, specials) = split_mesh(&mesh, &table);
    let fence = box_faces(&specials, [5, 5, 5]);
    // The post: 6 faces. Each of the 8 bars: top, bottom, two sides and its outer end, minus
    // the 2 outer ends against the other fence's bars (the end against the post is never drawn).
    assert_eq!(fence.len(), 6 + 8 * 5 - 2);
    assert!(
        !fence
            .iter()
            .any(|(f, b)| *f == Face::PosX && b.bounds.max[0] == 16)
    );
    let end = box_faces(&specials, [6, 5, 5]);
    assert_eq!(end.len(), 6 + 2 * 5 - 2);
    // The torch: no bottom (on stone), its 5 other faces, in the cutout bucket.
    let torch = box_faces(&specials, [9, 5, 9]);
    assert_eq!(torch.len(), 5);
    assert!(!torch.iter().any(|(f, _)| *f == Face::NegY));
    assert!(
        specials
            .iter()
            .filter(|s| s.0 == [9, 5, 9])
            .all(|s| s.4 == CUTOUT_BUCKET)
    );
}

/// A wall's bars stop at its post, a straight run is one box whose ends hide against the next
/// wall, and a gate between walls hides the ends of its posts against them (ADR 0027).
#[test]
fn walls_and_gates_hide_what_they_cover() {
    let table = table();
    let mut padded = PaddedChunk::new();
    let put = |padded: &mut PaddedChunk, p: [i32; 3], m| set_material(padded, &table, p, m);
    // A lone wall with a post and four low sides, in the air.
    put(&mut padded, [3, 5, 3], WALL);
    // A run of walls along z with a gate in it (the gate runs along z: it faces east).
    put(&mut padded, [9, 5, 8], WALL_RUN);
    put(&mut padded, [9, 5, 9], WALL_RUN);
    put(&mut padded, [9, 5, 10], GATE);
    put(&mut padded, [9, 5, 11], WALL_RUN);
    let mesh = mesh(&padded, &table, None, AoPolicy::Off);
    let (_, specials) = split_mesh(&mesh, &table);
    // The post: 6 faces; each bar: top, bottom, two sides and its outer end (the end at the post
    // is never drawn).
    assert_eq!(box_faces(&specials, [3, 5, 3]).len(), 6 + 4 * 5);
    // One box per run piece: top, bottom, two sides, and the open end of the first one.
    let first = box_faces(&specials, [9, 5, 8]);
    assert_eq!(first.len(), 5);
    assert!(first.iter().any(|(f, _)| *f == Face::NegZ));
    assert_eq!(
        box_faces(&specials, [9, 5, 9]).len(),
        4 + 1,
        "its end at the gate shows"
    );
    // The gate's posts end against the walls: no gate face on the cell border along z.
    let gate = box_faces(&specials, [9, 5, 10]);
    assert!(!gate.is_empty());
    assert!(!gate.iter().any(|(f, b)| {
        (*f == Face::NegZ && b.bounds.min[2] == 0) || (*f == Face::PosZ && b.bounds.max[2] == 16)
    }));
    // In the wall: 3 sixteenths lower than Minecraft's free gate (its posts start at 5).
    assert!(
        gate.iter()
            .all(|(_, b)| b.bounds.min[1] >= 2 && b.bounds.max[1] <= 13)
    );
}

#[test]
fn crosses_are_two_cutout_quads_lit_by_their_own_cell() {
    let table = table();
    let mut padded = PaddedChunk::new();
    set_material(&mut padded, &table, [7, 3, 9], CROSS);
    // Opaque blocks all around change nothing: a cross is never hidden.
    for face in Face::ALL {
        let n = face.normal();
        set_material(&mut padded, &table, [7 + n.x, 3 + n.y, 9 + n.z], 1);
    }
    let light = common::full_light(|x, y, z| {
        if [x, y, z] == [7, 3, 9] {
            0x9312
        } else {
            0xF000
        }
    });
    let mesh = mesh(&padded, &table, Some(&light), AoPolicy::Baked);
    let cross: Vec<PackedQuad> = mesh.bucket(CUTOUT_BUCKET).to_vec();
    assert_eq!(cross.len(), 2);
    for (quad, face) in cross.iter().zip([Face::PosX, Face::PosZ]) {
        assert_eq!(quad.shape(), layout::SHAPE_CROSS);
        assert_eq!(quad.face(), face);
        assert_eq!(quad.pos(), [7, 3, 9]);
        assert_eq!(quad.ao_byte(), 0xFF);
        assert_eq!(mesh.corner_light(*quad), [LightValue(0x9312); 4]);
    }
}

#[test]
fn the_registry_classes_follow_shape_and_model() {
    let registry = boxel_demo::blocks::registry();
    let table = MaterialTable::from_registry(&registry);
    let id = |name: &str| registry.parse_state(name).unwrap().0;
    let class = |name: &str| table.class(id(name));
    assert_eq!(class("boxel:stone"), VoxelClass::Opaque);
    assert_eq!(class("boxel:glass"), VoxelClass::Transparent);
    assert_eq!(class("boxel:water"), VoxelClass::Transparent);
    assert_eq!(class("boxel:stone_slab[type=bottom]"), VoxelClass::Special);
    assert_eq!(class("boxel:stone_slab[type=top]"), VoxelClass::Special);
    assert_eq!(class("boxel:stone_slab[type=double]"), VoxelClass::Opaque);
    assert_eq!(class("boxel:short_grass"), VoxelClass::Special);
    assert_eq!(class("boxel:poppy"), VoxelClass::Special);
    assert_eq!(class("boxel:log[axis=x]"), VoxelClass::Opaque);
    let log_x = id("boxel:log[axis=x]");
    assert_eq!(table.cube_code(log_x, Face::PosX), layout::SHAPE_CUBE);
    assert_eq!(table.cube_code(log_x, Face::PosY), layout::SHAPE_TURNED);
    assert_eq!(
        table.bucket(id("boxel:glass"), Face::PosY),
        boxel_mesh::TRANSLUCENT_BUCKET
    );
}

/// A double slab (ADR 0019) is a plain stone cube to the mesher: the greedy path merges it into
/// the same quads, with the same layers, as stone in its place.
#[test]
fn double_slabs_mesh_like_stone_cubes() {
    let registry = boxel_demo::blocks::registry();
    let table = MaterialTable::from_registry(&registry);
    let stone = registry.parse_state("boxel:stone").unwrap().0;
    let double = registry
        .parse_state("boxel:stone_slab[type=double]")
        .unwrap()
        .0;
    let bottom = registry.parse_state("boxel:stone_slab").unwrap().0;
    // A 4 x 3 x 5 box of `m` with a bottom slab on top of one corner.
    let build = |m: u16| {
        let mut padded = PaddedChunk::new();
        for x in 2..6 {
            for y in 1..4 {
                for z in 3..8 {
                    set_material(&mut padded, &table, [x, y, z], m);
                }
            }
        }
        set_material(&mut padded, &table, [2, 4, 3], bottom);
        padded
    };
    let ao = AoPolicy::Baked;
    let with_stone = mesh(&build(stone), &table, None, ao);
    let with_double = mesh(&build(double), &table, None, ao);
    for bucket in 0..BUCKET_COUNT {
        assert_eq!(
            with_double.bucket(bucket),
            with_stone.bucket(bucket),
            "bucket {bucket}"
        );
    }
    // The box is six greedy quads (a cube face is never hidden by a slab), and the slab five
    // (its bottom lies on the box).
    let quads: Vec<PackedQuad> = (0..BUCKET_COUNT)
        .flat_map(|b| with_double.bucket(b).iter().copied())
        .collect();
    let cubes = quads
        .iter()
        .filter(|q| q.shape() == layout::SHAPE_CUBE)
        .count();
    assert_eq!((cubes, quads.len() - cubes), (6, 5));
}
