//! Bodies saved with the world (ADR 0024 section 3): [`BodyCodec`], the physics part of the
//! record of a [`Saved`](boxel_world::persist::Saved) entity. [`PhysicsPlugin`](crate::PhysicsPlugin)
//! registers it, so a body with `Saved` is saved with its chunk and comes back as it was.
//!
//! What is saved: the kind, the exact position ([`WorldPos`]: the block's `i64`s and the offset),
//! the velocity, the [`BodySettings`], the [`Collider`], the [`Contacts`] of the latest step,
//! whether it sleeps and how many steps it has been still, and, when the body has them, its
//! [`Interpolated`] position before the latest step, [`ReportContacts`], the
//! [`CharacterController`] with its [`MoveIntent`], the [`Projectile`] (the shooter it ignores
//! is saved by its saved id, so only a saved shooter is kept) and the [`Swimmer`] (with the land
//! settings it keeps while in a fluid). Floats are saved bit for bit, so a body that comes back
//! steps exactly as it would have. Not saved: [`Frozen`](crate::Frozen) (the next step finds it
//! out), the contacts with other bodies and the events (they start again).
use crate::components::{
    BodyPosition, CharacterController, Collider, Contacts, Interpolated, MoveIntent, OnBlock,
    OnEntity, PhysicsSync, Projectile, ReportContacts, Response, Restored, RigidBody, Sleeping,
    Teleport, Velocity,
};
use crate::core::{BodySettings, CollisionLayers, Shape};
use crate::swim::{SwimSettings, Swimmer};
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, Vec2};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, WorldPos};
use boxel_world::persist::{EntityCodec, SavedId, SavedIds};

/// The name [`BodyCodec`] saves its parts under.
pub const BODY_CODEC: &str = "boxel:body";
/// The version of the bytes [`BodyCodec`] writes.
pub const BODY_CODEC_VERSION: u32 = 1;

/// The physics part of a saved entity (ADR 0024): the body of an entity with a [`RigidBody`].
/// [`PhysicsPlugin`](crate::PhysicsPlugin) registers it, so a body with
/// [`Saved`](boxel_world::persist::Saved) is saved with its chunk and comes back as it was.
///
/// It saves the kind, the exact position, the velocity, the [`BodySettings`], the [`Collider`],
/// the [`Contacts`] of the latest step, whether the body sleeps and how many steps it has been
/// still, and when present [`Interpolated`], [`ReportContacts`], the [`CharacterController`]
/// with its [`MoveIntent`], the [`Projectile`] (its shooter only if the shooter is saved) and
/// the [`Swimmer`], floats bit for bit: a body that comes back steps exactly as it would have.
/// A body that comes back is not woken by the chunk loads it came back with.
#[derive(Clone, Copy, Debug, Default)]
pub struct BodyCodec;

// Flags of a saved body.
const COLLIDER: u16 = 1;
const SLEEPING: u16 = 1 << 1;
const INTERPOLATED: u16 = 1 << 2;
const PREVIOUS: u16 = 1 << 3;
const REPORT: u16 = 1 << 4;
const CONTROLLER: u16 = 1 << 5;
const PROJECTILE: u16 = 1 << 6;
const SWIMMER: u16 = 1 << 7;
const ON_GROUND: u16 = 1 << 8;
const IN_FLUID: u16 = 1 << 9;
const GROUND: u16 = 1 << 10;
const KNOWN: u16 = (1 << 11) - 1;

/// Where a body is for saving: a pending [`Teleport`] or a `Transform` written since the latest
/// step (a teleport the next step takes), else its [`BodyPosition`].
fn position_of(world: &World, entity: Entity) -> Option<WorldPos> {
    let e = world.get_entity(entity).ok()?;
    e.get::<RigidBody>()?;
    if let Some(teleport) = e.get::<Teleport>() {
        return Some(teleport.0.normalize());
    }
    let position = e.get::<BodyPosition>().copied().unwrap_or_default();
    let sync = e.get::<PhysicsSync>().copied().unwrap_or_default();
    let shown = e.get::<Transform>().map(|t| t.translation);
    match shown {
        Some(t) if !sync.placed || sync.last_written != Some(t) => Some(WorldPos::from_dvec3(t)),
        _ => Some(position.0.normalize()),
    }
}

