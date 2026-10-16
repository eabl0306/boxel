//! The bytes of a saved world (ADR 0024): keys, chunk records, entity records, the id map and
//! the metadata blobs. Everything here is pure (no IO, no ECS) and never panics on bad input: a damaged
//! record is an error, so a save that went wrong regenerates one chunk instead of taking the
//! game down.
//!
//! Stability rules:
//! - Chunk keys keep all 64 bits of every axis ([`encode_key`]), in an order that sorts a chunk
//!   column's layers together.
//! - Blocks are stored by the stable name of their state ([`IdMap`], ADR 0019), never by the
//!   runtime id, which changes when the registry changes.
//! - Every blob is an envelope with a magic, a version, a length and a checksum. A new layout
//!   bumps the blob's version; [`FORMAT_VERSION`] changes only when the old files cannot be read
//!   any more.
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos, DimensionId, Paletted,
};
use boxel_worldgen::preset::{DimensionRecord, SpecLine, WorldgenRecord};
use boxel_worldgen::realm::RealmSpan;
use boxel_worldgen::registry::{DigestEntry, DigestScope, GenId, RegistryDigest};
use std::borrow::Cow;
use std::fmt;

/// Version of the world layout (tables, keys and the meaning of the metadata). A world with
/// another version is refused when it opens.
pub const FORMAT_VERSION: u32 = 1;

/// Bytes of an encoded chunk key: dimension (2), lod (1) and x, z, y (8 each).
pub const KEY_LEN: usize = 27;

/// Magic of a chunk record.
pub(crate) const CHUNK_MAGIC: [u8; 4] = *b"BXCK";
/// Magic of a metadata blob (the id map, the pending ticks, the clock).
pub(crate) const META_MAGIC: [u8; 4] = *b"BXMT";
/// Magic of an entity record.
pub(crate) const ENTITY_MAGIC: [u8; 4] = *b"BXEN";
/// Magic of the world generation record (`boxel:worldgen`).
pub(crate) const WORLDGEN_MAGIC: [u8; 4] = *b"BXWG";
/// Version of the chunk record layout.
pub(crate) const CHUNK_VERSION: u8 = 1;
/// Version of the metadata blob layouts.
pub(crate) const META_VERSION: u8 = 1;
/// Version of the entity record layout (the codecs' own bytes have their own versions).
pub(crate) const ENTITY_VERSION: u8 = 1;
/// Version of the world generation record layout ([`encode_worldgen`]): its own, so the record
/// can grow without a new [`FORMAT_VERSION`] (ADR 0039 sec. 17).
pub const WORLDGEN_VERSION: u8 = 1;

/// Bytes of an encoded entity key: its chunk's key ([`KEY_LEN`]) and its saved id (8).
pub const ENTITY_KEY_LEN: usize = KEY_LEN + 8;

/// Envelope header: magic (4), version (1), flags (1), payload length (4), checksum (8).
const HEADER_LEN: usize = 18;
/// Flag: the payload is LZ4 compressed (block format).
const FLAG_LZ4: u8 = 1;
/// Payloads up to this size are stored as they are (LZ4 would not pay).
const COMPRESS_FROM: usize = 64;
/// Largest chunk record payload: a direct chunk, its kind and a varint of at most 5 bytes per
/// voxel.
const MAX_CHUNK_PAYLOAD: usize = 1 + 5 * CHUNK_VOLUME;
/// LZ4 gives at most this many bytes per compressed byte (a byte of a match length stands for
/// at most 255 bytes out): a longer length in a damaged header is refused before allocating.
const LZ4_MAX_RATIO: usize = 255;

/// Largest payload of a blob with `magic`: a chunk record's size is bounded by its layout; a
/// metadata blob (a long pending tick list) or an entity record by the header's `u32` length
/// alone. The reader takes whatever the writer can seal.
fn max_payload(magic: [u8; 4]) -> usize {
    if magic == CHUNK_MAGIC {
        MAX_CHUNK_PAYLOAD
    } else {
        u32::MAX as usize
    }
}

/// Payload kinds of a chunk record.
const KIND_UNIFORM: u8 = 0;
const KIND_PACKED: u8 = 1;
const KIND_DIRECT: u8 = 2;

