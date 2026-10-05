//! Deterministic value noise (no dependencies). Same seed + coordinates = same value on every
//! platform (integer hashing + f64 math).

/// 32-bit hash of `(seed, x, z)` (e.g. a SplitMix64/xxhash-style mix).
pub fn hash2(seed: u64, x: i32, z: i32) -> u32 {
    todo!("WP4")
}

/// 2D value noise in `[-1, 1]`: smoothstep-interpolated lattice values.
pub fn value_noise_2d(seed: u64, x: f64, z: f64) -> f64 {
    todo!("WP4")
}

/// Fractal Brownian motion of [`value_noise_2d`] in roughly `[-1, 1]`.
pub fn fbm_2d(seed: u64, x: f64, z: f64, octaves: u32, lacunarity: f64, gain: f64) -> f64 {
    todo!("WP4")
}
