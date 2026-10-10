//! Texture layers are images (#48): the registry takes a layer whose image is a power of two on
//! a side, filled exactly, and as big as the layers before it.
use boxel_voxel::*;
use std::sync::Arc;

fn layer(name: &str, image: TextureImage) -> TextureSpec {
    TextureSpec {
        name: name.into(),
        image,
    }
}

#[test]
fn texture_sizes_are_checked() {
    let mut b = BlockRegistryBuilder::new();
    let gray = [128, 128, 128, 255];
    assert_eq!(
        b.add_texture(layer("test:a", TextureImage::solid(32, gray))),
        Ok(TextureId(0))
    );
    // not as big as the first layer
    assert_eq!(
        b.add_texture(layer("test:b", TextureImage::solid(16, gray))),
        Err(RegistryError::TextureSize("test:b".into()))
    );
    // not a power of two
    let mut odd = TextureImage::solid(24, gray);
    assert!(!odd.is_valid());
    assert_eq!(
        b.add_texture(layer("test:c", odd.clone())),
        Err(RegistryError::TextureSize("test:c".into()))
    );
    // texels that do not fill the edge
    odd = TextureImage {
        size: 32,
        texels: Arc::from(vec![gray; 32 * 31]),
        binary_alpha: false,
    };
    assert_eq!(
        b.add_texture(layer("test:d", odd)),
        Err(RegistryError::TextureSize("test:d".into()))
    );
    assert_eq!(
        b.add_texture(layer("test:e", TextureImage::solid(0, gray))),
        Err(RegistryError::TextureSize("test:e".into()))
    );
    let cutout = TextureImage {
        binary_alpha: true,
        ..TextureImage::solid(32, [0; 4])
    };
    assert_eq!(
        b.add_texture(layer("test:f", cutout.clone())),
        Ok(TextureId(1))
    );
    let r = b.build();
    assert_eq!(r.textures().len(), 2);
    assert_eq!(r.textures()[1].image, cutout);
    assert_eq!(r.textures()[0].image.texels.len(), 32 * 32);
}
