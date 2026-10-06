//! Busy time per span name, cut into frames: a `tracing` layer for frame breakdowns (the stress
//! example's `--spans`). It only costs anything when installed ([`init_logging_with_layer`]).
//!
//! Each span is keyed by its name, or by its `name` field when it has one: an ECS system span
//! (`system`, feature `trace` of the facade) is keyed by the system name, a `schedule` span by
//! `schedule <label>`, any other span with a `name` field by `<span> <name>`. The time between
//! entering and exiting a span is added to its key, whatever the thread. Each thread inside a span
//! counts on its own, also when several are inside the same span at once (the batches of a
//! parallel query): systems and spans that run in parallel add up to more than the wall time. A
//! span entered again inside itself on the same thread counts its time once (and both calls).
//! Entering the span whose key is the frame root
//! (`schedule Main`: one per `App::update`) closes the frame: the sums so far become the last
//! frame ([`SpanTimes::last_frame`]) and the next frame starts from zero.
//!
//! Code that times many tiny steps itself (a span each would cost more than the step) reports
//! the total with a span that has a `busy_ns` field (and optionally `calls`), created and never
//! entered: `info_span!("world.step", name = "create", busy_ns = ns, calls = n)`.
//!
//! [`init_logging_with_layer`]: crate::init_logging_with_layer

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Most distinct keys tracked. Spans with keys past the limit share the last one,
/// [`OVERFLOW_KEY`].
pub const MAX_SPAN_KEYS: usize = 1024;
/// Key of the spans past [`MAX_SPAN_KEYS`].
pub const OVERFLOW_KEY: &str = "(too many span names)";
/// Frame root of an `App`: the `Main` schedule span, entered once per update.
pub const MAIN_SCHEDULE: &str = "schedule Main";

/// The sums of one frame, indexed like [`SpanTimes::keys`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanFrame {
    /// Frames closed so far (0: none yet).
    pub seq: u64,
    /// Busy nanoseconds per key.
    pub busy_ns: Vec<u64>,
    /// Times each key was entered.
    pub calls: Vec<u32>,
}

/// Creates the layer and the handle that reads it. `frame_root` is the key whose span opens a
/// frame (usually [`MAIN_SCHEDULE`]).
pub fn span_times(frame_root: &str) -> (SpanTimesLayer, SpanTimes) {
    let shared = Arc::new(Shared {
        root: frame_root.to_string(),
        keys: Mutex::default(),
        len: AtomicUsize::new(0),
        busy: (0..MAX_SPAN_KEYS).map(|_| AtomicU64::new(0)).collect(),
        calls: (0..MAX_SPAN_KEYS).map(|_| AtomicU32::new(0)).collect(),
        last: Mutex::default(),
    });
    (
        SpanTimesLayer {
            shared: Arc::clone(&shared),
        },
        SpanTimes { shared },
    )
}

/// The `tracing` layer (see the module docs).
pub struct SpanTimesLayer {
    shared: Arc<Shared>,
}

/// Reads what [`SpanTimesLayer`] measured.
#[derive(Clone)]
pub struct SpanTimes {
    shared: Arc<Shared>,
}

impl SpanTimes {
    /// Every key seen so far, by index.
    pub fn keys(&self) -> Vec<String> {
        lock(&self.shared.keys).list.clone()
    }

    /// Copies the last closed frame into `out` (no allocation once `out` has grown).
    pub fn last_frame(&self, out: &mut SpanFrame) {
        out.clone_from(&lock(&self.shared.last));
    }
}

impl fmt::Debug for SpanTimes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpanTimes")
            .field("root", &self.shared.root)
            .field("keys", &self.shared.len.load(Ordering::Relaxed))
            .finish()
    }
}

struct Shared {
    root: String,
    keys: Mutex<Keys>,
    /// `keys.list.len()`, readable without the lock.
    len: AtomicUsize,
    busy: Box<[AtomicU64]>,
    calls: Box<[AtomicU32]>,
    last: Mutex<SpanFrame>,
}

#[derive(Default)]
struct Keys {
    index: HashMap<String, usize>,
    list: Vec<String>,
}

impl Shared {
    fn intern(&self, key: String) -> usize {
        let mut keys = lock(&self.keys);
        if let Some(&i) = keys.index.get(&key) {
            return i;
        }
        let i = keys.list.len().min(MAX_SPAN_KEYS - 1);
        if i == keys.list.len() {
            let name = if i == MAX_SPAN_KEYS - 1 {
                OVERFLOW_KEY.to_string()
            } else {
                key.clone()
            };
            keys.list.push(name);
            self.len.store(keys.list.len(), Ordering::Release);
        }
        keys.index.insert(key, i);
        i
    }

