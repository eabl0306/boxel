//! When chunk memory grows and shrinks (ADR 0035): a pure function of a sample of the pages and
//! a clock, so the tests drive it with a fake one. Growth keeps a reserve of free quads (plus
//! what refused chunks wait for) and chunk places per class and asks for one page of the class
//! at a time; the table is asked for ahead, when its free slots run low. Shrink waits until a
//! class could lose its emptiest growth page and still keep that reserve plus half a page of
//! quads, and its chunk headroom plus half a page of places, held for [`SHRINK_HOLD`], so growth
//! (below the reserve) and shrink (above it by half a page) never meet; never below the floor,
//! counted over both classes.
use super::PageClass;
use std::time::{Duration, Instant};

/// After a failed creation, the class (or the table) does not grow for this long.
pub(crate) const FAILURE_BACKOFF: Duration = Duration::from_secs(10);
/// Shrink starts once its condition held this long.
pub(crate) const SHRINK_HOLD: Duration = Duration::from_secs(5);
/// No shrink of a class this long after one of its pages was created.
pub(crate) const SHRINK_AFTER_CREATE: Duration = Duration::from_secs(10);
/// A victim whose evacuation moved nothing this long goes back to Active (nothing fits
/// elsewhere: fragmentation).
pub(crate) const EVACUATION_STALL: Duration = Duration::from_secs(2);
/// A creation between frames slower than this (a new driver memory block) doubles the class's
/// reserve once, so its pages are asked for earlier.
pub(crate) const SLOW_CREATE: Duration = Duration::from_millis(2);

/// A page in a [`ClassSample`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PageSample {
    pub id: u16,
    /// Its quads.
    pub quads: u32,
    /// Its used quads.
    pub used: u64,
    /// Chunks resident in it.
    pub chunks: u32,
}

/// One class's part of a [`Sample`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClassSample {
    /// Resident chunks of the class, wherever they sit.
    pub chunks: u32,
    /// Their quads.
    pub used: u64,
    /// Pages of the class, in any state.
    pub pages: u32,
    /// Free quads of its Active and Clearing pages.
    pub free: u64,
    /// Free chunk places of its Active and Clearing pages.
    pub places: u64,
    /// A page of the class is Clearing (asked for, not usable yet).
    pub clearing: bool,
    /// Its Active page of the growth size with the fewest used quads, ties to the highest id
    /// (shrink's victim). A smaller floor page is never one: growth could not make it again, so
    /// memory would settle above the floor.
    pub emptiest: Option<PageSample>,
    /// Its Draining page, with what is still in it.
    pub draining: Option<PageSample>,
    /// Quads of its refused chunks waiting for room (growth makes room for them too).
    pub waiting: u64,
    /// One of them fits no free range of any page (the free quads are cut in pieces too small:
    /// growth is the way out).
    pub blocked: bool,
}

/// What the policy looks at, taken at the end of each frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sample {
    /// Per class (`PageClass::index`).
    pub classes: [ClassSample; 2],
    /// Quads of a growth page.
    pub page_quads: u32,
    /// Chunks a page holds at most.
    pub page_chunks: u32,
    /// Pages, all classes and states.
    pub pages: u32,
    /// Quads of all pages.
    pub capacity: u64,
    /// The ceiling in quads, and the most pages.
    pub ceiling_quads: u64,
    /// The most pages.
    pub max_pages: u32,
    /// The floor: quads and pages created at GPU start (shrink never goes below them).
    pub floor_quads: u64,
    /// Pages of the floor.
    pub floor_pages: u32,
    /// Slots of the table in use, and its live chunks.
    pub slots: u32,
    /// Chunks in the table.
    pub used_slots: u32,
    /// The most slots.
    pub max_slots: u32,
    /// A larger table is being cleared or waits to be created.
    pub table_pending: bool,
}

/// What the policy asks for after a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Decision {
    /// A page of the class (`PageClass::index`), created between frames.
    pub want: [bool; 2],
    /// A table of this many slots, created between frames.
    pub want_table: Option<u32>,
    /// Start draining this page of the class.
    pub drain: [Option<u16>; 2],
    /// The class's Draining page goes back to Active.
    pub cancel: [bool; 2],
}

