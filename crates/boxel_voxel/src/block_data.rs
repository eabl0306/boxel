//! Data a block cell carries next to its state (ADR 0048): an inventory, a few typed fields or
//! bytes the game owns. A furnace keeps its slots and its fire here, a lectern its book, an item
//! frame (later) its item. The engine names no content: the fields are the game's, by name.
//!
//! The data of a chunk lives in its [`ChunkData`](crate::ChunkData), by cell
//! ([`ChunkData::block_data`](crate::ChunkData::block_data)): it is saved, loaded and streamed
//! with the chunk. A chunk without data pays one empty pointer.

use std::collections::BTreeMap;
use std::sync::Arc;

/// A stack of items in a block's inventory: the item by its name (the game's id, so a save
/// keeps it whatever the game's runtime ids), how many and its wear. An item the game no longer
/// has stays as its name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemStack {
    /// The item's name (`game:coal`).
    pub item: Arc<str>,
    /// How many, at least 1.
    pub count: u32,
    /// Uses taken off a tool (0: new or not a tool).
    pub damage: u32,
}

impl ItemStack {
    /// `count` of `item`, unworn.
    pub fn new(item: impl Into<Arc<str>>, count: u32) -> Self {
        Self {
            item: item.into(),
            count,
            damage: 0,
        }
    }
}

/// One field of a block's data.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DataValue {
    /// A number (a furnace's cook time, a page).
    Int(i64),
    /// A short text (a name, a sign's line).
    Text(Box<str>),
    /// Bytes only the game reads.
    Bytes(Box<[u8]>),
    /// An inventory: one entry per slot, `None` for an empty slot.
    Items(Box<[Option<ItemStack>]>),
}

/// The data of one block cell: named fields, kept sorted by name. Cheap to read; written whole
/// through the world's edit queue (`boxel_world::EditQueue::set_data`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlockData {
    fields: Vec<(Box<str>, DataValue)>,
}

impl BlockData {
    /// No fields.
    pub fn new() -> Self {
        Self::default()
    }

    /// The field `name`.
    pub fn get(&self, name: &str) -> Option<&DataValue> {
        let i = self.find(name).ok()?;
        Some(&self.fields[i].1)
    }

    /// Sets the field `name` (replaces an earlier one).
    pub fn set(&mut self, name: &str, value: DataValue) {
        match self.find(name) {
            Ok(i) => self.fields[i].1 = value,
            Err(i) => self.fields.insert(i, (name.into(), value)),
        }
    }

    /// The same data with the field `name` set.
    #[must_use]
    pub fn with(mut self, name: &str, value: DataValue) -> Self {
        self.set(name, value);
        self
    }

    /// Removes the field `name`, returning it.
    pub fn remove(&mut self, name: &str) -> Option<DataValue> {
        let i = self.find(name).ok()?;
        Some(self.fields.remove(i).1)
    }

    /// The number field `name` (`None` if missing or not a number).
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.get(name)? {
            DataValue::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// The text field `name`.
    pub fn text(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            DataValue::Text(t) => Some(t),
            _ => None,
        }
    }

    /// The bytes field `name`.
    pub fn bytes(&self, name: &str) -> Option<&[u8]> {
        match self.get(name)? {
            DataValue::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// The inventory field `name`, by slot.
    pub fn items(&self, name: &str) -> Option<&[Option<ItemStack>]> {
        match self.get(name)? {
            DataValue::Items(items) => Some(items),
            _ => None,
        }
    }

    /// Every field, by name.
    pub fn fields(&self) -> impl ExactSizeIterator<Item = (&str, &DataValue)> {
        self.fields.iter().map(|(k, v)| (&**k, v))
    }

    /// No fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Heap bytes held, about (names, texts, bytes, stacks).
    pub fn heap_bytes(&self) -> usize {
        let fields = self.fields.capacity() * size_of::<(Box<str>, DataValue)>();
        let values: usize = self
            .fields
            .iter()
            .map(|(k, v)| {
                k.len()
                    + match v {
                        DataValue::Int(_) => 0,
                        DataValue::Text(t) => t.len(),
                        DataValue::Bytes(b) => b.len(),
                        DataValue::Items(items) => items.len() * size_of::<Option<ItemStack>>(),
                    }
            })
            .sum();
        fields + values
    }

    fn find(&self, name: &str) -> Result<usize, usize> {
        self.fields.binary_search_by(|(k, _)| (**k).cmp(name))
    }
}

/// The data of a chunk's cells, by local index. `None` (no allocation) while no cell has data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::box_collection,
    reason = "one word in every chunk, where an empty map would take three"
)]
pub(crate) struct CellData(Option<Box<BTreeMap<u16, BlockData>>>);

