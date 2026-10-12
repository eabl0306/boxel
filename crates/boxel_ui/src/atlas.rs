//! The UI atlas: one RGBA8 sRGB page holding every sprite and glyph the UI draws.

use crate::sprite::{Scaling, Sprite};
use boxel_ecs::prelude::*;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// The page size [`UiAtlas::default`] starts with (menus and a HUD fit in it).
pub const DEFAULT_ATLAS_SIZE: u32 = 1024;
/// The largest page: the atlas doubles up to this size when something does not fit.
pub const MAX_ATLAS_SIZE: u32 = 4096;

/// A sprite in a [`UiAtlas`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpriteId(u32);

/// Why an image could not be added to the atlas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtlasError {
    /// `rgba` does not hold `w * h * 4` bytes.
    BadData {
        /// Width.
        w: u32,
        /// Height.
        h: u32,
        /// Bytes given.
        len: usize,
    },
    /// The page reached [`MAX_ATLAS_SIZE`] and the image still does not fit.
    Full {
        /// Width.
        w: u32,
        /// Height.
        h: u32,
    },
}

impl fmt::Display for AtlasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadData { w, h, len } => {
                write!(
                    f,
                    "a {w}x{h} RGBA image needs {} bytes, got {len}",
                    w * h * 4
                )
            }
            Self::Full { w, h } => write!(
                f,
                "the UI atlas is full: a {w}x{h} image does not fit in {MAX_ATLAS_SIZE}x{MAX_ATLAS_SIZE}"
            ),
        }
    }
}

impl std::error::Error for AtlasError {}

#[derive(Clone, Copy, Debug)]
struct Shelf {
    y: u32,
    h: u32,
    x: u32,
}

/// One square RGBA8 page in sRGB with straight alpha, packed with shelves. Texel (0, 0) is
/// opaque white, so a solid rectangle is a quad over it times a tint.
///
/// Images never move once placed: when the page is full it doubles (up to
/// [`MAX_ATLAS_SIZE`]) and keeps every texel where it was. `generation` changes with every
/// change of the pixels, so a renderer uploads the page only then. Sprites can be found by a
/// key (`game:heart_full`): the engine's skin uses none, a game registers its own.
#[derive(Resource, Clone, Debug)]
pub struct UiAtlas {
    size: u32,
    rgba: Arc<Vec<u8>>,
    generation: u64,
    shelves: Vec<Shelf>,
    sprites: Vec<Sprite>,
    keys: HashMap<String, SpriteId>,
}

impl Default for UiAtlas {
    fn default() -> Self {
        Self::new(DEFAULT_ATLAS_SIZE)
    }
}

impl UiAtlas {
    /// An empty page of `size` by `size` texels (clamped to 16..=[`MAX_ATLAS_SIZE`]) with its
    /// white texel at (0, 0).
    pub fn new(size: u32) -> Self {
        let size = size.clamp(16, MAX_ATLAS_SIZE);
        let mut atlas = Self {
            size,
            rgba: Arc::new(vec![0; (size * size * 4) as usize]),
            generation: 0,
            shelves: Vec::new(),
            sprites: Vec::new(),
            keys: HashMap::new(),
        };
        let white = atlas.add_image(1, 1, &[255; 4]);
        debug_assert_eq!(white, Ok([0, 0]));
        atlas
    }

    /// Width and height of the page in texels.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The page's texels, row by row, 4 bytes each.
    pub fn rgba(&self) -> &Arc<Vec<u8>> {
        &self.rgba
    }