/// One class's history.
#[derive(Clone, Copy, Debug, Default)]
struct ClassState {
    /// The last failed creation.
    failed: Option<Instant>,
    /// The last page created.
    created: Option<Instant>,
    /// A slow creation doubled the reserve (kept for the session).
    doubled: bool,
    /// Since when the shrink condition holds.
    shrink_since: Option<Instant>,
    /// When the Draining page last gave up a chunk (or started draining).
    progress: Option<Instant>,
}

/// The growth and shrink policy of one renderer (ADR 0035).
#[derive(Clone, Debug, Default)]
pub(crate) struct MemoryPolicy {
    class: [ClassState; 2],
    /// The last failed table creation.
    table_failed: Option<Instant>,
}

impl MemoryPolicy {
    /// Free quads the class keeps ahead once it has a chunk (or one waits for room): half a page
    /// for Solid, a quarter for Translucent, or an eighth of what it uses if more; twice that
    /// after a slow creation.
    pub fn reserve(&self, class: PageClass, sample: &Sample) -> u64 {
        let c = &sample.classes[class.index()];
        if c.chunks == 0 && c.waiting == 0 {
            return 0;
        }
        let page = u64::from(sample.page_quads);
        let base = match class {
            PageClass::Solid => page / 2,
            PageClass::Translucent => page / 4,
        };
        let reserve = base.max(c.used / 8);
        if self.class[class.index()].doubled {
            2 * reserve
        } else {
            reserve
        }
    }

    /// Free chunk places the class keeps ahead once it has a chunk: a quarter of a page's, or
    /// an eighth of its chunks if more.
    pub fn headroom(class: PageClass, sample: &Sample) -> u64 {
        let c = &sample.classes[class.index()];
        if c.chunks == 0 {
            return 0;
        }
        u64::from((sample.page_chunks / 4).max(1)).max(u64::from(c.chunks / 8))
    }

    /// True unless a creation of the class failed in the last [`FAILURE_BACKOFF`].
    pub fn can_create(&self, class: PageClass, now: Instant) -> bool {
        self.class[class.index()]
            .failed
            .is_none_or(|t| now.saturating_duration_since(t) >= FAILURE_BACKOFF)
    }

    /// True unless a table creation failed in the last [`FAILURE_BACKOFF`].
    pub fn can_grow_table(&self, now: Instant) -> bool {
        self.table_failed
            .is_none_or(|t| now.saturating_duration_since(t) >= FAILURE_BACKOFF)
    }

    /// A page of `class` fits under the ceiling of `sample`.
    pub fn under_ceiling(sample: &Sample) -> bool {
        sample.pages < sample.max_pages
            && sample.capacity + u64::from(sample.page_quads) <= sample.ceiling_quads
    }

    /// A page of `class` was created at `now` in `took`. True the first time a creation is
    /// slower than [`SLOW_CREATE`]: the class's reserve doubles for the session (warn once).
    pub fn created(&mut self, class: PageClass, now: Instant, took: Duration) -> bool {
        let c = &mut self.class[class.index()];
        c.created = Some(now);
        c.shrink_since = None;
        if took > SLOW_CREATE && !c.doubled {
            c.doubled = true;
            return true;
        }
        false
    }

    /// A creation of `class` failed at `now`: the class does not grow for [`FAILURE_BACKOFF`].
    pub fn failed(&mut self, class: PageClass, now: Instant) {
        self.class[class.index()].failed = Some(now);
    }

    /// A table creation failed at `now`.
    pub fn table_failed(&mut self, now: Instant) {
        self.table_failed = Some(now);
    }

    /// The Draining page of `class` gave up chunks at `now` (or started draining).
    pub fn progressed(&mut self, class: PageClass, now: Instant) {
        self.class[class.index()].progress = Some(now);
    }

    /// The world closed: shrink's holds and victims go; what the device taught (failures, the
    /// doubled reserves) stays.
    pub fn forget_world(&mut self) {
        for c in &mut self.class {
            c.shrink_since = None;
            c.progress = None;
            c.created = None;
        }
    }

