use crate::{App, Plugin};
use std::any::TypeId;

/// A named, ordered set of plugins that can be configured before adding.
pub trait PluginGroup: Sized {
    /// Builds the ordered plugin list.
    fn build(self) -> PluginGroupBuilder;

    /// Shortcut for `self.build().set(plugin)`.
    fn set<T: Plugin>(self, plugin: T) -> PluginGroupBuilder {
        self.build().set(plugin)
    }
}

struct Entry {
    type_id: TypeId,
    plugin: Box<dyn Plugin>,
    enabled: bool,
}

/// Ordered plugin list with replace/disable support.
pub struct PluginGroupBuilder {
    entries: Vec<Entry>,
}

impl PluginGroupBuilder {
    /// Empty builder.
    pub fn start() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Appends a plugin (replaces in place if the same type is already present).
    #[expect(
        clippy::should_implement_trait,
        reason = "Bevy-style builder method name"
    )]
    pub fn add<T: Plugin>(mut self, plugin: T) -> Self {
        let type_id = TypeId::of::<T>();
        if let Some(e) = self.entries.iter_mut().find(|e| e.type_id == type_id) {
            e.plugin = Box::new(plugin);
        } else {
            self.entries.push(Entry {
                type_id,
                plugin: Box::new(plugin),
                enabled: true,
            });
        }
        self
    }

    /// Replaces an existing plugin of the same type.
    ///
    /// # Panics
    /// If no plugin of type `T` is in the group.
    pub fn set<T: Plugin>(mut self, plugin: T) -> Self {
        let type_id = TypeId::of::<T>();
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.type_id == type_id)
            .unwrap_or_else(|| {
                panic!("{} is not in this plugin group", std::any::type_name::<T>())
            });
        e.plugin = Box::new(plugin);
        self
    }

    /// Disables a plugin (kept in order, skipped when added).
    pub fn disable<T: Plugin>(mut self) -> Self {
        let type_id = TypeId::of::<T>();
        for e in self.entries.iter_mut().filter(|e| e.type_id == type_id) {
            e.enabled = false;
        }
        self
    }

    /// Re-enables a disabled plugin.
    pub fn enable<T: Plugin>(mut self) -> Self {
        let type_id = TypeId::of::<T>();
        for e in self.entries.iter_mut().filter(|e| e.type_id == type_id) {
            e.enabled = true;
        }
        self
    }

    /// Inserts `plugin` right before the plugin of type `Target`.
    ///
    /// # Panics
    /// If `Target` is not in the group.
    pub fn add_before<Target: Plugin, T: Plugin>(self, plugin: T) -> Self {
        let _ = TypeId::of::<Target>();
        todo!("WP1")
    }

    /// Inserts `plugin` right after the plugin of type `Target`.
    ///
    /// # Panics
    /// If `Target` is not in the group.
    pub fn add_after<Target: Plugin, T: Plugin>(self, plugin: T) -> Self {
        let _ = TypeId::of::<Target>();
        todo!("WP1")
    }

    /// Adds every enabled plugin to `app`, in order.
    pub fn finish(self, app: &mut App) {
        for e in self.entries {
            if e.enabled {
                app.add_boxed_plugin(e.plugin);
            }
        }
    }
}

impl PluginGroup for PluginGroupBuilder {
    fn build(self) -> PluginGroupBuilder {
        self
    }
}