/// Why bytes could not be read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// Too short, a wrong magic, a bad length or a checksum that does not match.
    Damaged(&'static str),
    /// A blob version this build cannot read.
    Version {
        /// The blob's version.
        found: u8,
        /// The version this build writes.
        supported: u8,
    },
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::Damaged(what) => write!(f, "damaged record: {what}"),
            FormatError::Version { found, supported } => {
                write!(f, "record version {found} (this build reads {supported})")
            }
        }
    }
}

impl std::error::Error for FormatError {}

// ---- Keys ----------------------------------------------------------------------------------

/// An `i64` as 8 big-endian bytes whose byte order is the numeric order (sign bit flipped).
fn ordered(v: i64) -> [u8; 8] {
    ((v as u64) ^ (1 << 63)).to_be_bytes()
}

/// The inverse of [`ordered`].
fn unordered(b: [u8; 8]) -> i64 {
    (u64::from_be_bytes(b) ^ (1 << 63)) as i64
}

/// The bytes of a chunk key: dimension, lod, then x, z and y. Byte order is the order of
/// `(dim, lod, x, z, y)`, so the layers of one chunk column are next to each other. Exact for
/// every `i64` (ADR 0014).
pub fn encode_key(key: ChunkKey) -> [u8; KEY_LEN] {
    let mut out = [0; KEY_LEN];
    out[..2].copy_from_slice(&key.dim.0.to_be_bytes());
    out[2] = key.lod;
    out[3..11].copy_from_slice(&ordered(key.pos.x));
    out[11..19].copy_from_slice(&ordered(key.pos.z));
    out[19..27].copy_from_slice(&ordered(key.pos.y));
    out
}

/// The inverse of [`encode_key`].
pub fn decode_key(bytes: &[u8; KEY_LEN]) -> ChunkKey {
    let axis = |i: usize| {
        let mut b = [0; 8];
        b.copy_from_slice(&bytes[i..i + 8]);
        unordered(b)
    };
    ChunkKey {
        dim: DimensionId(u16::from_be_bytes([bytes[0], bytes[1]])),
        lod: bytes[2],
        pos: ChunkPos::new(axis(3), axis(19), axis(11)),
    }
}

// ---- Envelope ------------------------------------------------------------------------------

/// FNV-1a, 64 bits: a stable checksum (the same in every build and on every platform).
fn checksum(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Wraps `payload` in an envelope: magic, version, flags, length, checksum and the payload,
/// LZ4 compressed when that makes it smaller.
pub(crate) fn seal(magic: [u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
    let compressed = (payload.len() > COMPRESS_FROM)
        .then(|| lz4_flex::block::compress(payload))
        .filter(|c| c.len() < payload.len());
    let (flags, body) = match &compressed {
        Some(c) => (FLAG_LZ4, c.as_slice()),
        None => (0, payload),
    };
    debug_assert!(
        payload.len() <= max_payload(magic),
        "the reader would refuse it"
    );
    let len = u32::try_from(payload.len()).expect("payloads are far below 4 GiB");
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&magic);
    out.push(version);
    out.push(flags);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&checksum(payload).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// The payload of an envelope written by [`seal`] with this `magic` and `version`.
pub(crate) fn open(magic: [u8; 4], version: u8, bytes: &[u8]) -> Result<Vec<u8>, FormatError> {
    if bytes.len() < HEADER_LEN {
        return Err(FormatError::Damaged("shorter than its header"));
    }
    if bytes[..4] != magic {
        return Err(FormatError::Damaged("wrong magic"));
    }
    if bytes[4] != version {
        return Err(FormatError::Version {
            found: bytes[4],
            supported: version,
        });
    }
    let flags = bytes[5];
    let len = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as usize;
    let mut sum = [0; 8];
    sum.copy_from_slice(&bytes[10..18]);
    let body = &bytes[HEADER_LEN..];
    let compressed = flags & FLAG_LZ4 != 0;
    if len > max_payload(magic)
        || (compressed && len > body.len().saturating_mul(LZ4_MAX_RATIO))
        || flags & !FLAG_LZ4 != 0
    {
        return Err(FormatError::Damaged("bad header"));
    }
    let payload = if compressed {
        lz4_flex::block::decompress(body, len)
            .map_err(|_| FormatError::Damaged("bad compressed data"))?
    } else {
        body.to_vec()
    };
    if payload.len() != len {
        return Err(FormatError::Damaged("wrong payload length"));
    }
    if checksum(&payload) != u64::from_le_bytes(sum) {
        return Err(FormatError::Damaged("checksum mismatch"));
    }
    Ok(payload)
}

// ---- Primitive writer and reader -----------------------------------------------------------

/// LEB128 of `v`.
pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Reads a payload front to back; every read checks the length.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], FormatError> {
        if self.bytes.len() < n {
            return Err(FormatError::Damaged("payload ends early"));
        }
        let (head, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Ok(head)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u64(&mut self) -> Result<u64, FormatError> {
        let mut b = [0; 8];
        b.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(b))
    }

    pub(crate) fn i64(&mut self) -> Result<i64, FormatError> {
        Ok(self.u64()? as i64)
    }

    pub(crate) fn varint(&mut self) -> Result<u64, FormatError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(FormatError::Damaged("varint too long"))
    }

    /// A varint that must fit `usize` and be at most `max`.
    pub(crate) fn count(&mut self, max: usize) -> Result<usize, FormatError> {
        usize::try_from(self.varint()?)
            .ok()
            .filter(|&n| n <= max)
            .ok_or(FormatError::Damaged("count out of range"))
    }

    /// Fails unless every byte was read.
    pub(crate) fn finish(self) -> Result<(), FormatError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(FormatError::Damaged("trailing bytes"))
        }
    }
}

