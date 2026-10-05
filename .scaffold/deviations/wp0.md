# wp0 deviations and measurements

## Measurements
- Full verification (check, test, clippy, doc, fmt, ecs_surface, deny, no_bevy, vendor_lock_sync, typos): all green on 2026-10-05.
- vendor: 961/961 lib tests (with RUST_BACKTRACE=1; `filtered_backtrace_test` needs it, same as upstream), 480/480 doctests.
- Seed build (`scaffold.py seed`, cold): 140 s wall. Seed target size: 3.7 GB.
- refork.sh offline: 1.2 s.

## Deviations
- typos.toml: added `lod = "lod"` to `[default.extend-words]` (identifier for level of detail) and fixed British spellings in skeleton comments (behaviour, neighbour(s), honours) with `typos -w`, because `locale = "en-us"`. No code identifiers changed.
