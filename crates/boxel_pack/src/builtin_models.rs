//! Built-in vanilla block model parents used when a pack does not ship them.

use std::collections::BTreeMap;

use crate::id::ResourceId;
use crate::model::{Axis, FaceName, ModelElement, ModelFace, ModelRotation, RawModel, Vec3};

/// Returns a built-in raw model for well-known parents, if any.
pub fn builtin(id: &ResourceId) -> Option<RawModel> {
    if id.namespace != "minecraft" {
        return None;
    }
    match id.path.as_str() {
        "block/block" => Some(RawModel {
            parent: None,
            ambient_occlusion: Some(true),
            textures: BTreeMap::new(),
            elements: None,
        }),
        "block/cube" => Some(cube()),
        "block/cube_all" => Some(RawModel {
            parent: Some(ResourceId::parse("minecraft:block/cube")),
            ambient_occlusion: None,
            textures: [
                ("particle", "#all"),
                ("down", "#all"),
                ("up", "#all"),
                ("north", "#all"),
                ("south", "#all"),
                ("west", "#all"),
                ("east", "#all"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            elements: None,
        }),
        "block/cube_column" => Some(RawModel {
            parent: Some(ResourceId::parse("minecraft:block/cube")),
            ambient_occlusion: None,
            textures: [
                ("particle", "#side"),
                ("down", "#end"),
                ("up", "#end"),
                ("north", "#side"),
                ("south", "#side"),
                ("west", "#side"),
                ("east", "#side"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            elements: None,
        }),
        "block/cube_bottom_top" => Some(RawModel {
            parent: Some(ResourceId::parse("minecraft:block/cube")),
            ambient_occlusion: None,
            textures: [
                ("particle", "#side"),
                ("down", "#bottom"),
                ("up", "#top"),
                ("north", "#side"),
                ("south", "#side"),
                ("west", "#side"),
                ("east", "#side"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            elements: None,
        }),
        "block/orientable" => Some(RawModel {
            parent: Some(ResourceId::parse("minecraft:block/orientable_with_bottom")),
            ambient_occlusion: None,
            textures: [("bottom", "#top")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            elements: None,
        }),
        "block/orientable_with_bottom" => Some(RawModel {
            parent: Some(ResourceId::parse("minecraft:block/cube")),
            ambient_occlusion: None,
            textures: [
                ("particle", "#front"),
                ("down", "#bottom"),
                ("up", "#top"),
                ("north", "#front"),
                ("south", "#side"),
                ("west", "#side"),
                ("east", "#side"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            elements: None,
        }),
        "block/cross" => Some(cross()),
        _ => None,
    }
}

fn cube() -> RawModel {
    let faces = |tex: &str, cull: FaceName| ModelFace {
        uv: None,
        texture: format!("#{tex}"),
        cullface: Some(cull),
        rotation: 0,
        tintindex: None,
    };
    RawModel {
        parent: Some(ResourceId::parse("minecraft:block/block")),
        ambient_occlusion: None,
        textures: BTreeMap::new(),
        elements: Some(vec![ModelElement {
            from: Vec3([0.0, 0.0, 0.0]),
            to: Vec3([16.0, 16.0, 16.0]),
            rotation: None,
            shade: true,
            faces: [
                (FaceName::Down, faces("down", FaceName::Down)),
                (FaceName::Up, faces("up", FaceName::Up)),
                (FaceName::North, faces("north", FaceName::North)),
                (FaceName::South, faces("south", FaceName::South)),
                (FaceName::West, faces("west", FaceName::West)),
                (FaceName::East, faces("east", FaceName::East)),
            ]
            .into_iter()
            .collect(),
        }]),
    }
}

fn cross() -> RawModel {
    let face = |tex: &str| ModelFace {
        uv: None,
        texture: format!("#{tex}"),
        cullface: None,
        rotation: 0,
        tintindex: None,
    };
    let mut faces_a = BTreeMap::new();
    faces_a.insert(FaceName::North, face("cross"));
    faces_a.insert(FaceName::South, face("cross"));
    let mut faces_b = BTreeMap::new();
    faces_b.insert(FaceName::West, face("cross"));
    faces_b.insert(FaceName::East, face("cross"));
    RawModel {
        parent: Some(ResourceId::parse("minecraft:block/block")),
        ambient_occlusion: Some(false),
        textures: BTreeMap::new(),
        elements: Some(vec![
            ModelElement {
                from: Vec3([0.8, 0.0, 8.0]),
                to: Vec3([15.2, 16.0, 8.0]),
                rotation: Some(ModelRotation {
                    origin: Vec3([8.0, 8.0, 8.0]),
                    axis: Axis::Y,
                    angle: 45.0,
                    rescale: true,
                }),
                shade: true,
                faces: faces_a,
            },
            ModelElement {
                from: Vec3([8.0, 0.0, 0.8]),
                to: Vec3([8.0, 16.0, 15.2]),
                rotation: Some(ModelRotation {
                    origin: Vec3([8.0, 8.0, 8.0]),
                    axis: Axis::Y,
                    angle: 45.0,
                    rescale: true,
                }),
                shade: true,
                faces: faces_b,
            },
        ]),
    }
}