// ---- Id map --------------------------------------------------------------------------------

/// Stored ids of the block states of one world (ADR 0024): the world keeps the list of state
/// names it has ever used (`game:log[axis=x]`, ADR 0019) and stores a block as the index of its
/// name in that list. The list only grows, so the ids in saved chunks never change meaning. When
/// the world opens, every name is resolved against the registry of this run:
///
/// - a name the registry knows exactly is that state, and the state's stored id;
/// - a known block with a property left out, or an unknown property or value, keeps the
///   properties it still has and takes the default of the rest
///   (`game:fence[north=true,waterlogged=false]` without `waterlogged` becomes the fence with
///   `north=true`). It is listed in [`IdMap::unknown`], and the state it became saves under its
///   own exact name, so a later change of a default never changes a saved block;
/// - an unknown block becomes air.
///
/// Records are never rewritten just because they were read, so a block that comes back in a
/// later registry reappears in every chunk that was not edited meanwhile.
#[derive(Clone, Debug)]
pub struct IdMap {
    /// Every name, by stored id.
    names: Vec<String>,
    /// The runtime state of each stored id.
    to_runtime: Vec<BlockState>,
    /// The stored id of each runtime state.
    to_stored: Vec<u32>,
    /// Stored names this registry does not know exactly (with what they became).
    unknown: Vec<(String, BlockState)>,
}

impl IdMap {
    /// The map of a world that used `names` (stored id order), resolved against `registry`;
    /// every registry state missing from `names` is appended. Returns the map and whether
    /// names were appended (the list must then be saved).
    pub fn resolve(names: Vec<String>, registry: &BlockRegistry) -> (Self, bool) {
        let mut names = names;
        let mut to_runtime = Vec::with_capacity(names.len());
        let mut unknown = Vec::new();
        let mut to_stored = vec![u32::MAX; registry.len()];
        for (id, name) in names.iter().enumerate() {
            // Only a state's exact name is its id. Another name that parses (a block that gained
            // a property: its default fills in) reads as that state but is a fallback, like an
            // unknown one: a fallback never takes the id of the state it became, or the blocks
            // saved under it would change when the default changes.
            let exact = registry
                .parse_state(name)
                .ok()
                .filter(|&s| registry.state_name(s).to_string() == *name);
            match exact {
                Some(state) => {
                    to_runtime.push(state);
                    // The first name of a state is its id.
                    let slot = &mut to_stored[usize::from(state.0)];
                    if *slot == u32::MAX {
                        *slot = u32::try_from(id).expect("fewer than 2^32 names");
                    }
                }
                None => {
                    let state = registry
                        .parse_state(name)
                        .unwrap_or_else(|_| closest_state(registry, name));
                    unknown.push((name.clone(), state));
                    to_runtime.push(state);
                }
            }
        }
        let mut grew = false;
        for (i, stored) in to_stored.iter_mut().enumerate() {
            if *stored != u32::MAX {
                continue;
            }
            let state = BlockState(u16::try_from(i).expect("states fit u16"));
            *stored = u32::try_from(names.len()).expect("fewer than 2^32 names");
            names.push(registry.state_name(state).to_string());
            to_runtime.push(state);
            grew = true;
        }
        (
            Self {
                names,
                to_runtime,
                to_stored,
                unknown,
            },
            grew,
        )
    }

