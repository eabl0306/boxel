//! Interest math against brute-force definitions, the work lists of each kind of viewer change,
//! and a property test: applying the planner's positions after random viewer changes always
//! gives the from-scratch wanted set.
use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

/// The level a viewer gives `p`, straight from the definitions in the module docs.
pub(crate) fn reference_level(v: &Viewer, hysteresis: u32, p: ChunkPos) -> Level {
    let r = i64::from(v.radius);
    let (vv, h) = (i64::from(v.vertical), i64::from(hysteresis));
    let c = v.center;
    let (dx, dy, dz) = (
        i64::from(p.x) - i64::from(c.x),
        i64::from(p.y) - i64::from(c.y),
        i64::from(p.z) - i64::from(c.z),
    );
    let mesh_column = |dx: i64, dz: i64| dx * dx + dz * dz <= r * r;
    let d2 = dx * dx + dz * dz;
    let column = if mesh_column(dx, dz) {
        Level::Mesh
    } else if d2 <= (r + 1) * (r + 1)
        || (-1..=1).any(|a: i64| (-1..=1).any(|b: i64| mesh_column(dx + a, dz + b)))
    {
        Level::Data
    } else if d2 <= (r + 1 + h) * (r + 1 + h) {
        Level::Keep
    } else {
        Level::Out
    };
    let layer = if dy.abs() <= vv {
        Level::Mesh
    } else if dy.abs() <= vv + 1 {
        Level::Data
    } else if dy.abs() <= vv + 1 + h {
        Level::Keep
    } else {
        Level::Out
    };
    column.min(layer)
}

/// Best level over `viewers`, `Out` outside `bounds`.
pub(crate) fn reference_world_level(
    viewers: &[Viewer],
    hysteresis: u32,
    bounds: Option<&RangeInclusive<i32>>,
    p: ChunkPos,
) -> Level {
    if bounds.is_some_and(|b| !b.contains(&p.y)) {
        return Level::Out;
    }
    viewers
        .iter()
        .map(|v| reference_level(v, hysteresis, p))
        .max()
        .unwrap_or(Level::Out)
}

/// Every position at keep level or better for one viewer (brute force over its box).
fn reference_region(v: &Viewer, hysteresis: u32) -> BTreeMap<ChunkPos, Level> {
    let reach = i32::try_from(v.radius + 2 + hysteresis).expect("small");
    let vreach = i32::try_from(v.vertical + 2 + hysteresis).expect("small");
    let c = v.center;
    let mut out = BTreeMap::new();
    for y in c.y - vreach..=c.y + vreach {
        for z in c.z - reach..=c.z + reach {
            for x in c.x - reach..=c.x + reach {
                let p = ChunkPos::new(x, y, z);
                let level = reference_level(v, hysteresis, p);
                if level > Level::Out {
                    out.insert(p, level);
                }
            }
        }
    }
    out
}

fn shape(v: Viewer, hysteresis: u32) -> Shape {
    Shape::new(v, Arc::new(Columns::new(v.radius, hysteresis)))
}

fn drain(mut task: Task) -> Vec<ChunkPos> {
    let mut out = VecDeque::new();
    let mut calls = 0;
    while !task.fill(&mut out) {
        calls += 1;
        assert!(calls < 1_000_000, "task never finishes");
    }
    out.into_iter().collect()
}

fn viewer(x: i32, y: i32, z: i32, radius: u32, vertical: u32) -> Viewer {
    Viewer::new(ChunkPos::new(x, y, z), radius, vertical)
}

#[test]
fn columns_match_the_definitions() {
    for radius in 0..=20 {
        for h in 0..=3 {
            let columns = Columns::new(radius, h);
            let reach = i64::from(radius + h) + 3;
            for dz in -reach..=reach {
                for dx in -reach..=reach {
                    let p = ChunkPos::new(
                        i32::try_from(dx).expect("small"),
                        0,
                        i32::try_from(dz).expect("small"),
                    );
                    // A huge vertical radius makes the layer level Mesh: the column level shows.
                    let v = viewer(0, 0, 0, radius, 1000);
                    assert_eq!(
                        columns.level(dx, dz),
                        reference_level(&v, h, p),
                        "radius {radius} hysteresis {h} column ({dx}, {dz})"
                    );
                }
            }
        }
    }
}

