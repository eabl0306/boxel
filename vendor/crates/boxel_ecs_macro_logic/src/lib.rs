// boxel: modified by tools/refork (crate renames, platform shim, test-only patches or backports).
//! Reusable `boxel_ecs` macro logic. This enables defining derives that internally derive ECS traits
//! like `Component`.

/// `Component` macro logic. The primary interface is [`DeriveComponent`](component::DeriveComponent).
pub mod component;

/// `MapEntities` macro logic.
pub mod map_entities;
