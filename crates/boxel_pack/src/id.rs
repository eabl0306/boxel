//! Resource identifiers (`namespace:path`).

use std::fmt;
use std::str::FromStr;

/// A namespaced resource path, e.g. `minecraft:block/stone`.
///
/// Paths omit the `assets/<ns>/…` prefix and the file extension. Texture refs in models use the
/// same form (`block/stone` → `assets/minecraft/textures/block/stone.png`).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId {
    /// Namespace (`minecraft` by default).
    pub namespace: String,
    /// Path inside the namespace (`block/stone`).
    pub path: String,
}

impl ResourceId {
    /// Default Minecraft namespace.
    pub const DEFAULT_NAMESPACE: &'static str = "minecraft";

    /// Creates an id from namespace and path.
    pub fn new(namespace: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            path: path.into(),
        }
    }

    /// Parses `"ns:path"` or `"path"` (namespace defaults to `minecraft`).
    pub fn parse(s: &str) -> Self {
        match s.split_once(':') {
            Some((ns, path)) if !ns.is_empty() && !path.is_empty() => {
                Self::new(ns.to_string(), path.to_string())
            }
            _ => Self::new(Self::DEFAULT_NAMESPACE, s.to_string()),
        }
    }

    /// Formats as `namespace:path`.
    pub fn as_str_pair(&self) -> (&str, &str) {
        (&self.namespace, &self.path)
    }
}

impl FromStr for ResourceId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.path)
    }
}

impl fmt::Debug for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ResourceId({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_and_without_namespace() {
        assert_eq!(
            ResourceId::parse("minecraft:block/stone"),
            ResourceId::new("minecraft", "block/stone")
        );
        assert_eq!(
            ResourceId::parse("block/stone"),
            ResourceId::new("minecraft", "block/stone")
        );
        assert_eq!(
            ResourceId::parse("mymod:item/gem"),
            ResourceId::new("mymod", "item/gem")
        );
    }
}