#[test]
fn column_counts() {
    let count = |radius: u32, level: Level| {
        let c = Columns::new(radius, 2);
        let reach = i64::from(radius) + 4;
        let mut n = 0;
        for dz in -reach..=reach {
            for dx in -reach..=reach {
                if c.level(dx, dz) >= level {
                    n += 1;
                }
            }
        }
        n
    };
    // Radius 0: the 3x3 square of data around one mesh column.
    assert_eq!((count(0, Level::Mesh), count(0, Level::Data)), (1, 9));
    // Radius 1: the 5x5 square without its corners.
    assert_eq!((count(1, Level::Mesh), count(1, Level::Data)), (5, 21));
    // Gauss circle numbers N(12) = 441; the data set extends the radius-13 circle (N = 529),
    // for example with (1, 13), the diagonal neighbor of the mesh column (0, 12).
    assert_eq!(count(12, Level::Mesh), 441);
    assert!(count(12, Level::Data) > 529);
    let c = Columns::new(12, 2);
    assert_eq!(c.level(0, 12), Level::Mesh);
    assert_eq!(c.level(1, 13), Level::Data);
    assert_eq!(c.level(0, 14), Level::Keep);
    assert_eq!(c.level(0, 16), Level::Out);
}

#[test]
fn shape_levels_match_the_definitions() {
    for (radius, vertical, h) in [(0, 0, 0), (1, 0, 2), (3, 2, 1), (5, 1, 3), (2, 4, 0)] {
        let v = viewer(3, -2, 7, radius, vertical);
        let s = shape(v, h);
        let reach = i32::try_from(radius + h + 3).expect("small");
        let vreach = i32::try_from(vertical + h + 3).expect("small");
        for y in -2 - vreach..=-2 + vreach {
            for z in 7 - reach..=7 + reach {
                for x in 3 - reach..=3 + reach {
                    let p = ChunkPos::new(x, y, z);
                    assert_eq!(s.level(p), reference_level(&v, h, p), "{v:?} h {h} {p:?}");
                }
            }
        }
    }
}

#[test]
fn add_lists_the_data_region_nearest_shell_first() {
    for (radius, vertical) in [(0, 0), (2, 1), (4, 3), (6, 0)] {
        let v = viewer(-5, 4, 9, radius, vertical);
        let listed = drain(Task::add(shape(v, 2), None));
        let unique: BTreeSet<ChunkPos> = listed.iter().copied().collect();
        assert_eq!(unique.len(), listed.len(), "no duplicates");
        let expected: BTreeSet<ChunkPos> = reference_region(&v, 2)
            .into_iter()
            .filter(|&(_, l)| l >= Level::Data)
            .map(|(p, _)| p)
            .collect();
        assert_eq!(unique, expected, "{v:?}");
        let shell = |p: &ChunkPos| {
            let c = v.center;
            (p.x - c.x)
                .abs()
                .max((p.y - c.y).abs())
                .max((p.z - c.z).abs())
        };
        assert!(
            listed.windows(2).all(|w| shell(&w[0]) <= shell(&w[1])),
            "nearest shell first"
        );
        assert_eq!(listed.first(), Some(&v.center));
    }
    // Vertical bounds clip the region; bounds that miss it list nothing.
    let v = viewer(0, 0, 0, 2, 3);
    let bounds = -1..=1;
    let listed: BTreeSet<ChunkPos> = drain(Task::add(shape(v, 2), Some(&bounds)))
        .into_iter()
        .collect();
    let expected: BTreeSet<ChunkPos> = reference_region(&v, 2)
        .into_iter()
        .filter(|&(p, l)| l >= Level::Data && bounds.contains(&p.y))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(listed, expected);
    assert!(listed.iter().any(|p| p.y == 1) && listed.iter().any(|p| p.y == -1));
    assert!(drain(Task::add(shape(v, 2), Some(&(10..=12)))).is_empty());
}

#[test]
fn remove_lists_the_keep_region() {
    let v = viewer(2, 1, -3, 3, 1);
    let listed = drain(Task::remove(shape(v, 2), None));
    let unique: BTreeSet<ChunkPos> = listed.iter().copied().collect();
    assert_eq!(unique.len(), listed.len());
    let expected: BTreeSet<ChunkPos> = reference_region(&v, 2).into_keys().collect();
    assert_eq!(unique, expected);
}

/// Positions a move must list: level changed, except out -> keep.
fn reference_move(a: &Viewer, b: &Viewer, h: u32) -> BTreeSet<ChunkPos> {
    let ra = reference_region(a, h);
    let rb = reference_region(b, h);
    let level = |r: &BTreeMap<ChunkPos, Level>, p| r.get(p).copied().unwrap_or(Level::Out);
    ra.keys()
        .chain(rb.keys())
        .filter(|p| {
            let (la, lb) = (level(&ra, p), level(&rb, p));
            la != lb && !(la == Level::Out && lb == Level::Keep)
        })
        .copied()
        .collect()
}