    /// Every stored name, by stored id.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// The stored names this registry does not know exactly, with the state each one became.
    pub fn unknown(&self) -> &[(String, BlockState)] {
        &self.unknown
    }

    /// States of the registry the map was resolved against.
    pub fn registry_states(&self) -> usize {
        self.to_stored.len()
    }

    /// The stored id of runtime state `s`.
    fn stored(&self, s: BlockState) -> Result<u32, FormatError> {
        self.to_stored
            .get(usize::from(s.0))
            .copied()
            .filter(|&id| id != u32::MAX)
            .ok_or(FormatError::Damaged("state outside the registry"))
    }

    /// The runtime state of stored id `id`.
    fn runtime(&self, id: u64) -> Result<BlockState, FormatError> {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.to_runtime.get(i))
            .copied()
            .ok_or(FormatError::Damaged("stored id outside the id map"))
    }
}

/// The state of `registry` closest to the stored name `name` that it does not know exactly:
/// the block's default state with every property value it still has, or air.
fn closest_state(registry: &BlockRegistry, name: &str) -> BlockState {
    let (block, props) = match name.split_once('[') {
        Some((block, rest)) => (block, rest.strip_suffix(']').unwrap_or(rest)),
        None => (name, ""),
    };
    let Some(b) = registry.block_by_name(block) else {
        return BlockState::AIR;
    };
    let mut state = registry.default_state(b);
    for pair in props.split(',').filter(|p| !p.is_empty()) {
        if let Some((property, value)) = pair.split_once('=')
            && let Some(next) = registry.with(state, property, value)
        {
            state = next;
        }
    }
    state
}

/// The id map blob: the names in stored id order.
pub(crate) fn encode_names(names: &[String]) -> Vec<u8> {
    let mut payload = Vec::new();
    put_varint(&mut payload, names.len() as u64);
    for name in names {
        put_varint(&mut payload, name.len() as u64);
        payload.extend_from_slice(name.as_bytes());
    }
    seal(META_MAGIC, META_VERSION, &payload)
}

/// The inverse of [`encode_names`].
pub(crate) fn decode_names(bytes: &[u8]) -> Result<Vec<String>, FormatError> {
    let payload = open(META_MAGIC, META_VERSION, bytes)?;
    let mut r = Reader::new(&payload);
    let n = r.count(payload.len())?;
    let mut names = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.count(payload.len())?;
        let name = std::str::from_utf8(r.take(len)?)
            .map_err(|_| FormatError::Damaged("a name is not UTF-8"))?;
        names.push(name.to_string());
    }
    r.finish()?;
    Ok(names)
}

// ---- Chunk records -------------------------------------------------------------------------

/// The record of a chunk's blocks: its storage, compacted, with stored ids.
pub fn encode_chunk(data: &ChunkData, ids: &IdMap) -> Result<Vec<u8>, FormatError> {
    let mut blocks = data.blocks().clone();
    blocks.compact();
    let mut payload = Vec::new();
    match &blocks {
        Paletted::Uniform(s) => {
            payload.push(KIND_UNIFORM);
            put_varint(&mut payload, u64::from(ids.stored(*s)?));
        }
        Paletted::Packed {
            palette,
            bits,
            words,
        } => {
            payload.push(KIND_PACKED);
            put_varint(&mut payload, palette.len() as u64);
            for &s in palette {
                put_varint(&mut payload, u64::from(ids.stored(s)?));
            }
            payload.push(*bits);
            payload.reserve(words.len() * 8);
            for w in words.iter() {
                payload.extend_from_slice(&w.to_le_bytes());
            }
        }
        Paletted::Direct(states) => {
            payload.push(KIND_DIRECT);
            for &s in states.iter() {
                put_varint(&mut payload, u64::from(ids.stored(s)?));
            }
        }
    }
    Ok(seal(CHUNK_MAGIC, CHUNK_VERSION, &payload))
}

