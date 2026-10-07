//! Blockstate JSON: `variants` and `multipart`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::error::{PackError, PackWarning};
use crate::id::ResourceId;
use crate::overlay::FileIndex;

/// A block's blockstate definition.
#[derive(Debug, Clone)]
pub enum BlockState {
    /// Classic variant map keyed by property string (`facing=north,half=top` or `""`).
    Variants(BTreeMap<String, VariantList>),
    /// Multipart rules.
    Multipart(Vec<MultipartCase>),
}

/// One or more weighted model choices for a variant / multipart apply.
#[derive(Debug, Clone)]
pub struct VariantList(pub Vec<VariantModel>);

/// A single model placement.
#[derive(Debug, Clone)]
pub struct VariantModel {
    /// Model resource id.
    pub model: ResourceId,
    /// Rotation around X in degrees.
    pub x: i32,
    /// Rotation around Y in degrees.
    pub y: i32,
    /// UV lock.
    pub uvlock: bool,
    /// Random weight (default 1).
    pub weight: u32,
}

/// One multipart rule.
#[derive(Debug, Clone)]
pub struct MultipartCase {
    /// Optional condition; `None` means always apply.
    pub when: Option<Condition>,
    /// Models to apply when the condition matches.
    pub apply: VariantList,
}

/// Multipart condition tree.
#[derive(Debug, Clone)]
pub enum Condition {
    /// Logical OR of nested conditions.
    Or(Vec<Condition>),
    /// Logical AND of nested conditions.
    And(Vec<Condition>),
    /// Property match: each key must match one of the `|`-separated values.
    Match(BTreeMap<String, Vec<String>>),
}

/// Parses a variants key (`facing=north,half=top`) into property map.
pub fn parse_variant_key(key: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if key.is_empty() {
        return out;
    }
    for part in key.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        } else {
            out.insert(part.to_string(), String::new());
        }
    }
    out
}

/// Whether `state` satisfies `condition` (for multipart evaluation helpers / tests).
pub fn condition_matches(condition: &Condition, state: &BTreeMap<String, String>) -> bool {
    match condition {
        Condition::Or(xs) => xs.iter().any(|c| condition_matches(c, state)),
        Condition::And(xs) => xs.iter().all(|c| condition_matches(c, state)),
        Condition::Match(props) => props.iter().all(|(k, allowed)| {
            state
                .get(k)
                .is_some_and(|got| allowed.iter().any(|a| a == got))
        }),
    }
}

/// Loads all blockstates from the index.
pub fn load_blockstates(
    index: &FileIndex,
    warnings: &mut Vec<PackWarning>,
) -> Result<BTreeMap<ResourceId, BlockState>, PackError> {
    let mut out = BTreeMap::new();
    let mut rels: Vec<&String> = index
        .files
        .keys()
        .filter(|r| is_blockstate_json(r))
        .collect();
    rels.sort();
    for rel in rels {
        let path = &index.files[rel];
        let Some(id) = resource_id_from_blockstate_rel(rel) else {
            warnings.push(PackWarning::new(
                "blockstate",
                format!("could not parse blockstate id from {rel}"),
            ));
            continue;
        };
        let bs = load_one(path)?;
        out.insert(id, bs);
    }
    Ok(out)
}

fn is_blockstate_json(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    if !lower.ends_with(".json") {
        return false;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    parts.len() == 4 && parts[0] == "assets" && parts[2] == "blockstates"
}

fn resource_id_from_blockstate_rel(rel: &str) -> Option<ResourceId> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() != 4 || parts[0] != "assets" || parts[2] != "blockstates" {
        return None;
    }
    let ns = parts[1];
    let name = parts[3].strip_suffix(".json").unwrap_or(parts[3]);
    Some(ResourceId::new(ns, name))
}

#[derive(Debug, Deserialize)]
struct BlockStateFile {
    #[serde(default)]
    variants: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    multipart: Option<Vec<MultipartFile>>,
}

#[derive(Debug, Deserialize)]
struct MultipartFile {
    #[serde(default)]
    when: Option<Value>,
    apply: Value,
}

#[derive(Debug, Deserialize)]
struct VariantFile {
    model: String,
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
    #[serde(default)]
    uvlock: bool,
    #[serde(default = "default_weight")]
    weight: u32,
}

