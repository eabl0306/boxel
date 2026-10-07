//! Block model JSON loading, parent inheritance, and `#texture` variable resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::builtin_models;
use crate::error::{PackError, PackWarning};
use crate::id::ResourceId;
use crate::overlay::FileIndex;

/// Fully resolved block model (inheritance flattened, texture vars concrete).
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// Ambient occlusion flag after parent merge.
    pub ambient_occlusion: bool,
    /// Texture variables resolved to resource ids (no `#` refs left).
    pub textures: BTreeMap<String, ResourceId>,
    /// Geometry elements after parent merge.
    pub elements: Vec<ModelElement>,
}

/// One cuboid in a block model.
#[derive(Debug, Clone)]
pub struct ModelElement {
    /// Min corner in model space (0..16).
    pub from: Vec3,
    /// Max corner in model space (0..16).
    pub to: Vec3,
    /// Optional rotation around an origin.
    pub rotation: Option<ModelRotation>,
    /// Whether the element receives shade.
    pub shade: bool,
    /// Faces keyed by direction.
    pub faces: BTreeMap<FaceName, ModelFace>,
}

/// Element rotation.
#[derive(Debug, Clone)]
pub struct ModelRotation {
    /// Rotation origin.
    pub origin: Vec3,
    /// Axis.
    pub axis: Axis,
    /// Angle in degrees (−45/−22.5/0/22.5/45 in vanilla).
    pub angle: f32,
    /// Whether to rescale after rotation (used by cross models).
    pub rescale: bool,
}

/// Axis of rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// X axis.
    X,
    /// Y axis.
    Y,
    /// Z axis.
    Z,
}

/// Named face direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FaceName {
    /// −Y
    Down,
    /// +Y
    Up,
    /// −Z
    North,
    /// +Z
    South,
    /// −X
    West,
    /// +X
    East,
}

/// One face of an element.
#[derive(Debug, Clone)]
pub struct ModelFace {
    /// Optional UV quad `[u1, v1, u2, v2]`.
    pub uv: Option<[f32; 4]>,
    /// Texture reference: either `#var` (pre-resolve) or a concrete path string.
    pub texture: String,
    /// Optional cull face.
    pub cullface: Option<FaceName>,
    /// UV rotation in degrees (0/90/180/270).
    pub rotation: i32,
    /// Biome tint index.
    pub tintindex: Option<i32>,
}

/// Three floats.
#[derive(Debug, Clone, Copy)]
pub struct Vec3(pub [f32; 3]);

/// Unresolved model as stored on disk / builtins.
#[derive(Debug, Clone, Default)]
pub struct RawModel {
    /// Parent model id.
    pub parent: Option<ResourceId>,
    /// Ambient occlusion override.
    pub ambient_occlusion: Option<bool>,
    /// Texture variables (`all` → `minecraft:block/stone` or `#side`).
    pub textures: BTreeMap<String, String>,
    /// Elements; `None` means inherit from parent.
    pub elements: Option<Vec<ModelElement>>,
}

