//! [`WorldScope`]: which resources and messages belong to one world (engine and game alike).
use boxel_app::App;
use boxel_ecs::message::{Message, Messages};
use boxel_ecs::prelude::*;
use std::any::{TypeId, type_name};

/// A registered per-world resource: its type and the check that it is empty after a teardown.
struct ScopedResource {
    type_id: TypeId,
    name: &'static str,
    /// `None` when the resource is missing (a plugin registered it but never inserted it).
    is_closed: Box<dyn Fn(&World) -> Option<bool> + Send + Sync>,
}

/// A registered per-world message: cleared by the lifecycle after every teardown.
struct ScopedMessage {
    type_id: TypeId,
    name: &'static str,
    clear: fn(&mut World),
    len: fn(&World) -> usize,
}

/// The per-world resources and messages of the app ([`WorldScopeApp`]). The lifecycle checks the
/// resources right after every teardown in debug builds and tests, and clears the messages.
#[derive(Resource, Default)]
pub struct WorldScope {
    resources: Vec<ScopedResource>,
    messages: Vec<ScopedMessage>,
    /// The resources whose check failed after the last teardown (debug builds and tests).
    failed: Vec<&'static str>,
    /// The checks ran after the last teardown.
    checked: bool,
}

impl WorldScope {
    /// Every per-world resource type, with its name.
    pub fn resources(&self) -> impl Iterator<Item = (TypeId, &'static str)> + '_ {
        self.resources.iter().map(|r| (r.type_id, r.name))
    }

    /// Every per-world message `M`: the type of its `Messages<M>` resource and the name of `M`.
    pub fn messages(&self) -> impl Iterator<Item = (TypeId, &'static str)> + '_ {
        self.messages.iter().map(|m| (m.type_id, m.name))
    }

    /// After the last teardown: `Some` with the resources not back to empty (an empty list when
    /// every check held), `None` when no check ran (release builds, or no close yet).
    pub fn last_check(&self) -> Option<&[&'static str]> {
        self.checked.then_some(self.failed.as_slice())
    }

    /// The resources not empty now, by name (tests).
    pub fn not_closed(&self, world: &World) -> Vec<&'static str> {
        self.resources
            .iter()
            .filter(|r| (r.is_closed)(world) == Some(false))
            .map(|r| r.name)
            .collect()
    }

    /// Messages of the registered types waiting, in all (tests).
    pub fn messages_waiting(&self, world: &World) -> usize {
        self.messages.iter().map(|m| (m.len)(world)).sum()
    }

    fn add_resource(&mut self, resource: ScopedResource) {
        if !self.resources.iter().any(|r| r.type_id == resource.type_id) {
            self.resources.push(resource);
        }
    }

    fn add_message(&mut self, message: ScopedMessage) {
        if !self.messages.iter().any(|m| m.type_id == message.type_id) {
            self.messages.push(message);
        }
    }
}

/// Clears every registered message, then (debug builds) checks every registered resource.
pub(crate) fn after_teardown(world: &mut World) {
    world.resource_scope(|world, mut scope: Mut<WorldScope>| {
        for m in &scope.messages {
            (m.clear)(world);
        }
        if cfg!(any(debug_assertions, test)) {
            let failed = scope.not_closed(world);
            if !failed.is_empty() {
                tracing::error!(
                    ?failed,
                    "per-world resources are not empty after the world's teardown"
                );
            }
            scope.failed = failed;
            scope.checked = true;
        }
    });
}

/// Per-world registration on an [`App`].
pub trait WorldScopeApp {
    /// `R` belongs to one world: a system in a [`TeardownSystems`](super::TeardownSystems) set
    /// puts it back as a fresh process has it, and `is_closed` holds right after the teardown
    /// (checked in debug builds and tests). The census test of the engine fails on a resource
    /// that is neither registered nor on its app-wide list.
    fn world_resource<R: Resource>(&mut self, is_closed: fn(&R) -> bool) -> &mut Self;
    /// `M` belongs to one world: the lifecycle clears it after every teardown
    /// (`Messages::clear`, readers' cursors stay valid).
    fn world_message<M: Message>(&mut self) -> &mut Self;
}

impl WorldScopeApp for App {
    fn world_resource<R: Resource>(&mut self, is_closed: fn(&R) -> bool) -> &mut Self {
        self.init_resource::<WorldScope>();
        self.world_mut()
            .resource_mut::<WorldScope>()
            .add_resource(ScopedResource {
                type_id: TypeId::of::<R>(),
                name: type_name::<R>(),
                is_closed: Box::new(move |world| world.get_resource::<R>().map(is_closed)),
            });
        self
    }

    fn world_message<M: Message>(&mut self) -> &mut Self {
        self.init_resource::<WorldScope>();
        self.world_mut()
            .resource_mut::<WorldScope>()
            .add_message(ScopedMessage {
                type_id: TypeId::of::<Messages<M>>(),
                name: type_name::<M>(),
                clear: |world| {
                    if let Some(mut messages) = world.get_resource_mut::<Messages<M>>() {
                        messages.clear();
                    }
                },
                len: |world| world.get_resource::<Messages<M>>().map_or(0, Messages::len),
            });
        self
    }
}
