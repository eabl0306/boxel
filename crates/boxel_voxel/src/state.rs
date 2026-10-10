//! Block state properties (ADR 0019), Minecraft "flattening" style: a block has named properties,
//! each with a finite list of string values, and every combination of values is one dense
//! [`BlockState`](crate::BlockState) id. The ids of a block are contiguous; its properties are kept
//! sorted by name (so the property string matches a Minecraft blockstate variant key) and the last
//! one varies fastest.
use std::fmt;

/// A block state property: a name and its values (`axis` = `x | y | z`), and the value of the
/// block's default state. Names and values are `[a-z0-9_]`, non-empty.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Property {
    name: String,
    values: Vec<String>,
    /// Index of the default value in `values` (out of range when [`Property::with_default`] got
    /// an unknown value: the registry rejects it).
    default: usize,
}

impl Property {
    /// A property whose default is its first value.
    pub fn new<V: Into<String>>(
        name: impl Into<String>,
        values: impl IntoIterator<Item = V>,
    ) -> Self {
        Self {
            name: name.into(),
            values: values.into_iter().map(Into::into).collect(),
            default: 0,
        }
    }

    /// A `false | true` property, default `false` (fence sides).
    pub fn boolean(name: impl Into<String>) -> Self {
        Self::new(name, ["false", "true"])
    }

    /// An integer property with the values of `range` in order, default its start (water
    /// `level` `0..=15`).
    pub fn int(name: impl Into<String>, range: std::ops::RangeInclusive<u8>) -> Self {
        Self::new(name, range.map(|v| v.to_string()))
    }

    /// The same property with `value` as its default (an unknown value makes the registration
    /// fail with [`RegistryError::InvalidProperty`](crate::RegistryError::InvalidProperty)).
    #[must_use]
    pub fn with_default(mut self, value: &str) -> Self {
        self.default = self.index_of(value).unwrap_or(usize::MAX);
        self
    }

    /// Name (`axis`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Values in id order.
    pub fn values(&self) -> &[String] {
        &self.values
    }

    /// The value of the default state.
    pub fn default_value(&self) -> &str {
        self.values.get(self.default).map_or("", String::as_str)
    }

    /// Index of `value` in [`Property::values`].
    pub fn index_of(&self, value: &str) -> Option<usize> {
        self.values.iter().position(|v| v == value)
    }

    pub(crate) fn default_index(&self) -> usize {
        self.default
    }

    /// Names and values are `[a-z0-9_]` and non-empty, values are unique and the default is one
    /// of them.
    pub(crate) fn is_valid(&self) -> bool {
        let word = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        };
        word(&self.name)
            && !self.values.is_empty()
            && self.values.iter().all(|v| word(v))
            && self
                .values
                .iter()
                .enumerate()
                .all(|(i, v)| !self.values[..i].contains(v))
            && self.default < self.values.len()
    }
}

/// The property values of one state: given to the per-state function of
/// [`BlockRegistryBuilder::add_block_with_states`](crate::BlockRegistryBuilder::add_block_with_states)
/// and returned by [`BlockRegistry::values`](crate::BlockRegistry::values). Its `Display` is
/// `property=value,...` (empty without properties), a Minecraft blockstate variant key.
#[derive(Clone, Copy, Debug)]
pub struct StateValues<'a> {
    pub(crate) properties: &'a [Property],
    /// Id step of each property (the last one is 1).
    pub(crate) strides: &'a [u16],
    /// The state's offset from the first state of its block.
    pub(crate) offset: u16,
}

impl<'a> StateValues<'a> {
    /// No properties (a state outside the registry).
    pub(crate) const EMPTY: StateValues<'static> = StateValues {
        properties: &[],
        strides: &[],
        offset: 0,
    };

    fn value_index(&self, i: usize) -> usize {
        usize::from(self.offset / self.strides[i]) % self.properties[i].values.len()
    }

    /// The value of property `name` (`None` if the block has no such property).
    pub fn get(&self, name: &str) -> Option<&'a str> {
        let i = self.properties.iter().position(|p| p.name == name)?;
        Some(self.properties[i].values[self.value_index(i)].as_str())
    }

    /// The index of the value of `name` in its [`Property::values`].
    pub fn index(&self, name: &str) -> Option<usize> {
        let i = self.properties.iter().position(|p| p.name == name)?;
        Some(self.value_index(i))
    }

    /// `(property, value)` pairs, properties sorted by name.
    pub fn iter(&self) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        let values = *self;
        (0..values.properties.len()).map(move |i| {
            let p = &values.properties[i];
            (p.name.as_str(), p.values[values.value_index(i)].as_str())
        })
    }

    /// True for a block without properties.
    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }
}

impl fmt::Display for StateValues<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (k, v)) in self.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{k}={v}")?;
        }
        Ok(())
    }
}

/// Why a block state string (`game:stone_slab[type=top]`) did not parse.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum StateParseError {
    /// Not `name` or `name[property=value,...]`.
    #[error("malformed block state {0:?}")]
    Syntax(String),
    /// No block of that name.
    #[error("unknown block {0}")]
    UnknownBlock(String),
    /// The block has no property of that name.
    #[error("{block} has no property {property}")]
    UnknownProperty {
        /// Block name.
        block: String,
        /// Property name.
        property: String,
    },
    /// The property has no such value.
    #[error("property {property} of {block} has no value {value}")]
    UnknownValue {
        /// Block name.
        block: String,
        /// Property name.
        property: String,
        /// The value given.
        value: String,
    },
    /// The same property twice.
    #[error("property {property} of {block} given twice")]
    Repeated {
        /// Block name.
        block: String,
        /// Property name.
        property: String,
    },
}

/// Splits `name[a=b,c=d]` into the name and its `(property, value)` pairs (`name` alone has none).
pub(crate) fn split_state(text: &str) -> Result<(&str, Vec<(&str, &str)>), StateParseError> {
    let syntax = || StateParseError::Syntax(text.to_string());
    let Some((name, rest)) = text.split_once('[') else {
        return Ok((text.trim(), Vec::new()));
    };
    let body = rest.strip_suffix(']').ok_or_else(syntax)?;
    let mut pairs = Vec::new();
    if !body.trim().is_empty() {
        for pair in body.split(',') {
            let (k, v) = pair.split_once('=').ok_or_else(syntax)?;
            let (k, v) = (k.trim(), v.trim());
            if k.is_empty() || v.is_empty() {
                return Err(syntax());
            }
            pairs.push((k, v));
        }
    }
    Ok((name.trim(), pairs))
}

/// A state written as `name[property=value,...]` (just `name` for a block without properties),
/// from [`BlockRegistry::state_name`](crate::BlockRegistry::state_name).
#[derive(Clone, Copy, Debug)]
pub struct StateName<'a> {
    pub(crate) name: &'a str,
    pub(crate) values: StateValues<'a>,
}

impl<'a> StateName<'a> {
    /// The block name.
    pub fn block(&self) -> &'a str {
        self.name
    }

    /// The property values.
    pub fn values(&self) -> StateValues<'a> {
        self.values
    }
}

impl fmt::Display for StateName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)?;
        if !self.values.is_empty() {
            write!(f, "[{}]", self.values)?;
        }
        Ok(())
    }
}