/// Writes a saved body.
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
    fn vec(&mut self, v: DVec3) {
        self.f64(v.x);
        self.f64(v.y);
        self.f64(v.z);
    }
    fn block(&mut self, b: BlockPos) {
        self.i64(b.x);
        self.i64(b.y);
        self.i64(b.z);
    }
    fn pos(&mut self, p: WorldPos) {
        self.block(p.block);
        self.vec(p.frac);
    }
    fn settings(&mut self, s: &BodySettings) {
        for v in [
            s.gravity_scale,
            s.drag_xz,
            s.drag_y,
            s.ground_friction,
            s.restitution,
            s.bounce_min_speed,
            s.push_weight,
        ] {
            self.f64(v);
        }
    }
}

/// Reads a saved body; every read checks the length.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], String> {
        if self.0.len() < N {
            return Err("the saved body ends early".to_string());
        }
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        let mut out = [0; N];
        out.copy_from_slice(head);
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take()?))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn vec(&mut self) -> Result<DVec3, String> {
        Ok(DVec3::new(self.f64()?, self.f64()?, self.f64()?))
    }
    fn block(&mut self) -> Result<BlockPos, String> {
        Ok(BlockPos::new(self.i64()?, self.i64()?, self.i64()?))
    }
    fn pos(&mut self) -> Result<WorldPos, String> {
        let block = self.block()?;
        let frac = self.vec()?;
        // Saved normalized: a damaged offset never makes a position that is not one.
        if !(0.0..1.0).contains(&frac.x)
            || !(0.0..1.0).contains(&frac.y)
            || !(0.0..1.0).contains(&frac.z)
        {
            return Err("a saved position is not normalized".to_string());
        }
        Ok(WorldPos { block, frac })
    }
    fn settings(&mut self) -> Result<BodySettings, String> {
        Ok(BodySettings {
            gravity_scale: self.f64()?,
            drag_xz: self.f64()?,
            drag_y: self.f64()?,
            ground_friction: self.f64()?,
            restitution: self.f64()?,
            bounce_min_speed: self.f64()?,
            push_weight: self.f64()?,
        })
    }
    fn finish(self) -> Result<(), String> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err("trailing bytes after the saved body".to_string())
        }
    }
}

impl EntityCodec for BodyCodec {
    fn name(&self) -> &str {
        BODY_CODEC
    }

    fn version(&self) -> u32 {
        BODY_CODEC_VERSION
    }

    fn position(&self, world: &World, entity: Entity) -> Option<BlockPos> {
        position_of(world, entity).map(|p| p.block)
    }