/// The blocks of a chunk record, with runtime states.
pub fn decode_chunk(bytes: &[u8], ids: &IdMap) -> Result<ChunkData, FormatError> {
    let payload = open(CHUNK_MAGIC, CHUNK_VERSION, bytes)?;
    let mut r = Reader::new(&payload);
    let blocks = match r.u8()? {
        KIND_UNIFORM => Paletted::Uniform(ids.runtime(r.varint()?)?),
        KIND_PACKED => {
            let n = r.count(256)?;
            let palette = (0..n)
                .map(|_| ids.runtime(r.varint()?))
                .collect::<Result<Vec<_>, _>>()?;
            let bits = r.u8()?;
            if !matches!(bits, 1 | 2 | 4 | 8) || n == 0 || n > 1 << bits {
                return Err(FormatError::Damaged("bad palette"));
            }
            let b = usize::from(bits);
            let count = CHUNK_VOLUME * b / 64;
            let mut words = Vec::with_capacity(count);
            let mask = (1u64 << b) - 1;
            for _ in 0..count {
                let w = r.u64()?;
                // Every index must name a palette entry (a full palette cannot be exceeded).
                if n < 1 << bits && (0..64 / b).any(|j| ((w >> (j * b)) & mask) as usize >= n) {
                    return Err(FormatError::Damaged("palette index out of range"));
                }
                words.push(w);
            }
            Paletted::Packed {
                palette,
                bits,
                words: words.into_boxed_slice(),
            }
        }
        KIND_DIRECT => {
            let states = (0..CHUNK_VOLUME)
                .map(|_| ids.runtime(r.varint()?))
                .collect::<Result<Vec<_>, _>>()?;
            Paletted::Direct(states.into_boxed_slice())
        }
        _ => return Err(FormatError::Damaged("unknown chunk kind")),
    };
    r.finish()?;
    Ok(ChunkData::from_paletted(blocks))
}

// ---- Entities ------------------------------------------------------------------------------

/// The bytes of an entity key: the key of the chunk whose record holds the entity
/// ([`encode_key`]) and the entity's saved id, big endian. The entities of one chunk are one
/// range of the table, by id.
pub fn encode_entity_key(chunk: ChunkKey, id: u64) -> [u8; ENTITY_KEY_LEN] {
    let mut out = [0; ENTITY_KEY_LEN];
    out[..KEY_LEN].copy_from_slice(&encode_key(chunk));
    out[KEY_LEN..].copy_from_slice(&id.to_be_bytes());
    out
}

/// The inverse of [`encode_entity_key`].
pub fn decode_entity_key(bytes: &[u8; ENTITY_KEY_LEN]) -> (ChunkKey, u64) {
    let mut chunk = [0; KEY_LEN];
    chunk.copy_from_slice(&bytes[..KEY_LEN]);
    let mut id = [0; 8];
    id.copy_from_slice(&bytes[KEY_LEN..]);
    (decode_key(&chunk), u64::from_be_bytes(id))
}

/// One codec's part of a saved entity: the codec's stable name, the version of its bytes and
/// the bytes (see `persist::EntityCodec`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityPart {
    /// The codec's name (`boxel:body`).
    pub codec: String,
    /// The version of `bytes`, as the codec wrote it.
    pub version: u32,
    /// What the codec saved.
    pub bytes: Vec<u8>,
}

/// The record of one entity: its parts, in the order given.
pub fn encode_entity(parts: &[EntityPart]) -> Vec<u8> {
    seal_entity(&entity_payload(parts))
}

/// The inverse of [`encode_entity`].
pub fn decode_entity(bytes: &[u8]) -> Result<Vec<EntityPart>, FormatError> {
    entity_parts(&open_entity(bytes)?)
}

/// The content of an entity record before its envelope: what the main thread makes (the save
/// job seals it, [`seal_entity`]).
pub(crate) fn entity_payload(parts: &[EntityPart]) -> Vec<u8> {
    let mut payload = Vec::new();
    put_varint(&mut payload, parts.len() as u64);
    for part in parts {
        put_varint(&mut payload, part.codec.len() as u64);
        payload.extend_from_slice(part.codec.as_bytes());
        put_varint(&mut payload, u64::from(part.version));
        put_varint(&mut payload, part.bytes.len() as u64);
        payload.extend_from_slice(&part.bytes);
    }
    payload
}

/// An entity record from its content ([`entity_payload`]): the envelope, checksum and LZ4.
pub(crate) fn seal_entity(payload: &[u8]) -> Vec<u8> {
    seal(ENTITY_MAGIC, ENTITY_VERSION, payload)
}