#[derive(Debug, Deserialize)]
struct ModelFile {
    #[serde(default)]
    parent: Option<String>,
    #[serde(default, rename = "ambientocclusion")]
    ambientocclusion: Option<bool>,
    #[serde(default)]
    textures: BTreeMap<String, String>,
    #[serde(default)]
    elements: Option<Vec<ElementFile>>,
    /// Parsed and discarded: item display transforms.
    #[serde(default)]
    #[expect(
        dead_code,
        reason = "accepted so unknown display JSON does not fail parse"
    )]
    display: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ElementFile {
    from: [f32; 3],
    to: [f32; 3],
    #[serde(default)]
    rotation: Option<RotationFile>,
    #[serde(default = "default_true")]
    shade: bool,
    #[serde(default)]
    faces: BTreeMap<FaceName, FaceFile>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct RotationFile {
    origin: [f32; 3],
    axis: String,
    angle: f32,
    #[serde(default)]
    rescale: bool,
}

#[derive(Debug, Deserialize)]
struct FaceFile {
    #[serde(default)]
    uv: Option<[f32; 4]>,
    texture: String,
    #[serde(default)]
    cullface: Option<FaceName>,
    #[serde(default)]
    rotation: i32,
    #[serde(default)]
    tintindex: Option<i32>,
}

impl From<ModelFile> for RawModel {
    fn from(f: ModelFile) -> Self {
        Self {
            parent: f.parent.map(|p| ResourceId::parse(&p)),
            ambient_occlusion: f.ambientocclusion,
            textures: f.textures,
            elements: f.elements.map(|els| {
                els.into_iter()
                    .map(|e| ModelElement {
                        from: Vec3(e.from),
                        to: Vec3(e.to),
                        rotation: e.rotation.map(|r| ModelRotation {
                            origin: Vec3(r.origin),
                            axis: parse_axis(&r.axis),
                            angle: r.angle,
                            rescale: r.rescale,
                        }),
                        shade: e.shade,
                        faces: e
                            .faces
                            .into_iter()
                            .map(|(k, v)| {
                                (
                                    k,
                                    ModelFace {
                                        uv: v.uv,
                                        texture: v.texture,
                                        cullface: v.cullface,
                                        rotation: v.rotation,
                                        tintindex: v.tintindex,
                                    },
                                )
                            })
                            .collect(),
                    })
                    .collect()
            }),
        }
    }
}

fn parse_axis(s: &str) -> Axis {
    match s.to_ascii_lowercase().as_str() {
        "x" => Axis::X,
        "z" => Axis::Z,
        _ => Axis::Y,
    }
}

/// Loads all block models from the file index (unresolved).
pub fn load_raw_models(
    index: &FileIndex,
    warnings: &mut Vec<PackWarning>,
) -> Result<BTreeMap<ResourceId, RawModel>, PackError> {
    let mut out = BTreeMap::new();
    let mut rels: Vec<&String> = index
        .files
        .keys()
        .filter(|r| is_block_model_json(r))
        .collect();
    rels.sort();
    for rel in rels {
        let path = &index.files[rel];
        let Some(id) = resource_id_from_model_rel(rel) else {
            warnings.push(PackWarning::new(
                "model",
                format!("could not parse model id from {rel}"),
            ));
            continue;
        };
        let raw = load_raw_model(path)?;
        out.insert(id, raw);
    }
    Ok(out)
}

fn is_block_model_json(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    if !lower.ends_with(".json") {
        return false;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    parts.len() >= 5 && parts[0] == "assets" && parts[2] == "models" && parts[3] == "block"
}

fn resource_id_from_model_rel(rel: &str) -> Option<ResourceId> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() < 5 || parts[0] != "assets" || parts[2] != "models" {
        return None;
    }
    let ns = parts[1];
    let mut path = parts[3..].join("/");
    if let Some(s) = path.strip_suffix(".json") {
        path = s.to_string();
    }
    Some(ResourceId::new(ns, path))
}

fn load_raw_model(path: &Path) -> Result<RawModel, PackError> {
    let text = std::fs::read_to_string(path).map_err(|source| PackError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let file: ModelFile = serde_json::from_str(&text).map_err(|source| PackError::InvalidJson {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(file.into())
}

/// Looks up a raw model from the pack map or builtins.
pub fn lookup_raw(
    id: &ResourceId,
    pack_models: &BTreeMap<ResourceId, RawModel>,
) -> Option<RawModel> {
    if let Some(m) = pack_models.get(id) {
        return Some(m.clone());
    }
    builtin_models::builtin(id)
}

/// Resolves inheritance and texture variables for one model.
pub fn resolve_model(
    id: &ResourceId,
    pack_models: &BTreeMap<ResourceId, RawModel>,
) -> Result<ResolvedModel, PackError> {
    // Walk the parent chain: every id must exist (pack or builtin), and there must be no cycle.
    {
        let mut stack = BTreeSet::new();
        let mut cur = Some(id.clone());
        while let Some(c) = cur {
            if !stack.insert(c.clone()) {
                return Err(PackError::ModelParentCycle(c));
            }
            let raw = lookup_raw(&c, pack_models).ok_or(PackError::MissingModel(c))?;
            cur = raw.parent;
        }
    }

    let tex_vars = collect_texture_vars(id, pack_models, &mut BTreeSet::new())?;
    let resolved_textures = resolve_texture_map(id, &tex_vars)?;
    let ambient_occlusion = collect_ao(id, pack_models, &mut BTreeSet::new())?.unwrap_or(true);
    let elements = collect_elements(id, pack_models, &mut BTreeSet::new())?.unwrap_or_default();

    let elements = elements
        .into_iter()
        .map(|mut el| {
            for face in el.faces.values_mut() {
                if let Some(name) = face.texture.strip_prefix('#') {
                    if let Some(rid) = resolved_textures.get(name) {
                        face.texture = rid.to_string();
                    }
                } else {
                    face.texture = ResourceId::parse(&face.texture).to_string();
                }
            }
            el
        })
        .collect();

    Ok(ResolvedModel {
        ambient_occlusion,
        textures: resolved_textures,
        elements,
    })
}

fn collect_texture_vars(
    id: &ResourceId,
    pack_models: &BTreeMap<ResourceId, RawModel>,
    stack: &mut BTreeSet<ResourceId>,
) -> Result<BTreeMap<String, String>, PackError> {
    if !stack.insert(id.clone()) {
        return Err(PackError::ModelParentCycle(id.clone()));
    }
    let Some(raw) = lookup_raw(id, pack_models) else {
        stack.remove(id);
        return Ok(BTreeMap::new());
    };
    let mut map = if let Some(ref parent) = raw.parent {
        collect_texture_vars(parent, pack_models, stack)?
    } else {
        BTreeMap::new()
    };
    for (k, v) in raw.textures {
        map.insert(k, v);
    }
    stack.remove(id);
    Ok(map)
}

fn collect_ao(
    id: &ResourceId,
    pack_models: &BTreeMap<ResourceId, RawModel>,
    stack: &mut BTreeSet<ResourceId>,
) -> Result<Option<bool>, PackError> {
    if !stack.insert(id.clone()) {
        return Err(PackError::ModelParentCycle(id.clone()));
    }
    let Some(raw) = lookup_raw(id, pack_models) else {
        stack.remove(id);
        return Ok(None);
    };
    let ao = if let Some(ao) = raw.ambient_occlusion {
        Some(ao)
    } else if let Some(ref parent) = raw.parent {
        collect_ao(parent, pack_models, stack)?
    } else {
        None
    };
    stack.remove(id);
    Ok(ao)
}

fn collect_elements(
    id: &ResourceId,
    pack_models: &BTreeMap<ResourceId, RawModel>,
    stack: &mut BTreeSet<ResourceId>,
) -> Result<Option<Vec<ModelElement>>, PackError> {
    if !stack.insert(id.clone()) {
        return Err(PackError::ModelParentCycle(id.clone()));
    }
    let Some(raw) = lookup_raw(id, pack_models) else {
        stack.remove(id);
        return Ok(None);
    };
    let els = if let Some(els) = raw.elements {
        Some(els)
    } else if let Some(ref parent) = raw.parent {
        collect_elements(parent, pack_models, stack)?
    } else {
        None
    };
    stack.remove(id);
    Ok(els)
}

fn resolve_texture_map(
    model: &ResourceId,
    vars: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, ResourceId>, PackError> {
    let mut out = BTreeMap::new();
    for key in vars.keys() {
        let rid = resolve_var(model, vars, key, &mut BTreeSet::new())?;
        out.insert(key.clone(), rid);
    }
    Ok(out)
}

fn resolve_var(
    model: &ResourceId,
    vars: &BTreeMap<String, String>,
    key: &str,
    stack: &mut BTreeSet<String>,
) -> Result<ResourceId, PackError> {
    if !stack.insert(key.to_string()) {
        return Err(PackError::UndefinedTextureVar {
            model: model.clone(),
            name: format!("{key} (cycle)"),
        });
    }
    let value = vars
        .get(key)
        .ok_or_else(|| PackError::UndefinedTextureVar {
            model: model.clone(),
            name: key.to_string(),
        })?;
    let result = if let Some(next) = value.strip_prefix('#') {
        resolve_var(model, vars, next, stack)?
    } else {
        ResourceId::parse(value)
    };
    stack.remove(key);
    Ok(result)
}

/// Resolves every pack model; failures become warnings and the model is omitted.
pub fn resolve_all(
    pack_models: &BTreeMap<ResourceId, RawModel>,
    warnings: &mut Vec<PackWarning>,
) -> BTreeMap<ResourceId, ResolvedModel> {
    let mut out = BTreeMap::new();
    for id in pack_models.keys() {
        match resolve_model(id, pack_models) {
            Ok(resolved) => {
                out.insert(id.clone(), resolved);
            }
            Err(e) => {
                warnings.push(PackWarning::new(
                    "model_resolve",
                    format!("skipped {id}: {e}"),
                ));
            }
        }
    }
    out
}