    fn save(&self, world: &World, entity: Entity, ids: &SavedIds<'_>) -> Option<Vec<u8>> {
        let position = position_of(world, entity)?;
        let e = world.get_entity(entity).ok()?;
        let kind = *e.get::<RigidBody>()?;
        let collider = e.get::<Collider>().copied();
        let contacts = e.get::<Contacts>().copied().unwrap_or_default();
        let sync = e.get::<PhysicsSync>().copied().unwrap_or_default();
        let interpolated = e.get::<Interpolated>().copied();
        let controller = e.get::<CharacterController>().copied();
        let projectile = e.get::<Projectile>().copied();
        let swimmer = e.get::<Swimmer>().copied();
        let mut flags = 0;
        for (on, flag) in [
            (collider.is_some(), COLLIDER),
            (e.contains::<Sleeping>(), SLEEPING),
            (interpolated.is_some(), INTERPOLATED),
            (interpolated.is_some_and(|i| i.prev.is_some()), PREVIOUS),
            (e.contains::<ReportContacts>(), REPORT),
            (controller.is_some(), CONTROLLER),
            (projectile.is_some(), PROJECTILE),
            (swimmer.is_some(), SWIMMER),
            (contacts.on_ground, ON_GROUND),
            (contacts.in_fluid, IN_FLUID),
            (contacts.ground_block.is_some(), GROUND),
        ] {
            if on {
                flags |= flag;
            }
        }
        let mut w = Writer(Vec::with_capacity(160));
        w.u8(match kind {
            RigidBody::Dynamic => 0,
            RigidBody::Kinematic => 1,
            RigidBody::Static => 2,
        });
        w.u16(flags);
        w.pos(position);
        w.vec(e.get::<Velocity>().map_or(DVec3::ZERO, |v| v.0));
        w.settings(&e.get::<BodySettings>().copied().unwrap_or_default());
        w.u8(contacts.faces);
        if let Some(ground) = contacts.ground_block {
            w.block(ground);
        }
        w.u32(sync.still);
        if let Some(c) = collider {
            match c.shape {
                Shape::Aabb { half } => {
                    w.u8(0);
                    w.vec(half);
                }
                Shape::Sphere { radius } => {
                    w.u8(1);
                    w.f64(radius);
                }
            }
            w.vec(c.offset);
            w.u8(match c.response {
                Response::Solid => 0,
                Response::Sensor => 1,
            });
            w.u32(c.layers.member);
            w.u32(c.layers.mask);
        }
        if let Some(prev) = interpolated.and_then(|i| i.prev) {
            w.pos(prev.normalize());
        }
        if let Some(c) = controller {
            for v in [
                c.walk_speed,
                c.air_control,
                c.jump_speed,
                c.step_height,
                c.sneak_factor,
                c.sprint_factor,
                c.fly_speed,
                c.fly_vertical_speed,
            ] {
                w.f64(v);
            }
            w.u8(u8::from(c.flying));
            let intent = e.get::<MoveIntent>().copied().unwrap_or_default();
            w.f32(intent.wish.x);
            w.f32(intent.wish.y);
            w.u8(u8::from(intent.jump)
                | (u8::from(intent.up) << 1)
                | (u8::from(intent.sneak) << 2)
                | (u8::from(intent.sprint) << 3));
        }
        if let Some(p) = projectile {
            w.u8(match p.on_block {
                OnBlock::Stop => 0,
                OnBlock::Bounce => 1,
            });
            w.u8(match p.on_entity {
                OnEntity::Stop => 0,
                OnEntity::Pass => 1,
            });
            w.u8(u8::from(p.stuck.is_some()));
            if let Some(stuck) = p.stuck {
                w.block(stuck);
            }
            // Only a saved shooter can be found again.
            let shooter = p.ignore.and_then(|s| ids.id(world, s));
            w.u64(shooter.map_or(0, SavedId::to_bits));
        }
        if let Some(s) = swimmer {
            for v in [
                s.settings.gravity_scale,
                s.settings.drag,
                s.settings.speed,
                s.settings.up_speed,
            ] {
                w.f64(v);
            }
            w.u8(u8::from(s.in_fluid));
            w.u8(u8::from(s.land.is_some()));
            if let Some((settings, walk)) = s.land {
                w.settings(&settings);
                w.f64(walk);
            }
        }
        Some(w.0)
    }

