//! The demo's sky source (ADR 0061): the `boxel:sky_source` block and [`LitCaves`].
use boxel_demo::blocks::{self, SKY_SOURCE, SKY_SOURCE_RANGES};
use boxel_demo::worldgen::{DEFAULT_SEED, LitCaves, OverworldGenerator, OverworldSettings};
use boxel_voxel::{
    BeamDir, BlockCollision, BlockState, ChunkKey, ChunkPos, LocalPos, RenderShape, SkySourceDef,
};
use boxel_worldgen::WorldGenerator;
use std::sync::Arc;

#[test]
fn the_source_block_comes_after_every_demo_state() {
    let plain = blocks::registry();
    let lit = blocks::registry_with_sky_source();
    assert!(plain.by_name(SKY_SOURCE).is_none(), "not in the plain demo");
    // Every demo state keeps its id and name.
    for s in 0..plain.len() as u16 {
        let s = BlockState(s);
        assert_eq!(
            plain.state_name(s).to_string(),
            lit.state_name(s).to_string()
        );
    }
    assert_eq!(lit.len(), plain.len() + 2 * SKY_SOURCE_RANGES.len());
    let default = lit.by_name(SKY_SOURCE).expect("the source");
    assert_eq!(
        lit.state_name(default).to_string(),
        "boxel:sky_source[dir=down,range=64]"
    );
    for dir in [BeamDir::Down, BeamDir::Up] {
        for range in SKY_SOURCE_RANGES {
            let name = format!(
                "{SKY_SOURCE}[dir={},range={range}]",
                if dir == BeamDir::Down { "down" } else { "up" }
            );
            let state = lit.parse_state(&name).expect(&name);
            assert_eq!(lit.sky_source(state), Some(SkySourceDef { dir, range }));
            assert_eq!(lit.shape(state), RenderShape::None);
            assert_eq!(lit.collision(state), &BlockCollision::None);
            assert!(lit.keeps_fluids_out(state), "{name}");
        }
    }
}

#[test]
fn lit_caves_puts_a_source_under_each_roof_of_the_even_columns_in_its_layers() {
    let registry = blocks::registry_with_sky_source();
    let source = registry.by_name(SKY_SOURCE).expect("the source");
    let inner: Arc<dyn WorldGenerator> = Arc::new(OverworldGenerator::new(
        DEFAULT_SEED,
        OverworldSettings::default(),
    ));
    // The layers around y = -1000 (chunk -32), as stress's lit cavern.
    let lit = LitCaves::around(Arc::clone(&inner), -32, 2);
    assert_eq!(lit.sky_source_layers(), -34..=-30);
    assert_eq!(WorldGenerator::name(&lit), inner.name());
    assert_eq!(lit.vertical_chunks(), inner.vertical_chunks());
    let mut sources = 0;
    for (x, z) in [(0, 0), (20, 0), (0, -3), (5, 7)] {
        for y in -35..=-29 {
            let key = ChunkKey::new(ChunkPos::new(x, y, z));
            let (plain, chunk) = (inner.generate(key, &registry), lit.generate(key, &registry));
            let o = key.pos.origin();
            for lx in 0..32u32 {
                for lz in 0..32u32 {
                    let even = (o.x + i64::from(lx) + o.z + i64::from(lz)).rem_euclid(2) == 0;
                    for ly in 0..32u32 {
                        let p = LocalPos::new(lx, ly, lz);
                        let roof = ly < 31
                            && plain.get(p).is_air()
                            && registry.occludes(plain.get(LocalPos::new(lx, ly + 1, lz)));
                        let want = if roof && even && (-34..=-30).contains(&y) {
                            sources += 1;
                            source
                        } else {
                            plain.get(p)
                        };
                        assert_eq!(chunk.get(p), want, "{key:?} {p:?}");
                    }
                }
            }
        }
    }
    assert!(sources > 1000, "the caves get sources: {sources}");
    // Without the block in the registry, the chunks are the inner world's.
    let plain = blocks::registry();
    let key = ChunkKey::new(ChunkPos::new(0, -32, 0));
    assert_eq!(lit.generate(key, &plain), inner.generate(key, &plain));
}
