//! Quad arena: one `STORAGE | VERTEX | COPY_DST` buffer of `PackedQuads`, sub-allocated in quad
//! units with `offset_allocator` (one allocation per chunk, all 7 buckets contiguous).