/// The content of an entity record ([`seal_entity`]).
pub(crate) fn open_entity(bytes: &[u8]) -> Result<Vec<u8>, FormatError> {
    open(ENTITY_MAGIC, ENTITY_VERSION, bytes)
}

/// The parts in the content of an entity record ([`entity_payload`]).
pub(crate) fn entity_parts(payload: &[u8]) -> Result<Vec<EntityPart>, FormatError> {
    let mut r = Reader::new(payload);
    let n = r.count(payload.len())?;
    let mut parts = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.count(payload.len())?;
        let codec = std::str::from_utf8(r.take(len)?)
            .map_err(|_| FormatError::Damaged("a codec name is not UTF-8"))?
            .to_string();
        let version = u32::try_from(r.varint()?)
            .map_err(|_| FormatError::Damaged("codec version out of range"))?;
        let len = r.count(payload.len())?;
        let bytes = r.take(len)?.to_vec();
        parts.push(EntityPart {
            codec,
            version,
            bytes,
        });
    }
    r.finish()?;
    Ok(parts)
}

// ---- Pending ticks and the clock -----------------------------------------------------------

/// The pending block ticks blob: position and game ticks from the save to the tick, in the order
/// they run.
pub(crate) fn encode_ticks(ticks: &[(boxel_voxel::BlockPos, u64)]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + ticks.len() * 26);
    put_varint(&mut payload, ticks.len() as u64);
    for (p, delay) in ticks {
        for v in [p.x, p.y, p.z] {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        put_varint(&mut payload, *delay);
    }
    seal(META_MAGIC, META_VERSION, &payload)
}

/// The inverse of [`encode_ticks`].
pub(crate) fn decode_ticks(bytes: &[u8]) -> Result<Vec<(boxel_voxel::BlockPos, u64)>, FormatError> {
    let payload = open(META_MAGIC, META_VERSION, bytes)?;
    let mut r = Reader::new(&payload);
    let n = r.count(payload.len() / 25)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let (x, y, z) = (r.i64()?, r.i64()?, r.i64()?);
        out.push((boxel_voxel::BlockPos::new(x, y, z), r.varint()?));
    }
    r.finish()?;
    Ok(out)
}

/// The world clock blob (`TimeOfDay::tick`); also the next saved entity id.
pub(crate) fn encode_clock(tick: u64) -> Vec<u8> {
    let mut payload = Vec::new();
    put_varint(&mut payload, tick);
    seal(META_MAGIC, META_VERSION, &payload)
}

/// The inverse of [`encode_clock`].
pub(crate) fn decode_clock(bytes: &[u8]) -> Result<u64, FormatError> {
    let payload = open(META_MAGIC, META_VERSION, bytes)?;
    let mut r = Reader::new(&payload);
    let tick = r.varint()?;
    r.finish()?;
    Ok(tick)
}

// ---- The world generation record -----------------------------------------------------------

/// A string: its byte length and its bytes.
fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// `None` as the byte 0, `Some(id)` as the byte 1 and the id.
fn put_opt_id(out: &mut Vec<u8>, id: Option<&GenId>) {
    match id {
        None => out.push(0),
        Some(id) => {
            out.push(1);
            put_str(out, id.as_str());
        }
    }
}

/// A digest line: its scope, registry, id, version and parameter hash.
fn put_line(out: &mut Vec<u8>, line: &DigestEntry) {
    put_opt_id(out, line.scope.dimension.as_ref());
    put_opt_id(out, line.scope.realm.as_ref());
    put_str(out, &line.registry);
    put_str(out, line.id.as_str());
    put_varint(out, u64::from(line.version));
    out.extend_from_slice(&line.params.to_le_bytes());
}

/// A realm span: a byte of flags (1: a bottom plane, 2: a top plane), then the planes.
fn put_span(out: &mut Vec<u8>, span: RealmSpan) {
    out.push(u8::from(span.lo.is_some()) | (u8::from(span.hi.is_some()) << 1));
    for plane in span.planes() {
        out.extend_from_slice(&plane.to_le_bytes());
    }
}

