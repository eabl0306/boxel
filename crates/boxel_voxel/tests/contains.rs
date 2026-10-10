//! `ChunkData::may_contain`: a palette scan for a `StateSet` (physics asks it with the states
//! whose collision reaches into the cell above, ADR 0026), never a miss, and cached per set
//! until the blocks change on any path (`set`, `blocks_mut`, `compact`).
use boxel_voxel::*;
use proptest::prelude::*;

const STONE: BlockState = BlockState(1);
const FENCE: BlockState = BlockState(40);
const POST: BlockState = BlockState(300);

fn local(i: usize) -> LocalPos {
    LocalPos::from_index(i)
}

/// Whether some voxel of `chunk` holds a state of `set`, from its dense blocks.
fn holds(chunk: &ChunkData, set: &StateSet) -> bool {
    let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
    chunk.to_dense(&mut dense);
    dense.iter().any(|&s| set.contains(s))
}

/// The palette scan `may_contain` answers.
fn scan(chunk: &ChunkData, set: &StateSet) -> bool {
    match chunk.blocks() {
        Paletted::Uniform(s) => set.contains(*s),
        Paletted::Packed { palette, .. } => palette.iter().any(|&s| set.contains(s)),
        Paletted::Direct(_) => !set.is_empty(),
    }
}

#[test]
fn a_state_set_holds_its_states_and_its_own_key() {
    let set = StateSet::new([FENCE, POST]);
    assert!(set.contains(FENCE) && set.contains(POST));
    assert!(!set.contains(STONE) && !set.contains(BlockState(u16::MAX)));
    assert!(!set.is_empty() && StateSet::default().is_empty());
    // A clone is the same set (same key); a new set of the same states is equal but keyed
    // apart, so a chunk never answers one with the other's cached answer.
    let again = StateSet::new([POST, FENCE]);
    assert_eq!(set.clone().key(), set.key());
    assert_eq!(again, set);
    assert_ne!(again.key(), set.key());
}

#[test]
fn may_contain_scans_the_palette() {
    let fences = StateSet::new([FENCE]);
    assert!(!ChunkData::uniform(STONE).may_contain(&fences));
    assert!(ChunkData::uniform(FENCE).may_contain(&fences));
    let mut chunk = ChunkData::uniform(BlockState::AIR);
    chunk.set(local(5), STONE);
    assert!(!chunk.may_contain(&fences));
    chunk.set(local(9), FENCE);
    assert!(chunk.may_contain(&fences));
    // Removed, the fence stays in the palette until a compaction: a yes that may be stale is
    // fine, a no never is.
    chunk.set(local(9), BlockState::AIR);
    assert!(chunk.may_contain(&fences));
    chunk.compact();
    assert!(!chunk.may_contain(&fences));
    // Direct storage (more than 256 states) answers yes, except for the empty set.
    let direct: Vec<BlockState> = (0..CHUNK_VOLUME).map(|i| BlockState(i as u16)).collect();
    let direct = ChunkData::from_dense(&direct);
    assert!(matches!(direct.blocks(), Paletted::Direct(_)));
    assert!(direct.may_contain(&StateSet::new([BlockState(60_000)])));
    assert!(!direct.may_contain(&StateSet::default()));
    assert!(!chunk.may_contain(&StateSet::default()));
}

#[test]
fn the_cached_answer_follows_every_change() {
    let fences = StateSet::new([FENCE]);
    let stones = StateSet::new([STONE]);
    let mut chunk = ChunkData::uniform(BlockState::AIR);
    chunk.set(local(0), STONE);
    // Two sets asked in turns keep their own answers.
    for _ in 0..2 {
        assert!(!chunk.may_contain(&fences));
        assert!(chunk.may_contain(&stones));
    }
    // A cached no turns into a yes with the edit that adds a fence.
    assert!(!chunk.may_contain(&fences));
    chunk.set(local(1), FENCE);
    assert!(chunk.may_contain(&fences));
    // A clone shares the blocks and the answers; a change to it changes only its own.
    let mut copy = chunk.clone();
    assert!(copy.may_contain(&fences));
    *copy.blocks_mut() = Paletted::encode(&vec![STONE; CHUNK_VOLUME]);
    assert!(!copy.may_contain(&fences));
    assert!(chunk.may_contain(&fences));
    // In place through `blocks_mut`, without a version bump: still dropped.
    assert!(!copy.may_contain(&fences));
    copy.blocks_mut().set(local(7), FENCE);
    assert!(copy.may_contain(&fences));
    // Equality ignores the cache (the version is compared: the fresh chunk takes the copy's).
    let mut fresh = ChunkData::from_paletted(copy.blocks().clone());
    fresh.version = copy.version;
    assert_eq!(fresh, copy);
}

/// One edit of the random chunk below: a voxel, its new state, and whether to compact after.
fn edit() -> impl Strategy<Value = (usize, u16, bool)> {
    (
        0..CHUNK_VOLUME,
        prop_oneof![Just(0u16), Just(1), Just(40), Just(300), 2..20u16],
        prop::bool::weighted(0.1),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Random edits (and compactions) with both sets asked in between: the answer is always
    /// the palette scan of the blocks as they are now, so it never misses a state that is there.
    #[test]
    fn may_contain_never_misses_and_never_goes_stale(
        edits in prop::collection::vec(edit(), 1..64),
    ) {
        let sets = [StateSet::new([FENCE]), StateSet::new([FENCE, POST])];
        let mut chunk = ChunkData::uniform(BlockState::AIR);
        for (i, (at, state, compact)) in edits.into_iter().enumerate() {
            chunk.set(local(at), BlockState(state));
            if compact {
                chunk.compact();
            }
            for set in sets.iter().cycle().skip(i % 2).take(3) {
                prop_assert_eq!(chunk.may_contain(set), scan(&chunk, set));
                prop_assert!(chunk.may_contain(set) || !holds(&chunk, set));
            }
        }
    }
}
