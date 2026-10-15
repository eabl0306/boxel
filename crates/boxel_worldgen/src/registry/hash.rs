//! The parameter hash of registry entries ([`HashParams`], [`ParamsHasher`]) and FNV-1a
//! ([`fnv1a64`]), which the salts use too.
//!
//! Not the standard `Hash`: its output may change with the Rust version (and `usize` has the
//! platform's width), and a digest saved with a world must read the same after a toolchain
//! update. Here the byte encoding is fixed and the hash is defined locally.

use crate::noise::splitmix64;
use std::sync::Arc;

/// FNV-1a's offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a's 64-bit prime.
const FNV_PRIME: u64 = 0x0100_0000_01b3;

/// FNV-1a 64 of `bytes`.
pub const fn fnv1a64(bytes: &[u8]) -> u64 {
    fnv1a64_from(FNV_OFFSET, bytes)
}

/// FNV-1a 64 of `bytes`, continued from the state `h`.
const fn fnv1a64_from(mut h: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    h
}

/// The parameters of a registry entry, written to a [`ParamsHasher`] in a fixed byte encoding,
/// the same in every run and on every platform:
///
/// - integers little-endian at their width, `usize` and `isize` as 64 bits;
/// - `bool` as one byte, `char` as its `u32`, floats as their bits (`to_bits`, so `-0.0` and
///   `0.0` differ);
/// - strings and ids: the byte length as `u64`, then the bytes;
/// - slices, arrays and `Vec`: the length as `u64`, then each element;
/// - `Option`: the byte 0, or the byte 1 and the value; tuples: each field in order; `()`:
///   nothing.
///
/// The lengths keep a concatenation unambiguous (`("ab", "c")` is not `("a", "bc")`). A struct
/// writes its fields in a fixed order; a new field is a new hash, and a world saved before
/// shows it as a changed entry.
pub trait HashParams {
    /// Writes the parameters to `h`.
    fn hash_params(&self, h: &mut ParamsHasher);
}

/// The hash of `params` alone ([`ParamsHasher::finish`] after one write).
pub fn hash_params<P: HashParams + ?Sized>(params: &P) -> u64 {
    let mut h = ParamsHasher::new();
    h.write(params);
    h.finish()
}

/// Hashes parameters ([`HashParams`]): FNV-1a 64 over their bytes, finished with
/// [`splitmix64`] so every bit of the result depends on every byte.
#[derive(Clone, Debug)]
pub struct ParamsHasher {
    state: u64,
}

impl ParamsHasher {
    /// A hasher with nothing written.
    pub const fn new() -> Self {
        Self { state: FNV_OFFSET }
    }

    /// Writes `bytes` as they are, without a length: the caller's own encoding.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.state = fnv1a64_from(self.state, bytes);
        self
    }

    /// Writes `params` ([`HashParams`]).
    pub fn write<P: HashParams + ?Sized>(&mut self, params: &P) -> &mut Self {
        params.hash_params(self);
        self
    }

    /// The hash of everything written so far.
    pub fn finish(&self) -> u64 {
        splitmix64(self.state)
    }
}

impl Default for ParamsHasher {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! le_bytes {
    ($($t:ty),*) => {$(
        impl HashParams for $t {
            fn hash_params(&self, h: &mut ParamsHasher) {
                h.write_bytes(&self.to_le_bytes());
            }
        }
    )*};
}
le_bytes!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128);

impl HashParams for usize {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&(*self as u64));
    }
}

impl HashParams for isize {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&(*self as i64));
    }
}

impl HashParams for bool {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&u8::from(*self));
    }
}

impl HashParams for char {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&u32::from(*self));
    }
}

impl HashParams for f32 {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.to_bits());
    }
}

impl HashParams for f64 {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.to_bits());
    }
}

impl HashParams for str {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.len()).write_bytes(self.as_bytes());
    }
}

impl HashParams for String {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(self.as_str());
    }
}

impl<T: HashParams> HashParams for [T] {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.len());
        for item in self {
            h.write(item);
        }
    }
}

impl<T: HashParams, const N: usize> HashParams for [T; N] {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(self.as_slice());
    }
}

impl<T: HashParams> HashParams for Vec<T> {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(self.as_slice());
    }
}

impl<T: HashParams> HashParams for Option<T> {
    fn hash_params(&self, h: &mut ParamsHasher) {
        match self {
            None => h.write(&0u8),
            Some(value) => h.write(&1u8).write(value),
        };
    }
}

impl<T: HashParams + ?Sized> HashParams for &T {
    fn hash_params(&self, h: &mut ParamsHasher) {
        (**self).hash_params(h);
    }
}

impl<T: HashParams + ?Sized> HashParams for Box<T> {
    fn hash_params(&self, h: &mut ParamsHasher) {
        (**self).hash_params(h);
    }
}

impl<T: HashParams + ?Sized> HashParams for Arc<T> {
    fn hash_params(&self, h: &mut ParamsHasher) {
        (**self).hash_params(h);
    }
}

impl HashParams for () {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

macro_rules! tuple {
    ($($t:ident $i:tt),+) => {
        impl<$($t: HashParams),+> HashParams for ($($t,)+) {
            fn hash_params(&self, h: &mut ParamsHasher) {
                $(h.write(&self.$i);)+
            }
        }
    };
}
tuple!(A 0);
tuple!(A 0, B 1);
tuple!(A 0, B 1, C 2);
tuple!(A 0, B 1, C 2, D 3);
