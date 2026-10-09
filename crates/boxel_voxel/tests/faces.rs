//! `ChunkFaceInfo` (cave culling, ADR 0022) against a brute-force flood fill voxel by voxel, on
//! random chunks of every storage representation (uniform, 1/2/4/8-bit packed, direct) and on
//! cave-like chunks carved by random tunnels.
use boxel_voxel::*;
use proptest::prelude::*;
use std::collections::VecDeque;

/// Small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// States of [`test_registry`] (more than 256, so `Direct` chunks are possible).
const STATES: u16 = 320;

/// A registry whose states cycle through occluding cubes, opaque non-cubes (slabs), cutout,
/// translucent and air-like blocks: every third state occludes.
fn test_registry() -> BlockRegistry {
    let mut b = BlockRegistryBuilder::new();
    let t = b
        .add_texture(TextureSpec {
            name: "test:t".into(),
            base_color: [1, 2, 3],
            noise: 0,
            pattern: TexturePattern::Noise,
        })
        .unwrap();
    for i in 1..STATES {
        let (shape, model) = match i % 6 {
            0 | 3 => (RenderShape::Opaque, BlockModel::CUBE),
            1 => (RenderShape::Cutout, BlockModel::CUBE),
            2 => (RenderShape::Translucent, BlockModel::CUBE),
            4 => (RenderShape::Opaque, BlockModel::Slab(SlabHalf::Bottom)),
            _ => (RenderShape::None, BlockModel::CUBE),
        };
        b.add_block(BlockDef {
            name: format!("test:b{i}"),
            shape,
            model,
            collision: BlockCollision::Full,
            emission: [0; 3],
            light_opacity: if i.is_multiple_of(3) {
                OPAQUE_OPACITY
            } else {
                1
            },
            textures: FaceTextures::all(t),
        })
        .unwrap();
    }
    b.build()
}

/// Face info voxel by voxel: a flood fill over open voxels from each face's open voxels.
fn brute_force(chunk: &ChunkData, registry: &BlockRegistry) -> ChunkFaceInfo {
    let open: Vec<bool> = (0..CHUNK_VOLUME)
        .map(|i| !registry.occludes(chunk.get(LocalPos::from_index(i))))
        .collect();
    let mut links = 0;
    for from in Face::ALL {
        let mut seen = vec![false; CHUNK_VOLUME];
        let mut queue = VecDeque::new();
        for i in 0..CHUNK_VOLUME {
            let p = LocalPos::from_index(i);
            if open[i] && p.border_faces() & (1 << from.index()) != 0 {
                seen[i] = true;
                queue.push_back(p);
            }
        }
        let mut touched = 0u8;
        while let Some(p) = queue.pop_front() {
            touched |= p.border_faces();
            for f in Face::ALL {
                let n = f.normal();
                let (x, y, z) = (p.x() as i32 + n.x, p.y() as i32 + n.y, p.z() as i32 + n.z);
                if !(0..32).contains(&x) || !(0..32).contains(&y) || !(0..32).contains(&z) {
                    continue;
                }
                let q = LocalPos::new(x as u32, y as u32, z as u32);
                if open[q.index()] && !seen[q.index()] {
                    seen[q.index()] = true;
                    queue.push_back(q);
                }
            }
        }
        for to in Face::ALL {
            if to != from && touched & (1 << to.index()) != 0 {
                links |= link_bit(from, to);
            }
        }
    }
    let mut opaque_faces = 0;
    for f in Face::ALL {
        let closed = (0..CHUNK_VOLUME)
            .all(|i| !(LocalPos::from_index(i).border_faces() & (1 << f.index()) != 0 && open[i]));
        opaque_faces |= u8::from(closed) << f.index();
    }
    ChunkFaceInfo {
        links,
        opaque_faces,
        all_opaque: open.iter().all(|&o| !o),
    }
}

/// A dense chunk of `k` distinct states, in runs of up to `run` voxels, `occluding_pct` % of
/// the runs of occluding states.
fn random_dense(rng: &mut Rng, k: usize, run: usize, occluding_pct: u64) -> Vec<BlockState> {
    let pick = |rng: &mut Rng, occluding: bool| loop {
        let s = 1 + rng.below(u64::from(STATES) - 1) as u16;
        if s.is_multiple_of(3) == occluding {
            return BlockState(s);
        }
    };
    let mut open = Vec::new();
    let mut closed = Vec::new();
    for j in 0..k {
        if j % 2 == 0 {
            closed.push(pick(rng, true));
        } else {
            open.push(pick(rng, false));
        }
    }
    let mut out = Vec::with_capacity(CHUNK_VOLUME);
    while out.len() < CHUNK_VOLUME {
        let set = if rng.below(100) < occluding_pct || open.is_empty() {
            &closed
        } else {
            &open
        };
        let s = set[rng.below(set.len() as u64) as usize];
        let len = 1 + rng.below(run as u64) as usize;
        for _ in 0..len.min(CHUNK_VOLUME - out.len()) {
            out.push(s);
        }
    }
    out
}