fn default_weight() -> u32 {
    1
}

fn load_one(path: &Path) -> Result<BlockState, PackError> {
    let text = std::fs::read_to_string(path).map_err(|source| PackError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let file: BlockStateFile =
        serde_json::from_str(&text).map_err(|source| PackError::InvalidJson {
            path: path.to_path_buf(),
            source,
        })?;
    if let Some(multipart) = file.multipart {
        let mut cases = Vec::with_capacity(multipart.len());
        for m in multipart {
            let when =
                match m.when {
                    None => None,
                    Some(v) => Some(parse_condition(v).map_err(|message| {
                        PackError::InvalidBlockstate {
                            path: path.to_path_buf(),
                            message,
                        }
                    })?),
                };
            let apply = parse_variant_list(&m.apply).map_err(|source| PackError::InvalidJson {
                path: path.to_path_buf(),
                source,
            })?;
            cases.push(MultipartCase { when, apply });
        }
        return Ok(BlockState::Multipart(cases));
    }
    if let Some(variants) = file.variants {
        let mut map = BTreeMap::new();
        for (k, v) in variants {
            let list = parse_variant_list(&v).map_err(|source| PackError::InvalidJson {
                path: path.to_path_buf(),
                source,
            })?;
            map.insert(k, list);
        }
        return Ok(BlockState::Variants(map));
    }
    Ok(BlockState::Variants(BTreeMap::new()))
}

fn parse_variant_list(v: &Value) -> Result<VariantList, serde_json::Error> {
    if v.is_array() {
        let items: Vec<VariantFile> = serde_json::from_value(v.clone())?;
        Ok(VariantList(
            items.into_iter().map(variant_from_file).collect(),
        ))
    } else {
        let item: VariantFile = serde_json::from_value(v.clone())?;
        Ok(VariantList(vec![variant_from_file(item)]))
    }
}

fn variant_from_file(f: VariantFile) -> VariantModel {
    VariantModel {
        model: ResourceId::parse(&f.model),
        x: f.x,
        y: f.y,
        uvlock: f.uvlock,
        weight: f.weight,
    }
}

fn parse_condition(v: Value) -> Result<Condition, String> {
    match v {
        Value::Object(map) => {
            if let Some(or) = map.get("OR") {
                let arr = or
                    .as_array()
                    .ok_or_else(|| "OR must be an array".to_string())?;
                let mut out = Vec::new();
                for item in arr {
                    out.push(parse_condition(item.clone())?);
                }
                return Ok(Condition::Or(out));
            }
            if let Some(and) = map.get("AND") {
                let arr = and
                    .as_array()
                    .ok_or_else(|| "AND must be an array".to_string())?;
                let mut out = Vec::new();
                for item in arr {
                    out.push(parse_condition(item.clone())?);
                }
                return Ok(Condition::And(out));
            }
            let mut props = BTreeMap::new();
            for (k, val) in map {
                let s = match val {
                    Value::String(s) => s,
                    Value::Bool(b) => b.to_string(),
                    Value::Number(n) => n.to_string(),
                    other => other.to_string(),
                };
                let options = s
                    .split('|')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect();
                props.insert(k, options);
            }
            Ok(Condition::Match(props))
        }
        _ => Err("condition must be a JSON object".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_key_parsing() {
        let m = parse_variant_key("facing=north,half=top");
        assert_eq!(m.get("facing").map(String::as_str), Some("north"));
        assert_eq!(m.get("half").map(String::as_str), Some("top"));
        assert!(parse_variant_key("").is_empty());
    }

    #[test]
    fn multipart_or_and_pipe() {
        let cond = Condition::Or(vec![
            Condition::Match(BTreeMap::from([(
                "east".into(),
                vec!["true".into(), "false".into()],
            )])),
            Condition::And(vec![Condition::Match(BTreeMap::from([(
                "west".into(),
                vec!["true".into()],
            )]))]),
        ]);
        let state = BTreeMap::from([("east".into(), "false".into())]);
        assert!(condition_matches(&cond, &state));
        let state2 = BTreeMap::from([("west".into(), "true".into())]);
        assert!(condition_matches(&cond, &state2));
        let state3 = BTreeMap::from([("east".into(), "maybe".into())]);
        assert!(!condition_matches(&cond, &state3));
    }
}