    /// Moves the running sums into `last` and starts the next frame from zero.
    fn close_frame(&self) {
        let len = self.len.load(Ordering::Acquire);
        let mut last = lock(&self.last);
        last.seq += 1;
        last.busy_ns.clear();
        last.busy_ns.extend(
            self.busy[..len]
                .iter()
                .map(|b| b.swap(0, Ordering::Relaxed)),
        );
        last.calls.clear();
        last.calls.extend(
            self.calls[..len]
                .iter()
                .map(|c| c.swap(0, Ordering::Relaxed)),
        );
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Per-span state, in the registry's extensions.
struct Slot {
    key: usize,
    root: bool,
    /// The first thread inside the span (usually the only one).
    first: Option<Inside>,
    /// Other threads inside the span at the same time.
    more: Vec<Inside>,
}

/// One thread inside a span.
#[derive(Clone, Copy)]
struct Inside {
    thread: u64,
    /// Entries not exited yet (more than 1: the span was entered again inside itself).
    depth: u32,
    /// When the outermost entry began.
    since: Instant,
}

impl Slot {
    fn new(key: usize, root: bool) -> Self {
        Self {
            key,
            root,
            first: None,
            more: Vec::new(),
        }
    }

    /// Entry of `thread`.
    fn enter(&mut self, thread: u64, now: Instant) {
        let inside = match &mut self.first {
            Some(first) if first.thread == thread => Some(first),
            Some(_) => self.more.iter_mut().find(|i| i.thread == thread),
            None => {
                self.first = Some(Inside {
                    thread,
                    depth: 1,
                    since: now,
                });
                return;
            }
        };
        match inside {
            Some(inside) => inside.depth += 1,
            None => self.more.push(Inside {
                thread,
                depth: 1,
                since: now,
            }),
        }
    }

    /// Exit of `thread`.
    fn exit(&mut self, thread: u64) -> Exit {
        let inside = match &mut self.first {
            Some(first) if first.thread == thread => first,
            _ => match self.more.iter_mut().find(|i| i.thread == thread) {
                Some(inside) => inside,
                None => return Exit::NotInside,
            },
        };
        inside.depth -= 1;
        if inside.depth > 0 {
            return Exit::Nested;
        }
        let since = inside.since;
        if self.first.is_some_and(|first| first.thread == thread) {
            self.first = self.more.pop();
        } else {
            self.more.retain(|i| i.thread != thread);
        }
        Exit::Left(since)
    }
}

/// What [`Slot::exit`] did.
enum Exit {
    /// The thread was not inside (it entered before the layer saw the span).
    NotInside,
    /// Still inside an outer entry of the same span.
    Nested,
    /// Left its outermost entry, which began at this instant.
    Left(Instant),
}

/// A small number for the calling thread, unique in the process (cheaper than `ThreadId`).
fn thread_number() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    thread_local! {
        static NUMBER: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    NUMBER.with(|n| *n)
}

/// Reads the `name`, `busy_ns` and `calls` fields.
#[derive(Default)]
struct Fields {
    name: Option<String>,
    busy_ns: Option<u64>,
    calls: Option<u64>,
}

impl Visit for Fields {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "busy_ns" => self.busy_ns = Some(value),
            "calls" => self.calls = Some(value),
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_u64(field, u64::try_from(value).unwrap_or(0));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "name" {
            self.name = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "name" {
            self.name = Some(format!("{value:?}"));
        }
    }
}

/// The key of a span named `span` with an optional `name` field.
fn key_of(span: &str, name: Option<String>) -> String {
    match name {
        Some(name) if span == "system" => name,
        Some(name) => format!("{span} {name}"),
        None => span.to_string(),
    }
}

impl<S> Layer<S> for SpanTimesLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let key = key_of(attrs.metadata().name(), fields.name);
        let root = key == self.shared.root;
        let key = self.shared.intern(key);
        if let Some(ns) = fields.busy_ns {
            // Timed by the caller: added now, never entered.
            self.shared.busy[key].fetch_add(ns, Ordering::Relaxed);
            let calls = u32::try_from(fields.calls.unwrap_or(1)).unwrap_or(u32::MAX);
            self.shared.calls[key].fetch_add(calls, Ordering::Relaxed);
            return;
        }
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Slot::new(key, root));
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(slot) = extensions.get_mut::<Slot>() else {
            return;
        };
        if slot.root {
            self.shared.close_frame();
        }
        slot.enter(thread_number(), Instant::now());
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let now = Instant::now();
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(slot) = extensions.get_mut::<Slot>() else {
            return;
        };
        match slot.exit(thread_number()) {
            Exit::NotInside => {}
            Exit::Nested => {
                self.shared.calls[slot.key].fetch_add(1, Ordering::Relaxed);
            }
            Exit::Left(since) => {
                let ns = u64::try_from(now.duration_since(since).as_nanos()).unwrap_or(u64::MAX);
                self.shared.busy[slot.key].fetch_add(ns, Ordering::Relaxed);
                self.shared.calls[slot.key].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    /// Schedule labels print like this (`derive(Debug)` on a unit struct).
    #[derive(Debug)]
    struct Main;
    #[derive(Debug)]
    struct Update;

    /// Busy for 2 ms (sleeping is not allowed in this workspace).
    fn spin_2ms() {
        let start = Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(2) {
            std::hint::spin_loop();
        }
    }

    fn busy(times: &SpanTimes, frame: &SpanFrame, key: &str) -> (u64, u32) {
        let i = times
            .keys()
            .iter()
            .position(|k| k == key)
            .unwrap_or_else(|| panic!("no key {key:?} in {:?}", times.keys()));
        (frame.busy_ns[i], frame.calls[i])
    }

    #[test]
    fn spans_add_up_per_key_and_the_root_closes_frames() {
        let (layer, times) = span_times("schedule Main");
        let subscriber = tracing_subscriber::registry().with(layer);
        let mut frame = SpanFrame::default();
        tracing::subscriber::with_default(subscriber, || {
            // Frame 1: a system run twice (one cached span), a plain span, a schedule.
            let main = tracing::info_span!("schedule", name = ?Main);
            main.in_scope(|| {
                let system = tracing::info_span!("system", name = "app::update".to_string());
                for _ in 0..2 {
                    system.in_scope(spin_2ms);
                }
                let _plain = tracing::info_span!("world.positions").entered();
                // Timed by the caller.
                tracing::info_span!(
                    "world.step",
                    name = "create",
                    busy_ns = 7_u64,
                    calls = 3_u64
                );
                let _update = tracing::info_span!("schedule", name = ?Update).entered();
            });
            times.last_frame(&mut frame);
            assert_eq!(frame.seq, 1, "entering the root closed the empty frame 0");
            assert!(frame.busy_ns.iter().all(|&ns| ns == 0));

            // Entering the root again closes frame 1.
            let main = tracing::info_span!("schedule", name = ?Main);
            let _main = main.enter();
            times.last_frame(&mut frame);
        });
        assert_eq!(frame.seq, 2);
        let (system_ns, calls) = busy(&times, &frame, "app::update");
        assert!(system_ns >= 4_000_000, "{system_ns}");
        assert_eq!(calls, 2);
        assert_eq!(busy(&times, &frame, "world.positions").1, 1);
        assert_eq!(busy(&times, &frame, "world.step create"), (7, 3));
        assert_eq!(busy(&times, &frame, "schedule Update").1, 1);
        let (main_ns, main_calls) = busy(&times, &frame, "schedule Main");
        assert!(main_ns >= system_ns, "the root holds the system");
        assert_eq!(main_calls, 1);
    }

    #[test]
    fn threads_inside_one_span_count_on_their_own() {
        let (layer, times) = span_times("schedule Main");
        let subscriber = tracing_subscriber::registry().with(layer);
        let mut frame = SpanFrame::default();
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("par.batch");
            let _main = tracing::info_span!("schedule", name = ?Main).entered();
            // Two threads inside the same span at once, like the batches of a parallel query:
            // both stay inside until both have entered.
            let both_in = std::sync::Barrier::new(2);
            let dispatch = tracing::dispatcher::get_default(Clone::clone);
            std::thread::scope(|scope| {
                for _ in 0..2 {
                    scope.spawn(|| {
                        tracing::dispatcher::with_default(&dispatch, || {
                            let _in = span.enter();
                            both_in.wait();
                            spin_2ms();
                        });
                    });
                }
            });
            // Entered again inside itself: both calls, the time once.
            let nested = tracing::info_span!("nested");
            nested.in_scope(|| nested.in_scope(spin_2ms));
            drop(_main);
            let _next = tracing::info_span!("schedule", name = ?Main).entered();
            times.last_frame(&mut frame);
        });
        let (par_ns, par_calls) = busy(&times, &frame, "par.batch");
        assert_eq!(par_calls, 2);
        assert!(par_ns >= 4_000_000, "both threads counted: {par_ns}");
        let (nested_ns, nested_calls) = busy(&times, &frame, "nested");
        assert_eq!(nested_calls, 2);
        let (main_ns, _) = busy(&times, &frame, "schedule Main");
        assert!(
            (2_000_000..main_ns).contains(&nested_ns),
            "nested time counted once: {nested_ns} (frame {main_ns})"
        );
    }

    #[test]
    fn keys() {
        assert_eq!(key_of("system", Some("a::b".into())), "a::b");
        assert_eq!(key_of("schedule", Some("Main".into())), "schedule Main");
        assert_eq!(key_of("render.upload", None), "render.upload");
        let (_, times) = span_times(MAIN_SCHEDULE);
        let shared = &times.shared;
        for i in 0..MAX_SPAN_KEYS + 5 {
            shared.intern(format!("k{i}"));
        }
        let keys = times.keys();
        assert_eq!(keys.len(), MAX_SPAN_KEYS);
        assert_eq!(keys[MAX_SPAN_KEYS - 1], OVERFLOW_KEY);
        assert_eq!(shared.intern("k3".into()), 3);
        assert_eq!(shared.intern("k5000".into()), MAX_SPAN_KEYS - 1);
    }
}