#[test]
fn small_moves_list_only_the_changed_positions() {
    let base = viewer(0, 0, 0, 4, 2);
    for (dx, dy, dz) in [
        (1, 0, 0),
        (0, 1, 0),
        (0, 0, -1),
        (1, -1, 1),
        (4, 0, 0),
        (-3, 4, 2),
    ] {
        let to = viewer(dx, dy, dz, 4, 2);
        let listed = drain(Task::moved(shape(base, 2), shape(to, 2), None));
        let unique: BTreeSet<ChunkPos> = listed.iter().copied().collect();
        assert_eq!(unique.len(), listed.len(), "no duplicates");
        assert_eq!(
            unique,
            reference_move(&base, &to, 2),
            "move ({dx}, {dy}, {dz})"
        );
    }
    // One chunk in x at radius 4, vertical 2: a few slabs, far less than the view.
    let to = viewer(1, 0, 0, 4, 2);
    let listed = drain(Task::moved(shape(base, 2), shape(to, 2), None));
    let view = reference_region(&base, 2).len();
    assert!(listed.len() * 3 < view, "{} of {view}", listed.len());
}

#[test]
fn planner_pairs_viewers_by_id() {
    let mut p = Planner::default();
    let a = viewer(0, 0, 0, 2, 1);
    assert!(p.changed(&[(1, a)], 2, None));
    assert!(p.update(&[(1, a)], 2, None, Vec::new));
    assert!(!p.changed(&[(1, a)], 2, None));
    assert!(!p.update(&[(1, a)], 2, None, Vec::new), "nothing changed");
    let first: BTreeSet<ChunkPos> = std::iter::from_fn(|| p.next()).collect();
    assert_eq!(p.backlog(), 0);
    assert_eq!(
        first.len(),
        reference_region(&a, 2)
            .values()
            .filter(|&&l| l >= Level::Data)
            .count()
    );

    // A one-chunk move lists a slab, not the view.
    let moved = viewer(1, 0, 0, 2, 1);
    assert!(p.update(&[(1, moved)], 2, None, Vec::new));
    let listed: Vec<ChunkPos> = std::iter::from_fn(|| p.next()).collect();
    assert_eq!(
        listed.iter().copied().collect::<BTreeSet<_>>(),
        reference_move(&a, &moved, 2)
    );

    // Levels and priorities use the current viewers.
    assert_eq!(p.level(ChunkPos::new(1, 0, 0)), Level::Mesh);
    assert_eq!(p.level(ChunkPos::new(1, 3, 0)), Level::Keep);
    assert_eq!(p.priority(ChunkPos::new(3, 1, 0)), 5);

    // A settings change re-lists every loaded slot plus the data regions.
    let loaded = vec![ChunkPos::new(50, 0, 0), ChunkPos::new(51, 0, 0)];
    let bounds = 0..=0;
    assert!(p.update(&[(1, moved)], 2, Some(&bounds), || loaded.clone()));
    let listed: BTreeSet<ChunkPos> = std::iter::from_fn(|| p.next()).collect();
    assert!(loaded.iter().all(|l| listed.contains(l)));
    assert!(listed.iter().filter(|q| q.x < 50).all(|q| q.y == 0));
    assert_eq!(
        p.level(ChunkPos::new(1, 1, 0)),
        Level::Out,
        "outside the bounds"
    );
    assert!(p.in_bounds(0) && !p.in_bounds(1));

    // No viewers: everything is out, priorities are the max.
    assert!(p.update(&[], 2, Some(&bounds), Vec::new));
    assert_eq!(p.level(ChunkPos::new(1, 0, 0)), Level::Out);
    assert_eq!(p.priority(ChunkPos::new(1, 0, 0)), u32::MAX);
}

#[test]
fn capacity_hint_is_the_keep_region() {
    let mut p = Planner::default();
    assert_eq!(p.capacity_hint(), 0);
    let a = viewer(0, 0, 0, 3, 1);
    let b = viewer(40, 0, 0, 1, 0);
    p.update(&[(1, a), (2, b)], 2, None, Vec::new);
    assert_eq!(
        p.capacity_hint(),
        reference_region(&a, 2).len() + reference_region(&b, 2).len()
    );
    // Clipped to the vertical bounds: one layer.
    let bounds = 0..=0;
    p.update(&[(1, a)], 2, Some(&bounds), Vec::new);
    let one_layer = reference_region(&a, 2).keys().filter(|q| q.y == 0).count();
    assert_eq!(p.capacity_hint(), one_layer);
    // Capped.
    p.update(&[(1, viewer(0, 0, 0, 200, 200))], 2, None, Vec::new);
    assert_eq!(p.capacity_hint(), MAX_CAPACITY_HINT);
}

