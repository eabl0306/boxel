//! G4 of #48: the light tables of the built-in content (opacity and emission of each state) and
//! the light of a showcase chunk with every state of the registry on a grid, lit by the sky and
//! by its own emitters. Frozen before the content moved out of the engine: the literal never
//! changes, only the setup line below may.

#[path = "../../boxel_voxel/tests/golden/mod.rs"]
mod golden;

use boxel_light::box_light::{chunk_index, tile_index};
use boxel_light::sky_tile::walk;
use boxel_light::{LayerData, LightInput, LightScratch, PaddedLight, SkyInput, SkyTile, compute};
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, ChunkPos, LightTables, LocalPos};
use std::fmt::Write as _;

/// The registry under test.
fn registry() -> BlockRegistry {
    BlockRegistry::default_blocks()
}

/// Every state of `r` on a grid (the mesh golden's showcase): a sparse grid in the lower half,
/// two dense layers in the upper half.
fn showcase(r: &BlockRegistry) -> ChunkData {
    let mut chunk = ChunkData::uniform(BlockState::AIR);
    let n = r.len();
    for i in 0..n {
        let s = BlockState(i as u16);
        let (x, z, y) = ((i % 16) * 2, (i / 16 % 16) * 2, (i / 256) * 2);
        chunk.set(LocalPos::new(x as u32, y as u32, z as u32), s);
        let (x, z) = (i % 32, i / 32 % 32);
        chunk.set(LocalPos::new(x as u32, 20 + (i / 1024) as u32, z as u32), s);
        let j = (i * 7 + 3) % n;
        chunk.set(
            LocalPos::new(x as u32, 21 + (i / 1024) as u32, z as u32),
            BlockState(j as u16),
        );
    }
    chunk
}

fn dump(r: &BlockRegistry) -> String {
    let t = LightTables::from_registry(r);
    let mut d = String::new();
    let _ = writeln!(
        d,
        "len={} max_partial={} any_emitter={}",
        t.len(),
        t.max_partial_opacity(),
        t.any_emitter()
    );
    for i in 0..r.len() {
        let s = BlockState(i as u16);
        let _ = writeln!(
            d,
            "{i} opacity={} emission={:#06x} blocks_sky={} opaque={} emits={}",
            t.opacity(s),
            t.emission(s).0,
            t.blocks_sky(s),
            t.is_opaque(s),
            t.emits(s),
        );
    }
    // The showcase alone at the origin, air around it, its column walked for the sky tops.
    let chunk = showcase(r);
    let floor = -64;
    let tiles: Vec<SkyTile> = (0..9)
        .map(|i| {
            let center = i == tile_index(0, 0);
            walk(31, floor, &t, |cy| {
                if center && cy == 0 {
                    LayerData::Ref(&chunk)
                } else {
                    LayerData::Air
                }
            })
        })
        .collect();
    let mut chunks = [None; 27];
    chunks[chunk_index(0, 0, 0)] = Some(&chunk);
    let input = LightInput {
        origin: ChunkPos::new(0, 0, 0),
        chunks,
        sky: SkyInput::Tiles(std::array::from_fn(|i| &tiles[i])),
        tables: &t,
    };
    let mut out = PaddedLight::default();
    let path = compute(&input, &mut LightScratch::new(), &mut out);
    let values: Vec<u16> = out.to_vec().iter().map(|v| v.0).collect();
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let _ = writeln!(
        d,
        "showcase path={path:?} tops={:#018x} light={:#018x}",
        golden::fnv1a64(
            &tiles[tile_index(0, 0)]
                .tops()
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>()
        ),
        golden::fnv1a64(&bytes),
    );
    d
}

#[test]
fn the_light_of_the_content_is_frozen() {
    golden::check("light", &dump(&registry()), 0x98de_5364_522d_a3f1);
}
