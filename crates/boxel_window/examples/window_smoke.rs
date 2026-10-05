//! Window and runner smoke test (owner: WP2). Agent-verifiable: no human input needed.
//!
//! `cargo run -p boxel_window --example window_smoke -- [--frames N] [--resize-test]`
//!
//! Contract (`SCAFFOLD_SPEC` section 9.5): `WindowPlugin` + `InputPlugin` + `TimePlugin`; prints
//! `boxel-window: created WxH scale=S` on `WindowCreated`; writes `AppExit::Success` after N
//! updates (default 120) and prints `boxel-window: updates=N redraws=M occluded_frames=K`;
//! `--resize-test` requests a +100x+50 inner size at update 30 and exits 3 if no
//! `WindowResized` arrives within 60 updates. Exit 0 on success.
// SKELETON: remove this attribute when the example is implemented (owner: WP2).
#![allow(clippy::todo, reason = "skeleton stub")]
fn main() {
    todo!("WP2: window_smoke")
}
