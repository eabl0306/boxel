//! The procedural painter of the demo layers (`boxel_demo::textures`), moved from the renderer's
//! unit tests (#48): level 0 of each pattern. The mips are the renderer's and its tests check
//! them.

use boxel_demo::textures::{Pattern, SIZE, paint, spec};
use boxel_voxel::TextureImage;

/// The base color and noise of the layers under test.
const BASE: [u8; 3] = [120, 90, 60];

/// The layer `name` with `pattern` over [`BASE`] and noise 16.
fn layer(name: &str, pattern: Pattern) -> TextureImage {
    paint(name, BASE, 16, pattern)
}

#[test]
fn generation_is_deterministic_per_name() {
    let first = [
        layer("test:a", Pattern::Noise),
        layer("test:b", Pattern::Noise),
    ];
    assert_eq!(
        first,
        [
            layer("test:a", Pattern::Noise),
            layer("test:b", Pattern::Noise)
        ]
    );
    // a layer depends on its name, base color, noise and pattern only
    assert_eq!(spec("test:a", BASE, 16, Pattern::Noise).image, first[0]);
    assert_ne!(first[0], first[1]);
}

#[test]
fn patterns_follow_the_spec() {
    let band = paint(
        "test:band",
        BASE,
        0,
        Pattern::TopBand {
            color: [10, 200, 10],
            rows: 4,
        },
    );
    for (i, t) in band.texels.iter().enumerate() {
        let expected = if i / 16 < 4 {
            [10, 200, 10, 255]
        } else {
            [120, 90, 60, 255]
        };
        assert_eq!(*t, expected, "texel {i}");
    }

    let speckle = paint(
        "test:speckle",
        BASE,
        0,
        Pattern::Speckle {
            color: [0, 0, 255],
            density: 40,
        },
    );
    let blue = speckle
        .texels
        .iter()
        .filter(|t| **t == [0, 0, 255, 255])
        .count();
    assert_eq!(blue, 40);

    let planks = paint("test:planks", BASE, 0, Pattern::Planks);
    let texels = &planks.texels;
    assert_eq!(texels[3 * 16], [84, 63, 42, 255]);
    assert_eq!(texels[2 * 16], [120, 90, 60, 255]);

    // a flow texture: streaks that repeat every 8 rows, lighter and darker than the base
    let flow = paint("test:flow", BASE, 0, Pattern::Flow { alpha: 160 });
    let texels = &flow.texels;
    for (i, t) in texels.iter().enumerate() {
        assert_eq!(t[3], 160);
        assert_eq!(*t, texels[(i + 8 * 16) % 256], "texel {i}");
    }
    for x in 0..16 {
        let column: Vec<u8> = (0..16).map(|y| texels[y * 16 + x][0]).collect();
        assert!(column.contains(&150) && column.contains(&98) && column.contains(&120));
    }

    // noise stays within the amplitude
    let noisy = layer("test:noise", Pattern::Noise);
    for t in noisy.texels.iter() {
        assert!(t[0].abs_diff(120) <= 16 && t[1].abs_diff(90) <= 16 && t[2].abs_diff(60) <= 16);
    }
}

/// The cutout patterns, and only they, paint images with binary alpha (the renderer keeps their
/// mips binary); every image is a valid `SIZE`^2 one.
#[test]
fn cutout_patterns_have_binary_alpha() {
    let patterns = [
        (Pattern::Noise, false),
        (
            Pattern::TopBand {
                color: [0; 3],
                rows: 4,
            },
            false,
        ),
        (
            Pattern::Speckle {
                color: [0; 3],
                density: 40,
            },
            false,
        ),
        (Pattern::Leaves, true),
        (Pattern::Planks, false),
        (Pattern::Bark, false),
        (Pattern::Glass, false),
        (Pattern::Liquid { alpha: 160 }, false),
        (Pattern::Plant, true),
        (Pattern::Flower { color: [0; 3] }, true),
        (Pattern::Torch, true),
        (Pattern::Cobble, false),
        (Pattern::Flow { alpha: 160 }, false),
    ];
    for (pattern, binary) in patterns {
        let image = layer("test:cutout", pattern);
        assert!(image.is_valid() && image.size == SIZE, "{pattern:?}");
        assert_eq!(image.binary_alpha, binary, "{pattern:?}");
        if binary {
            assert!(image.texels.iter().all(|t| t[3] == 0 || t[3] == 255));
        }
    }
}
