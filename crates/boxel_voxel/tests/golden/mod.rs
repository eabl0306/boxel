//! Shared by the content goldens of #48 (`content_golden.rs` here, and the mesh, light, physics,
//! world, render and demo goldens, which include this file by path): each golden builds a
//! canonical text dump of what the engine makes of the content, hashes it with FNV-1a 64 and
//! compares the hash with a literal computed on the commit before the content moved. On a
//! mismatch the dump is written to `<target>/golden/<name>.txt` to diff against the one of the
//! old commit.
#![allow(dead_code, reason = "each golden uses a subset")]

use std::path::PathBuf;

/// FNV-1a 64 of `bytes`: a local hash, so the literals never depend on a library.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Where a dump that does not match is written: `<target>/golden/<name>.txt`.
pub fn dump_path(name: &str) -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    );
    target.join("golden").join(format!("{name}.txt"))
}

/// Asserts that `dump` hashes to `expected`; writes the dump first when it does not.
#[track_caller]
pub fn check(name: &str, dump: &str, expected: u64) {
    let got = fnv1a64(dump.as_bytes());
    if got != expected {
        let path = dump_path(name);
        let _ = std::fs::create_dir_all(path.parent().expect("golden dir"));
        let _ = std::fs::write(&path, dump);
        panic!(
            "golden {name}: hash {got:#018x}, expected {expected:#018x} ({} bytes, dump in {})",
            dump.len(),
            path.display()
        );
    }
}

/// splitmix64: the seeded stream of the goldens that draw states at random.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// A value in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}