    /// Changes whenever the pixels or the page size change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The texel at `(x, y)` (transparent black outside the page).
    pub fn texel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.size || y >= self.size {
            return [0; 4];
        }
        let i = ((y * self.size + x) * 4) as usize;
        self.rgba[i..i + 4].try_into().unwrap_or([0; 4])
    }

    /// Copies a `w` by `h` RGBA8 image into a free place and returns its top-left texel.
    pub fn add_image(&mut self, w: u32, h: u32, rgba: &[u8]) -> Result<[u32; 2], AtlasError> {
        if rgba.len() != (w as usize) * (h as usize) * 4 {
            return Err(AtlasError::BadData {
                w,
                h,
                len: rgba.len(),
            });
        }
        let at = self.place(w, h)?;
        if w > 0 && h > 0 {
            let page = Arc::make_mut(&mut self.rgba);
            let row = (w * 4) as usize;
            for y in 0..h {
                let dst = (((at[1] + y) * self.size + at[0]) * 4) as usize;
                let src = (y * w * 4) as usize;
                page[dst..dst + row].copy_from_slice(&rgba[src..src + row]);
            }
        }
        self.generation += 1;
        Ok(at)
    }

    /// Adds an image as a sprite drawn with `scaling`.
    pub fn add_sprite(
        &mut self,
        w: u32,
        h: u32,
        rgba: &[u8],
        scaling: Scaling,
    ) -> Result<SpriteId, AtlasError> {
        let at = self.add_image(w, h, rgba)?;
        let id = SpriteId(self.sprites.len() as u32);
        self.sprites.push(Sprite {
            src: [at[0], at[1], w, h],
            scaling,
        });
        Ok(id)
    }

    /// Adds a sprite under `key`. A key added again points to the new sprite (the old pixels
    /// stay in the page).
    pub fn insert_sprite(
        &mut self,
        key: &str,
        w: u32,
        h: u32,
        rgba: &[u8],
        scaling: Scaling,
    ) -> Result<SpriteId, AtlasError> {
        let id = self.add_sprite(w, h, rgba, scaling)?;
        self.keys.insert(key.to_owned(), id);
        Ok(id)
    }

    /// The sprite registered under `key`.
    pub fn find(&self, key: &str) -> Option<SpriteId> {
        self.keys.get(key).copied()
    }

    /// A sprite's rectangle and scaling.
    ///
    /// # Panics
    /// If `id` comes from another atlas with fewer sprites.
    pub fn sprite(&self, id: SpriteId) -> Sprite {
        self.sprites[id.0 as usize]
    }

    /// Finds room for `w` by `h` texels, growing the page when needed.
    fn place(&mut self, w: u32, h: u32) -> Result<[u32; 2], AtlasError> {
        loop {
            // The lowest shelf that is tall enough and has room, preferring the tightest fit.
            let best = self
                .shelves
                .iter_mut()
                .filter(|s| s.h >= h && s.x + w <= self.size && s.h <= h.max(1) * 2)
                .min_by_key(|s| s.h);
            if let Some(shelf) = best {
                let at = [shelf.x, shelf.y];
                shelf.x += w;
                return Ok(at);
            }
            let top = self.shelves.last().map_or(0, |s| s.y + s.h);
            if w <= self.size && top + h <= self.size {
                self.shelves.push(Shelf { y: top, h, x: w });
                return Ok([0, top]);
            }
            if self.size >= MAX_ATLAS_SIZE {
                return Err(AtlasError::Full { w, h });
            }
            self.grow();
        }
    }

    /// Doubles the page, keeping every texel where it was.
    fn grow(&mut self) {
        let old = self.size as usize;
        let size = self.size * 2;
        let mut page = vec![0; (size * size * 4) as usize];
        for y in 0..old {
            let src = y * old * 4;
            let dst = y * size as usize * 4;
            page[dst..dst + old * 4].copy_from_slice(&self.rgba[src..src + old * 4]);
        }
        self.size = size;
        self.rgba = Arc::new(page);
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, value: u8) -> Vec<u8> {
        vec![value; (w * h * 4) as usize]
    }

    #[test]
    fn white_texel_and_packing() {
        let mut atlas = UiAtlas::new(64);
        assert_eq!(atlas.texel(0, 0), [255; 4]);
        let a = atlas.add_image(10, 8, &image(10, 8, 7)).unwrap();
        let b = atlas.add_image(10, 8, &image(10, 8, 9)).unwrap();
        assert_eq!(a[1], b[1], "same shelf");
        assert_eq!(b[0], a[0] + 10);
        assert_eq!(atlas.texel(a[0] + 9, a[1] + 7), [7; 4]);
        assert_eq!(atlas.texel(b[0], b[1]), [9; 4]);
        assert_eq!(
            atlas.add_image(2, 2, &[0; 3]),
            Err(AtlasError::BadData { w: 2, h: 2, len: 3 })
        );
    }

    #[test]
    fn growing_keeps_texels_in_place() {
        let mut atlas = UiAtlas::new(16);
        let a = atlas.add_image(16, 10, &image(16, 10, 5)).unwrap();
        let generation = atlas.generation();
        let b = atlas.add_image(20, 20, &image(20, 20, 6)).unwrap();
        assert_eq!(atlas.size(), 32);
        assert!(atlas.generation() > generation);
        assert_eq!(atlas.texel(a[0] + 15, a[1] + 9), [5; 4]);
        assert_eq!(atlas.texel(b[0] + 19, b[1] + 19), [6; 4]);
        assert_eq!(atlas.texel(0, 0), [255; 4]);
        let too_big = MAX_ATLAS_SIZE + 1;
        assert_eq!(
            atlas.place(too_big, 1),
            Err(AtlasError::Full { w: too_big, h: 1 })
        );
    }

    #[test]
    fn keyed_sprites() {
        let mut atlas = UiAtlas::new(64);
        let id = atlas
            .insert_sprite("test:box", 4, 4, &image(4, 4, 1), Scaling::Stretch)
            .unwrap();
        assert_eq!(atlas.find("test:box"), Some(id));
        assert_eq!(atlas.find("test:other"), None);
        let sprite = atlas.sprite(id);
        assert_eq!([sprite.src[2], sprite.src[3]], [4, 4]);
    }
}