impl CellData {
    pub(crate) fn get(&self, index: u16) -> Option<&BlockData> {
        self.0.as_ref()?.get(&index)
    }

    pub(crate) fn set(&mut self, index: u16, data: Option<BlockData>) -> Option<BlockData> {
        match data {
            Some(data) => self.0.get_or_insert_default().insert(index, data),
            None => {
                let map = self.0.as_mut()?;
                let old = map.remove(&index);
                if map.is_empty() {
                    self.0 = None;
                }
                old
            }
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (u16, &BlockData)> {
        self.0.iter().flat_map(|m| m.iter().map(|(&i, d)| (i, d)))
    }

    pub(crate) fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |m| m.len())
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.0.as_ref().map_or(0, |m| {
            m.values()
                .map(|d| size_of::<(u16, BlockData)>() + d.heap_bytes())
                .sum()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_stay_sorted_and_typed() {
        let mut d = BlockData::new()
            .with("lit", DataValue::Int(80))
            .with("items", DataValue::Items(vec![None; 3].into()));
        d.set("cook", DataValue::Int(5));
        d.set("lit", DataValue::Int(79));
        let names: Vec<&str> = d.fields().map(|(k, _)| k).collect();
        assert_eq!(names, ["cook", "items", "lit"]);
        assert_eq!(d.int("lit"), Some(79));
        assert_eq!(d.int("items"), None, "not a number");
        assert_eq!(d.items("items").map(<[_]>::len), Some(3));
        assert_eq!(d.text("lit"), None);
        assert_eq!(d.remove("cook"), Some(DataValue::Int(5)));
        assert_eq!(d.get("cook"), None);
        assert!(!d.is_empty());
    }

    /// A chunk's data rides in its snapshot: a clone has it, equality sees it, and writing it
    /// leaves the blocks and their version alone.
    #[test]
    fn a_chunk_keeps_its_cells_data_apart_from_its_blocks() {
        use crate::{BlockState, ChunkData, LocalPos};
        let mut chunk = ChunkData::uniform(BlockState(1));
        assert!(!chunk.has_block_data());
        let before = chunk.clone();
        let (a, b) = (LocalPos::new(31, 0, 5), LocalPos::new(0, 1, 0));
        let data = BlockData::new().with("page", DataValue::Int(3));
        chunk.set_block_data(a, Some(data.clone()));
        chunk.set_block_data(b, Some(BlockData::new()));
        assert_eq!(chunk.version, 0);
        assert_eq!(chunk.as_uniform(), Some(BlockState(1)));
        assert_ne!(chunk, before, "equality sees the data");
        assert_eq!(chunk.clone(), chunk);
        assert_eq!(chunk.block_data(a), Some(&data));
        // In local index order: y first.
        let cells: Vec<LocalPos> = chunk.block_data_cells().map(|(p, _)| p).collect();
        assert_eq!(cells, [a, b]);
        assert_eq!(chunk.block_data_len(), 2);
        assert!(chunk.heap_bytes() > before.heap_bytes());
        chunk.set_block_data(a, None);
        chunk.set_block_data(b, None);
        assert_eq!(chunk, before);
    }

    #[test]
    fn cells_allocate_only_with_data() {
        let mut cells = CellData::default();
        assert_eq!(cells.len(), 0);
        assert_eq!(cells.set(7, None), None);
        assert!(cells.0.is_none());
        let data = BlockData::new().with("name", DataValue::Text("x".into()));
        assert_eq!(cells.set(7, Some(data.clone())), None);
        assert_eq!(cells.get(7), Some(&data));
        assert_eq!(cells.set(7, None), Some(data));
        assert!(cells.0.is_none(), "the last one gone frees the map");
    }
}