    /// What to do after a frame with `sample`, at `now`; `grow`: growth and shrink are on.
    pub fn decide(&mut self, sample: &Sample, now: Instant, grow: bool) -> Decision {
        let mut decision = Decision::default();
        if !grow {
            return decision;
        }
        let page = u64::from(sample.page_quads);
        let page_chunks = u64::from(sample.page_chunks);
        let over = sample.capacity > sample.ceiling_quads;
        // The floor (and the ceiling a victim is picked against) counts what stays once the
        // Draining pages and the victims picked below are gone, over both classes: each class
        // alone could pass it and the two together go under.
        let (mut capacity, mut pages) = (sample.capacity, sample.pages);
        for d in sample.classes.iter().filter_map(|c| c.draining) {
            capacity = capacity.saturating_sub(u64::from(d.quads));
            pages = pages.saturating_sub(1);
        }
        for class in PageClass::ALL {
            let i = class.index();
            let c = sample.classes[i];
            let reserve = self.reserve(class, sample);
            let headroom = Self::headroom(class, sample);
            // growth
            let short = c.free < reserve + c.waiting || c.blocked || c.places < headroom;
            // (a class short of room cancels its victim below, so growth never waits for one)
            decision.want[i] =
                short && !c.clearing && Self::under_ceiling(sample) && self.can_create(class, now);
            // shrink: a victim draining goes on, or back to Active
            let state = &mut self.class[i];
            if let Some(d) = c.draining {
                // what the class keeps once the victim's chunks moved in: its quads and places
                let pressure = !over
                    && (c.free.saturating_sub(d.used) < reserve + c.waiting
                        || c.places.saturating_sub(u64::from(d.chunks)) < headroom);
                let stalled = state
                    .progress
                    .is_some_and(|t| now.saturating_duration_since(t) >= EVACUATION_STALL);
                if pressure || stalled {
                    decision.cancel[i] = true;
                    state.shrink_since = None;
                    capacity += u64::from(d.quads);
                    pages += 1;
                }
                continue;
            }
            let victim = c.emptiest.filter(|v| {
                // the other pages' free quads, then what they keep once the victim's chunks
                // moved in
                let left = c.free.saturating_sub(u64::from(v.quads) - v.used);
                let others = left.saturating_sub(v.used);
                // its free chunk places go and its chunks take places elsewhere: a page's
                // worth fewer (a class can grow for places alone, with quads to spare)
                let places = c.places.saturating_sub(page_chunks);
                let floor = capacity.saturating_sub(u64::from(v.quads)) >= sample.floor_quads
                    && pages > sample.floor_pages;
                let recent = state
                    .created
                    .is_some_and(|t| now.saturating_duration_since(t) < SHRINK_AFTER_CREATE);
                c.pages >= 2
                    && floor
                    && !c.clearing
                    && if capacity > sample.ceiling_quads {
                        // past a lowered ceiling: as soon as the victim's chunks fit elsewhere
                        left >= v.used && c.places >= page_chunks
                    } else {
                        // the waiting chunks count as for growth, and half a page of quads
                        // and of places more: the two never meet
                        !recent
                            && others >= reserve + c.waiting + page / 2
                            && places >= headroom + page_chunks / 2
                    }
            });
            match victim {
                Some(v) => {
                    let since = *state.shrink_since.get_or_insert(now);
                    if now.saturating_duration_since(since) >= SHRINK_HOLD {
                        decision.drain[i] = Some(v.id);
                        state.shrink_since = None;
                        state.progress = Some(now);
                        capacity -= u64::from(v.quads);
                        pages -= 1;
                    }
                }
                None => state.shrink_since = None,
            }
        }
        // the table, ahead
        let free_slots = sample.slots - sample.used_slots.min(sample.slots);
        if sample.used_slots > 0
            && free_slots < (sample.slots / 8).max(4096)
            && sample.slots < sample.max_slots
            && !sample.table_pending
            && self.can_grow_table(now)
        {
            decision.want_table = Some(sample.slots.saturating_mul(2).min(sample.max_slots));
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: u32 = 1 << 20;
    const PAGE_CHUNKS: u32 = 400;

    /// A page of a [`Fake`].
    #[derive(Clone, Copy)]
    struct FakePage {
        quads: u32,
        used: u64,
        chunks: u32,
        /// 0 Clearing, 1 Active, 2 Draining.
        state: u8,
    }

    impl FakePage {
        fn new(used: u64, chunks: u32, state: u8) -> Self {
            Self {
                quads: PAGE,
                used,
                chunks,
                state,
            }
        }
    }

    /// A fake memory driven by a scripted demand: pages of `PAGE` quads and `PAGE_CHUNKS`
    /// chunks, one class, chunks of `chunk` quads.
    struct Fake {
        policy: MemoryPolicy,
        now: Instant,
        pages: Vec<FakePage>,
        floor: u32,
        /// Quads of one chunk: a hundredth of a page by default (a full page holds 101 chunks
        /// of its 400 places: quads bind).
        chunk: u64,
        created: u32,
        released: u32,
    }

    impl Fake {
        fn new(floor: u32) -> Self {
            Self {
                policy: MemoryPolicy::default(),
                now: Instant::now(),
                pages: vec![FakePage::new(0, 0, 1); floor as usize],
                floor,
                chunk: u64::from(PAGE / 100),
                created: 0,
                released: 0,
            }
        }

        /// What `sample_classes` would see.
        fn sample(&self) -> Sample {
            let mut c = ClassSample {
                pages: self.pages.len() as u32,
                ..ClassSample::default()
            };
            for (id, p) in self.pages.iter().enumerate() {
                c.chunks += p.chunks;
                c.used += p.used;
                let places = u64::from(PAGE_CHUNKS - p.chunks);
                let page = PageSample {
                    id: id as u16,
                    quads: p.quads,
                    used: p.used,
                    chunks: p.chunks,
                };
                match p.state {
                    0 => {
                        c.clearing = true;
                        c.free += u64::from(p.quads);
                        c.places += places;
                    }
                    1 => {
                        c.free += u64::from(p.quads) - p.used;
                        c.places += places;
                        if p.quads == PAGE && c.emptiest.is_none_or(|e| p.used <= e.used) {
                            c.emptiest = Some(page);
                        }
                    }
                    _ => c.draining = Some(page),
                }
            }
            Sample {
                classes: [c, ClassSample::default()],
                page_quads: PAGE,
                page_chunks: PAGE_CHUNKS,
                pages: self.pages.len() as u32,
                capacity: self.pages.iter().map(|p| u64::from(p.quads)).sum(),
                ceiling_quads: 64 * u64::from(PAGE),
                max_pages: 64,
                floor_quads: u64::from(self.floor) * u64::from(PAGE),
                floor_pages: self.floor,
                slots: 1 << 20,
                used_slots: 0,
                max_slots: 1 << 20,
                table_pending: false,
            }
        }

        /// One frame of `used` quads in chunks of `chunk` quads, spread lowest page first;
        /// returns the pages.
        fn frame(&mut self, used: u64) -> usize {
            self.now += Duration::from_millis(16);
            // a Clearing page is ready the next frame; a Draining page empties at once
            for p in &mut self.pages {
                if p.state == 0 {
                    p.state = 1;
                }
            }
            self.pages.retain(|p| p.state != 2);
            let mut left = used;
            for p in &mut self.pages {
                let take = left
                    .min(u64::from(p.quads))
                    .min(u64::from(PAGE_CHUNKS) * self.chunk);
                p.used = take;
                p.chunks = take.div_ceil(self.chunk) as u32;
                left -= take;
            }
            assert_eq!(left, 0, "the scripted demand outgrew the pages");
            let sample = self.sample();
            let d = self.policy.decide(&sample, self.now, true);
            if d.want[0] {
                self.pages.push(FakePage::new(0, 0, 0));
                self.created += 1;
                self.policy
                    .created(PageClass::Solid, self.now, Duration::ZERO);
            }
            if let Some(id) = d.drain[0] {
                self.pages[usize::from(id)].state = 2;
                self.released += 1;
            }
            self.pages.len()
        }
    }

    #[test]
    fn a_radius_swing_grows_and_shrinks_back_to_the_floor() {
        let mut fake = Fake::new(2);
        // r8: a page's worth
        for _ in 0..100 {
            fake.frame(u64::from(PAGE));
        }
        assert_eq!(fake.pages.len(), 2, "r8 stays on the floor");
        // r32: ten pages' worth, growing a quarter page a frame
        let mut used = u64::from(PAGE);
        while used < 10 * u64::from(PAGE) {
            used += u64::from(PAGE) / 4;
            fake.frame(used);
        }
        for _ in 0..100 {
            fake.frame(used);
        }
        let peak = fake.pages.len();
        assert!((11..=13).contains(&peak), "r32 grew to {peak} pages");
        // back to r8 at once; shrink takes its time (5 s holds, one victim at a time)
        for _ in 0..(60 * 120) {
            fake.frame(u64::from(PAGE));
        }
        assert_eq!(fake.pages.len(), 2, "back to the floor");
        assert!(fake.created <= 12 && fake.released <= 12, "bounded");
    }

    /// Small chunks fill a page's chunk places long before its quads: the class grows for
    /// places alone, its newest page holds no chunk and plenty of quads. Shrink must not take
    /// that page back (it used to look at quads only: the page drained, places ran short and
    /// growth made it again, every 15 s). A swing down still returns to the floor.
    #[test]
    fn pages_grown_for_chunk_places_stay_until_the_chunks_go() {
        let mut fake = Fake::new(2);
        // 524 quads a chunk: a page holds 400 chunks, a fifth of its quads
        fake.chunk = u64::from(PAGE / 2000);
        let chunks = |n: u64| n * u64::from(PAGE / 2000);
        // up to 2,000 chunks, 50 a frame
        let mut n = 0;
        while n < 2000 {
            n += 50;
            fake.frame(chunks(n));
        }
        // two minutes there: nothing moves once settled
        for _ in 0..(60 * 30) {
            fake.frame(chunks(n));
        }
        let (pages, created, released) = (fake.pages.len(), fake.created, fake.released);
        assert!(
            pages >= 6,
            "{pages} pages hold 2,000 chunks of 400 a page with headroom"
        );
        for _ in 0..(60 * 90) {
            fake.frame(chunks(n));
        }
        assert_eq!(
            (fake.pages.len(), fake.created, fake.released),
            (pages, created, released),
            "growth and shrink undo each other"
        );
        // most chunks go: back to the floor
        for _ in 0..(60 * 120) {
            fake.frame(chunks(100));
        }
        assert_eq!(fake.pages.len(), 2, "back to the floor");
    }

    #[test]
    fn a_sawtooth_around_the_thresholds_does_not_oscillate() {
        let mut fake = Fake::new(2);
        let page = u64::from(PAGE);
        // around the growth threshold: free just above and below the reserve
        for i in 0..(60 * 60) {
            let used = 2 * page - page / 2 + if i % 2 == 0 { 1 } else { 0 };
            fake.frame(used);
        }
        let created = fake.created;
        assert!(
            created <= 1,
            "{created} pages for a sawtooth at the reserve"
        );
        // then around the shrink threshold
        for i in 0..(60 * 60) {
            let used = page / 2 + if i % 2 == 0 { 64 } else { 0 };
            fake.frame(used);
        }
        assert!(
            fake.created + fake.released <= 2,
            "{} created, {} released",
            fake.created,
            fake.released
        );
    }

    #[test]
    fn the_ceiling_holds() {
        let mut fake = Fake::new(2);
        fake.pages = vec![FakePage::new(u64::from(PAGE), 101, 1); 2];
        let mut sample = fake.sample();
        sample.ceiling_quads = 2 * u64::from(PAGE);
        sample.max_pages = 2;
        let d = fake.policy.decide(&sample, fake.now, true);
        assert!(!d.want[0], "no page past the ceiling");
        sample.ceiling_quads = 3 * u64::from(PAGE);
        sample.max_pages = 3;
        let d = fake.policy.decide(&sample, fake.now, true);
        assert!(d.want[0]);
    }

    #[test]
    fn a_failed_creation_backs_off_ten_seconds() {
        let mut fake = Fake::new(1);
        fake.pages = vec![FakePage::new(u64::from(PAGE), 101, 1)];
        let sample = fake.sample();
        let t0 = fake.now;
        fake.policy.failed(PageClass::Solid, t0);
        let d = fake
            .policy
            .decide(&sample, t0 + Duration::from_secs(9), true);
        assert!(!d.want[0]);
        let d = fake.policy.decide(&sample, t0 + FAILURE_BACKOFF, true);
        assert!(d.want[0]);
        // the table too
        let mut sample = sample;
        sample.slots = 1024;
        sample.used_slots = 1000;
        fake.policy.table_failed(t0);
        let d = fake
            .policy
            .decide(&sample, t0 + Duration::from_secs(1), true);
        assert_eq!(d.want_table, None);
        let d = fake.policy.decide(&sample, t0 + FAILURE_BACKOFF, true);
        assert_eq!(d.want_table, Some(2048));
    }

    #[test]
    fn a_slow_creation_doubles_the_reserve_once() {
        let mut policy = MemoryPolicy::default();
        let mut fake = Fake::new(1);
        fake.pages = vec![FakePage::new(u64::from(PAGE), 101, 1)];
        let sample = fake.sample();
        let before = policy.reserve(PageClass::Solid, &sample);
        assert!(!policy.created(PageClass::Solid, fake.now, Duration::from_millis(1)));
        assert!(policy.created(PageClass::Solid, fake.now, Duration::from_millis(3)));
        assert!(!policy.created(PageClass::Solid, fake.now, Duration::from_millis(30)));
        assert_eq!(policy.reserve(PageClass::Solid, &sample), 2 * before);
        assert_eq!(
            policy.reserve(PageClass::Translucent, &sample),
            0,
            "a class without chunks keeps no reserve"
        );
    }

    #[test]
    fn a_victim_goes_back_when_pressure_returns_or_its_evacuation_stalls() {
        let mut fake = Fake::new(1);
        let page = u64::from(PAGE);
        fake.pages = vec![
            FakePage::new(page / 8, 13, 1),
            FakePage::new(page / 8, 13, 2),
        ];
        // pressure: the class's free space without the victim's chunks under its reserve
        let mut sample = fake.sample();
        sample.classes[0].free = page / 4;
        fake.policy.progressed(PageClass::Solid, fake.now);
        let d = fake.policy.decide(&sample, fake.now, true);
        assert!(d.cancel[0], "pressure cancels");
        // or its chunk places: the victim's chunks would leave the others under their headroom
        sample.classes[0].free = 4 * page;
        sample.classes[0].places = 100 + 12;
        fake.policy.progressed(PageClass::Solid, fake.now);
        let d = fake.policy.decide(&sample, fake.now, true);
        assert!(d.cancel[0], "pressure on places cancels");
        // stall: no pressure, but nothing moved for 2 s
        sample.classes[0].places = 387;
        fake.policy.progressed(PageClass::Solid, fake.now);
        let d = fake
            .policy
            .decide(&sample, fake.now + Duration::from_secs(1), true);
        assert!(!d.cancel[0]);
        let d = fake
            .policy
            .decide(&sample, fake.now + EVACUATION_STALL, true);
        assert!(d.cancel[0], "a stall cancels");
    }

    #[test]
    fn a_lower_ceiling_drains_pages_once_their_chunks_fit_elsewhere() {
        let mut fake = Fake::new(1);
        let page = u64::from(PAGE);
        // three pages, a page's worth used: under the ceiling the hysteresis keeps them
        fake.pages = vec![
            FakePage::new(page / 2, 51, 1),
            FakePage::new(page / 2, 51, 1),
            FakePage::new(0, 0, 1),
        ];
        let mut sample = fake.sample();
        fake.policy
            .created(PageClass::Solid, fake.now, Duration::ZERO);
        let later = fake.now + SHRINK_HOLD;
        assert_eq!(fake.policy.decide(&sample, fake.now, true).drain[0], None);
        assert_eq!(
            fake.policy.decide(&sample, later, true).drain[0],
            None,
            "too recent"
        );
        // a ceiling of two pages: the empty page goes after the hold, recent or not
        sample.ceiling_quads = 2 * page;
        sample.max_pages = 2;
        assert_eq!(
            fake.policy.decide(&sample, fake.now, true).drain[0],
            None,
            "held"
        );
        assert_eq!(fake.policy.decide(&sample, later, true).drain[0], Some(2));
        // full pages that cannot fit their chunks elsewhere stay
        fake.pages = vec![FakePage::new(page, 101, 1); 3];
        let mut sample = fake.sample();
        sample.ceiling_quads = 2 * page;
        let mut policy = MemoryPolicy::default();
        let t = fake.now;
        assert_eq!(policy.decide(&sample, t, true).drain[0], None);
        assert_eq!(policy.decide(&sample, t + SHRINK_HOLD, true).drain[0], None);
        // nor do pages whose chunks find no chunk place elsewhere
        fake.pages = vec![
            FakePage::new(page / 8, PAGE_CHUNKS, 1),
            FakePage::new(page / 8, PAGE_CHUNKS, 1),
            FakePage::new(page / 8, PAGE_CHUNKS - 10, 1),
        ];
        let mut sample = fake.sample();
        sample.ceiling_quads = 2 * page;
        let mut policy = MemoryPolicy::default();
        assert_eq!(policy.decide(&sample, t, true).drain[0], None);
        assert_eq!(policy.decide(&sample, t + SHRINK_HOLD, true).drain[0], None);
    }

    /// The floor holds over both classes: each class may lose a page and stay above it alone,
    /// but not both at once, nor one while the other's page drains.
    #[test]
    fn two_classes_never_drain_below_the_floor_together() {
        let page = u64::from(PAGE);
        let empty = |id| PageSample {
            id,
            quads: PAGE,
            used: 0,
            chunks: 0,
        };
        // a floor of four pages; Solid has pages 0, 1 and 3, Translucent 2 and 4, with pages 3
        // and 4 empty: each class alone could lose its empty page and stay on the floor
        let solid = ClassSample {
            chunks: 10,
            used: page / 16,
            pages: 3,
            free: 3 * page - page / 16,
            places: u64::from(3 * PAGE_CHUNKS - 10),
            emptiest: Some(empty(3)),
            ..ClassSample::default()
        };
        let translucent = ClassSample {
            chunks: 10,
            used: page / 16,
            pages: 2,
            free: 2 * page - page / 16,
            places: u64::from(2 * PAGE_CHUNKS - 10),
            emptiest: Some(empty(4)),
            ..ClassSample::default()
        };
        let sample = Sample {
            classes: [solid, translucent],
            page_quads: PAGE,
            page_chunks: PAGE_CHUNKS,
            pages: 5,
            capacity: 5 * page,
            ceiling_quads: 64 * page,
            max_pages: 64,
            floor_quads: 4 * page,
            floor_pages: 4,
            slots: 1 << 20,
            used_slots: 20,
            max_slots: 1 << 20,
            table_pending: false,
        };
        let mut policy = MemoryPolicy::default();
        let t = Instant::now();
        assert_eq!(policy.decide(&sample, t, true).drain, [None, None]);
        let d = policy.decide(&sample, t + SHRINK_HOLD, true);
        assert_eq!(d.drain, [Some(3), None], "one of the two, never both");
        // Solid's page drains: Translucent keeps its page
        let mut draining = sample;
        draining.classes[0].draining = draining.classes[0].emptiest.take();
        draining.classes[0].free -= page;
        draining.classes[0].places -= u64::from(PAGE_CHUNKS);
        let mut policy = MemoryPolicy::default();
        assert_eq!(policy.decide(&draining, t, true).drain, [None, None]);
        let d = policy.decide(&draining, t + SHRINK_HOLD, true);
        assert_eq!(d.drain, [None, None], "the other class's victim counts");
        assert!(!d.cancel[0]);
    }

    #[test]
    fn the_table_is_asked_for_ahead() {
        let mut fake = Fake::new(1);
        fake.pages = vec![FakePage::new(u64::from(PAGE), 101, 1)];
        let mut policy = MemoryPolicy::default();
        let mut sample = fake.sample();
        sample.slots = 32768;
        sample.used_slots = 32768 - 4096;
        assert_eq!(policy.decide(&sample, fake.now, true).want_table, None);
        sample.used_slots += 1;
        assert_eq!(
            policy.decide(&sample, fake.now, true).want_table,
            Some(65536)
        );
        sample.table_pending = true;
        assert_eq!(policy.decide(&sample, fake.now, true).want_table, None);
        sample.table_pending = false;
        sample.max_slots = 40000;
        assert_eq!(
            policy.decide(&sample, fake.now, true).want_table,
            Some(40000),
            "at most the ceiling"
        );
        sample.slots = 40000;
        sample.used_slots = 39999;
        assert_eq!(policy.decide(&sample, fake.now, true).want_table, None);
        // an empty table never grows; growth off decides nothing
        sample.slots = 1024;
        sample.max_slots = 1 << 20;
        sample.used_slots = 0;
        assert_eq!(policy.decide(&sample, fake.now, true).want_table, None);
        sample.used_slots = 1000;
        assert_eq!(policy.decide(&sample, fake.now, false), Decision::default());
    }

    #[test]
    fn the_same_samples_and_clock_decide_the_same() {
        let run = || {
            let mut fake = Fake::new(2);
            let now = fake.now;
            let mut log = Vec::new();
            for i in 0..2000u64 {
                let used = (i % 700) * u64::from(PAGE) / 64;
                log.push(fake.frame(used));
            }
            (log, fake.now - now)
        };
        assert_eq!(run(), run());
    }
}