impl Reader<'_> {
    /// A string of at most `max` bytes.
    fn str(&mut self, max: usize) -> Result<&str, FormatError> {
        let len = self.count(max)?;
        std::str::from_utf8(self.take(len)?)
            .map_err(|_| FormatError::Damaged("a name is not UTF-8"))
    }

    /// A world generation id.
    fn id(&mut self, max: usize) -> Result<GenId, FormatError> {
        GenId::new(self.str(max)?)
            .map_err(|_| FormatError::Damaged("an invalid world generation id"))
    }

    /// [`put_opt_id`] back.
    fn opt_id(&mut self, max: usize) -> Result<Option<GenId>, FormatError> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.id(max).map(Some),
            _ => Err(FormatError::Damaged("a bad option tag")),
        }
    }

    /// A `u32` written as a varint.
    fn u32(&mut self) -> Result<u32, FormatError> {
        u32::try_from(self.varint()?).map_err(|_| FormatError::Damaged("a version out of range"))
    }

    /// [`put_line`] back.
    fn line(&mut self, max: usize) -> Result<DigestEntry, FormatError> {
        let scope = DigestScope {
            dimension: self.opt_id(max)?,
            realm: self.opt_id(max)?,
        };
        Ok(DigestEntry {
            scope,
            registry: Cow::Owned(self.str(max)?.to_owned()),
            id: self.id(max)?,
            version: self.u32()?,
            params: self.u64()?,
        })
    }

    /// [`put_span`] back.
    fn span(&mut self) -> Result<RealmSpan, FormatError> {
        let flags = self.u8()?;
        if flags & !3 != 0 {
            return Err(FormatError::Damaged("bad span flags"));
        }
        let lo = (flags & 1 != 0).then(|| self.i64()).transpose()?;
        let hi = (flags & 2 != 0).then(|| self.i64()).transpose()?;
        Ok(RealmSpan::new(lo, hi))
    }
}

/// The world generation record blob (`boxel:worldgen`, ADR 0039 sec. 17), in its own envelope
/// and record version ([`WORLDGEN_VERSION`]): the preset's line, the dimension name table, then
/// per dimension its id, its realms (id and span), its spec (kind, version, parameter hash) and
/// its digest lines.
pub fn encode_worldgen(record: &WorldgenRecord) -> Vec<u8> {
    let mut payload = Vec::new();
    put_line(&mut payload, &record.preset);
    put_varint(&mut payload, record.names.len() as u64);
    for name in &record.names {
        put_str(&mut payload, name.as_str());
    }
    put_varint(&mut payload, record.dimensions.len() as u64);
    for dim in &record.dimensions {
        put_str(&mut payload, dim.id.as_str());
        put_varint(&mut payload, dim.realms.len() as u64);
        for (realm, span) in &dim.realms {
            put_str(&mut payload, realm.as_str());
            put_span(&mut payload, *span);
        }
        put_str(&mut payload, dim.generator.kind.as_str());
        put_varint(&mut payload, u64::from(dim.generator.version));
        payload.extend_from_slice(&dim.generator.params.to_le_bytes());
        let lines = dim.digest.entries();
        put_varint(&mut payload, lines.len() as u64);
        for line in lines {
            put_line(&mut payload, line);
        }
    }
    seal(WORLDGEN_MAGIC, WORLDGEN_VERSION, &payload)
}

/// The inverse of [`encode_worldgen`]. Another record version is [`FormatError::Version`].
pub fn decode_worldgen(bytes: &[u8]) -> Result<WorldgenRecord, FormatError> {
    let payload = open(WORLDGEN_MAGIC, WORLDGEN_VERSION, bytes)?;
    let max = payload.len();
    let mut r = Reader::new(&payload);
    let preset = r.line(max)?;
    let n = r.count(max)?;
    let names = (0..n).map(|_| r.id(max)).collect::<Result<Vec<_>, _>>()?;
    let n = r.count(max)?;
    let mut dimensions = Vec::with_capacity(n);
    for _ in 0..n {
        let id = r.id(max)?;
        let n = r.count(max)?;
        let mut realms = Vec::with_capacity(n);
        for _ in 0..n {
            realms.push((r.id(max)?, r.span()?));
        }
        let generator = SpecLine {
            kind: r.id(max)?,
            version: r.u32()?,
            params: r.u64()?,
        };
        let n = r.count(max)?;
        let lines = (0..n).map(|_| r.line(max)).collect::<Result<Vec<_>, _>>()?;
        dimensions.push(DimensionRecord {
            id,
            realms,
            generator,
            digest: RegistryDigest::from_entries(lines),
        });
    }
    r.finish()?;
    Ok(WorldgenRecord {
        preset,
        names,
        dimensions,
    })
}

#[cfg(test)]
pub(crate) mod tests;