    fn load(
        &self,
        world: &mut World,
        entity: Entity,
        version: u32,
        bytes: &[u8],
        ids: &SavedIds<'_>,
    ) -> Result<(), String> {
        if version != BODY_CODEC_VERSION {
            return Err(format!(
                "saved body version {version} (this build reads {BODY_CODEC_VERSION})"
            ));
        }
        let mut r = Reader(bytes);
        let kind = match r.u8()? {
            0 => RigidBody::Dynamic,
            1 => RigidBody::Kinematic,
            2 => RigidBody::Static,
            _ => return Err("unknown body kind".to_string()),
        };
        let flags = r.u16()?;
        if flags & !KNOWN != 0 {
            return Err("unknown body flags".to_string());
        }
        let has = |flag: u16| flags & flag != 0;
        let position = r.pos()?;
        let velocity = r.vec()?;
        let settings = r.settings()?;
        let faces = r.u8()?;
        let ground_block = if has(GROUND) { Some(r.block()?) } else { None };
        let still = r.u32()?;
        let collider = if has(COLLIDER) {
            let shape = match r.u8()? {
                0 => Shape::Aabb { half: r.vec()? },
                1 => Shape::Sphere { radius: r.f64()? },
                _ => return Err("unknown collider shape".to_string()),
            };
            let offset = r.vec()?;
            let response = match r.u8()? {
                0 => Response::Solid,
                1 => Response::Sensor,
                _ => return Err("unknown collider response".to_string()),
            };
            let layers = CollisionLayers {
                member: r.u32()?,
                mask: r.u32()?,
            };
            Some(Collider {
                shape,
                offset,
                response,
                layers,
            })
        } else {
            None
        };
        let previous = if has(PREVIOUS) { Some(r.pos()?) } else { None };
        let controller = if has(CONTROLLER) {
            let mut c = CharacterController {
                walk_speed: r.f64()?,
                air_control: r.f64()?,
                jump_speed: r.f64()?,
                step_height: r.f64()?,
                sneak_factor: r.f64()?,
                sprint_factor: r.f64()?,
                fly_speed: r.f64()?,
                fly_vertical_speed: r.f64()?,
                flying: false,
            };
            c.flying = r.u8()? != 0;
            let wish = Vec2::new(r.f32()?, r.f32()?);
            let keys = r.u8()?;
            let intent = MoveIntent {
                wish,
                jump: keys & 1 != 0,
                up: keys & 2 != 0,
                sneak: keys & 4 != 0,
                sprint: keys & 8 != 0,
            };
            Some((c, intent))
        } else {
            None
        };
        let projectile = if has(PROJECTILE) {
            let on_block = match r.u8()? {
                0 => OnBlock::Stop,
                1 => OnBlock::Bounce,
                _ => return Err("unknown projectile block rule".to_string()),
            };
            let on_entity = match r.u8()? {
                0 => OnEntity::Stop,
                1 => OnEntity::Pass,
                _ => return Err("unknown projectile entity rule".to_string()),
            };
            let stuck = if r.u8()? != 0 { Some(r.block()?) } else { None };
            let ignore = SavedId::from_bits(r.u64()?).and_then(|id| ids.entity(id));
            Some(Projectile {
                on_block,
                on_entity,
                ignore,
                stuck,
            })
        } else {
            None
        };
        let swimmer = if has(SWIMMER) {
            let settings = SwimSettings {
                gravity_scale: r.f64()?,
                drag: r.f64()?,
                speed: r.f64()?,
                up_speed: r.f64()?,
            };
            let in_fluid = r.u8()? != 0;
            let land = if r.u8()? != 0 {
                Some((r.settings()?, r.f64()?))
            } else {
                None
            };
            Some(Swimmer {
                settings,
                in_fluid,
                land,
            })
        } else {
            None
        };
        r.finish()?;

        // Placed already, where it was: the next step neither teleports nor nudges it.
        let translation = position.to_dvec3();
        let mut e = world.entity_mut(entity);
        e.insert((
            (kind, Transform::from_translation(translation)),
            (BodyPosition(position), Velocity(velocity), settings),
            Contacts {
                faces,
                on_ground: has(ON_GROUND),
                ground_block,
                in_fluid: has(IN_FLUID),
            },
            PhysicsSync {
                last_written: Some(translation),
                still,
                placed: true,
                ..PhysicsSync::default()
            },
            Restored,
        ));
        if let Some(collider) = collider {
            e.insert(collider);
        }
        if has(INTERPOLATED) {
            e.insert(Interpolated { prev: previous });
        }
        if has(REPORT) {
            e.insert(ReportContacts);
        }
        if let Some((controller, intent)) = controller {
            e.insert((controller, intent));
        }
        if let Some(projectile) = projectile {
            e.insert(projectile);
        }
        if let Some(swimmer) = swimmer {
            e.insert(swimmer);
        }
        // Last: its hook reads the rest (the same change tick, so nothing counts as changed
        // after it fell asleep).
        if has(SLEEPING) {
            e.insert(Sleeping);
        }
        Ok(())
    }
}
