//! Tier paths (internal). Each implements the same internal trait (owner: WP7):
//! `prepare` (CPU culling / uploads), `encode_compute` (High cull), `draw_opaque`.
mod high;
mod low;
mod mid;