/// Solid stone with `tunnels` random axis-aligned tunnels of open blocks (cave-like: few large
/// regions, some touching several faces, some closed pockets).
fn tunnels(rng: &mut Rng, tunnels: usize) -> Vec<BlockState> {
    let mut out = vec![BlockState(3); CHUNK_VOLUME];
    for _ in 0..tunnels {
        let mut p = [
            rng.below(32) as i32,
            rng.below(32) as i32,
            rng.below(32) as i32,
        ];
        let steps = 4 + rng.below(60);
        let open = BlockState([0, 1, 2, 4, 5][rng.below(5) as usize]);
        for _ in 0..steps {
            let axis = rng.below(3) as usize;
            let dir = if rng.below(2) == 0 { -1 } else { 1 };
            for _ in 0..1 + rng.below(6) {
                p[axis] = (p[axis] + dir).clamp(0, 31);
                let l = LocalPos::new(p[0] as u32, p[1] as u32, p[2] as u32);
                out[l.index()] = open;
            }
        }
    }
    out
}

fn check(dense: &[BlockState], registry: &BlockRegistry, what: &str) {
    let chunk = ChunkData::from_dense(dense);
    let want = brute_force(&chunk, registry);
    assert_eq!(ChunkFaceInfo::compute(&chunk, registry), want, "{what}");
    let occupancy = chunk.opaque_occupancy(registry);
    assert_eq!(ChunkFaceInfo::from_occupancy(&occupancy), want, "{what}");
}

#[test]
fn uniform_and_trivial_chunks() {
    let r = test_registry();
    assert_eq!(
        ChunkFaceInfo::compute(&ChunkData::uniform(BlockState(3)), &r),
        ChunkFaceInfo::SOLID
    );
    for s in [0, 1, 2, 4, 5] {
        assert_eq!(
            ChunkFaceInfo::compute(&ChunkData::uniform(BlockState(s)), &r),
            ChunkFaceInfo::OPEN,
            "state {s}"
        );
    }
    // One open voxel in the middle of stone links nothing. A voxel on an edge touches two
    // faces: it is a path between them.
    let mut dense = vec![BlockState(3); CHUNK_VOLUME];
    dense[LocalPos::new(16, 16, 16).index()] = BlockState::AIR;
    check(&dense, &r, "pocket");
    dense[LocalPos::new(0, 31, 7).index()] = BlockState::AIR;
    check(&dense, &r, "edge voxel");
    let chunk = ChunkData::from_dense(&dense);
    let info = ChunkFaceInfo::compute(&chunk, &r);
    assert!(info.linked(Face::NegX, Face::PosY));
    assert_eq!(info.links.count_ones(), 1);
    assert_eq!(info.opaque_faces, 0x3f & !(1 << 0) & !(1 << 3));
    // A straight shaft along y links +Y and -Y only.
    let mut shaft = vec![BlockState(3); CHUNK_VOLUME];
    for y in 0..32 {
        shaft[LocalPos::new(10, y, 20).index()] = BlockState(2);
    }
    check(&shaft, &r, "shaft");
    let info = ChunkFaceInfo::compute(&ChunkData::from_dense(&shaft), &r);
    assert_eq!(info.links, link_bit(Face::PosY, Face::NegY));
}

#[test]
fn every_storage_representation() {
    let r = test_registry();
    let mut rng = Rng(0x5eed_f00d);
    // k distinct states give 1, 2, 4, 8 bits and Direct (more than 256).
    for (k, run) in [(2, 64), (3, 40), (12, 16), (100, 8), (300, 4), (300, 1)] {
        for pct in [20, 50, 75, 90] {
            let dense = random_dense(&mut rng, k, run, pct);
            check(&dense, &r, &format!("k={k} run={run} pct={pct}"));
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn random_chunks_match_brute_force(
        seed in any::<u64>(),
        k in 2usize..20,
        run in 1usize..128,
        pct in 30u64..95,
    ) {
        let r = test_registry();
        let mut rng = Rng(seed | 1);
        let dense = random_dense(&mut rng, k, run, pct);
        check(&dense, &r, "random");
    }

    #[test]
    fn cave_like_chunks_match_brute_force(seed in any::<u64>(), n in 0usize..12) {
        let r = test_registry();
        let mut rng = Rng(seed | 1);
        let dense = tunnels(&mut rng, n);
        check(&dense, &r, "tunnels");
    }
}
