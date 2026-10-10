//! The demo's pack recipes (#48): one per demo texture layer, in layer order, and the states and
//! tints they name exist.

use boxel_demo::pack::{FOLIAGE_TINT, GRASS_TINT, WATER_TINT, recipes};

#[test]
fn every_demo_layer_has_one_recipe_in_layer_order() {
    let registry = boxel_demo::blocks::registry();
    let layers: Vec<&str> = registry
        .textures()
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    let recipes = recipes();
    let named: Vec<&str> = recipes.iter().map(|r| r.layer.as_str()).collect();
    assert_eq!(named, layers);
}

#[test]
fn recipe_states_and_tints_exist() {
    let registry = boxel_demo::blocks::registry();
    for recipe in recipes() {
        if !recipe.state.is_empty() {
            assert!(
                registry.parse_state(&recipe.state).is_ok(),
                "{}: no state {}",
                recipe.layer,
                recipe.state
            );
        }
        let tints = recipe
            .fallback
            .iter()
            .map(|(_, tint)| *tint)
            .chain([recipe.tint]);
        for tint in tints.flatten() {
            assert!(
                [GRASS_TINT, FOLIAGE_TINT, WATER_TINT].contains(&tint),
                "{}: {tint:?}",
                recipe.layer
            );
        }
        // Only the flow sprites take a quarter of their frame (ADR 0025).
        assert_eq!(
            recipe.quarter,
            recipe.layer.ends_with("_flow"),
            "{}",
            recipe.layer
        );
    }
}