#[test]
fn viewers_are_clamped() {
    let v = Viewer::new(ChunkPos::new(i32::MAX, i32::MIN, 0), u32::MAX, u32::MAX);
    assert_eq!(v.radius, MAX_RADIUS);
    assert_eq!(v.vertical, MAX_RADIUS);
    assert_eq!(v.center, ChunkPos::new(MAX_CENTER, -MAX_CENTER, 0));
}

/// A model of the world's slot set: interest per loaded position.
#[derive(Default)]
struct Model {
    loaded: BTreeMap<ChunkPos, Interest>,
}

impl Model {
    /// What `WorldJobs::refresh_position` does, without slot states (nothing is ever `Queued`).
    fn refresh(&mut self, planner: &Planner, p: ChunkPos) {
        let level = planner.level(p);
        let want = level.interest();
        if !want.is_empty() {
            self.loaded.insert(p, want);
        } else if level == Level::Out {
            self.loaded.remove(&p);
        } else if let Some(interest) = self.loaded.get_mut(&p) {
            *interest = want;
        }
    }
}

/// One step of a random walk: which viewer, and what happens to it.
#[derive(Clone, Debug)]
enum Step {
    Move(u8, i32, i32, i32),
    Jump(u8, i32, i32, i32),
    Radius(u8, u32, u32),
    Toggle(u8),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        6 => (0u8..2, -2i32..=2, -2i32..=2, -2i32..=2).prop_map(|(i, x, y, z)| Step::Move(i, x, y, z)),
        1 => (0u8..2, -20i32..=20, -20i32..=20, -20i32..=20).prop_map(|(i, x, y, z)| Step::Jump(i, x, y, z)),
        1 => (0u8..2, 0u32..=4, 0u32..=3).prop_map(|(i, r, v)| Step::Radius(i, r, v)),
        1 => (0u8..2).prop_map(Step::Toggle),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// After every change and a full drain of the planner, the modeled slot set equals the
    /// from-scratch wanted set, and every kept unwanted slot is inside some keep region.
    #[test]
    fn incremental_interest_matches_from_scratch(
        steps in proptest::collection::vec(step(), 1..24),
        hysteresis in 0u32..=2,
        bounded in any::<bool>(),
    ) {
        let bounds = bounded.then_some(-3..=2);
        let mut planner = Planner::default();
        let mut model = Model::default();
        let mut viewers: [Option<Viewer>; 2] = [Some(viewer(0, 0, 0, 2, 1)), None];
        for step in steps {
            match step {
                Step::Move(i, x, y, z) | Step::Jump(i, x, y, z) => {
                    let jump = matches!(step, Step::Jump(..));
                    let slot = &mut viewers[usize::from(i)];
                    if let Some(v) = slot {
                        let c = if jump { ChunkPos::new(x, y, z) } else { v.center.offset(ChunkPos::new(x, y, z).as_ivec3()) };
                        *v = Viewer::new(c, v.radius, v.vertical);
                    }
                }
                Step::Radius(i, r, vr) => {
                    if let Some(v) = &mut viewers[usize::from(i)] {
                        *v = Viewer::new(v.center, r, vr);
                    }
                }
                Step::Toggle(i) => {
                    let slot = &mut viewers[usize::from(i)];
                    *slot = match slot {
                        Some(_) => None,
                        None => Some(viewer(3, 1, -2, 1, 1)),
                    };
                }
            }
            let list: Vec<(u64, Viewer)> = viewers
                .iter()
                .enumerate()
                .filter_map(|(i, v)| v.map(|v| (i as u64, v)))
                .collect();
            let loaded: Vec<ChunkPos> = model.loaded.keys().copied().collect();
            planner.update(&list, hysteresis, bounds.as_ref(), || loaded);
            while let Some(p) = planner.next() {
                model.refresh(&planner, p);
            }
            prop_assert_eq!(planner.backlog(), 0);

            let current: Vec<Viewer> = list.iter().map(|&(_, v)| v).collect();
            let mut wanted = BTreeMap::new();
            for v in &current {
                for (p, _) in reference_region(v, hysteresis) {
                    let level = reference_world_level(&current, hysteresis, bounds.as_ref(), p);
                    if level >= Level::Data {
                        wanted.insert(p, level.interest());
                    }
                }
            }
            let model_wanted: BTreeMap<ChunkPos, Interest> = model
                .loaded
                .iter()
                .filter(|(_, i)| !i.is_empty())
                .map(|(&p, &i)| (p, i))
                .collect();
            prop_assert_eq!(&model_wanted, &wanted);
            for (p, interest) in &model.loaded {
                if interest.is_empty() {
                    let level = reference_world_level(&current, hysteresis, bounds.as_ref(), *p);
                    prop_assert_eq!(level, Level::Keep, "{:?} kept outside every keep region", p);
                }
            }
        }
    }
}
