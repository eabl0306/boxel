//! Texture animation metadata (`.png.mcmeta`).

use serde::Deserialize;

/// One frame of an animated texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationFrame {
    /// Index into the vertical strip (0-based).
    pub index: u32,
    /// Duration in game ticks. `None` means use [`Animation::frametime`].
    pub time: Option<u32>,
}

/// Animation description attached to a [`crate::texture::Texture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Animation {
    /// Default frame duration in ticks.
    pub frametime: u32,
    /// Whether the game should interpolate between frames.
    pub interpolate: bool,
    /// Frame sequence. Empty means play every strip frame in order.
    pub frames: Vec<AnimationFrame>,
    /// Optional frame width override from mcmeta.
    pub width: Option<u32>,
    /// Optional frame height override from mcmeta.
    pub height: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct McMetaTextureFile {
    #[serde(default)]
    pub animation: Option<AnimationRaw>,
    // `texture: { mipmap_strategy: ... }` and friends are ignored.
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnimationRaw {
    #[serde(default = "default_frametime")]
    pub frametime: u32,
    #[serde(default)]
    pub interpolate: bool,
    #[serde(default)]
    pub frames: Vec<FrameRaw>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

fn default_frametime() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum FrameRaw {
    Index(u32),
    Object {
        index: u32,
        #[serde(default)]
        time: Option<u32>,
    },
}

impl AnimationRaw {
    pub(crate) fn into_animation(self) -> Animation {
        let frames = self
            .frames
            .into_iter()
            .map(|f| match f {
                FrameRaw::Index(index) => AnimationFrame { index, time: None },
                FrameRaw::Object { index, time } => AnimationFrame { index, time },
            })
            .collect();
        Animation {
            frametime: self.frametime,
            interpolate: self.interpolate,
            frames,
            width: self.width,
            height: self.height,
        }
    }
}

/// Splits a vertical animation strip into individual RGBA frames.
///
/// Frame size is `anim.width`×`anim.height` when set, otherwise `width`×`width` (square frames
/// stacked vertically). Returns an empty vec when the strip cannot be divided evenly.
pub fn split_frames(
    width: u32,
    height: u32,
    rgba: &[u8],
    anim: &Animation,
) -> Result<Vec<Vec<u8>>, String> {
    let fw = anim.width.unwrap_or(width);
    let fh = anim.height.unwrap_or(width);
    if fw == 0 || fh == 0 {
        return Err("animation frame size is zero".into());
    }
    if !width.is_multiple_of(fw) {
        return Err(format!(
            "texture width {width} is not a multiple of frame width {fw}"
        ));
    }
    if !height.is_multiple_of(fh) {
        return Err(format!(
            "texture height {height} is not a multiple of frame height {fh}"
        ));
    }
    let cols = width / fw;
    let rows = height / fh;
    let frame_count = cols * rows;
    let mut out = Vec::with_capacity(frame_count as usize);
    for row in 0..rows {
        for col in 0..cols {
            let mut frame = vec![0u8; (fw * fh * 4) as usize];
            for y in 0..fh {
                let src_y = row * fh + y;
                let src_off = ((src_y * width + col * fw) * 4) as usize;
                let dst_off = (y * fw * 4) as usize;
                let len = (fw * 4) as usize;
                frame[dst_off..dst_off + len].copy_from_slice(&rgba[src_off..src_off + len]);
            }
            out.push(frame);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_vertical_strip() {
        // 2 frames of 2x2 RGBA stacked → 2x4 image
        let mut rgba = Vec::new();
        // frame 0: red
        for _ in 0..4 {
            rgba.extend_from_slice(&[255, 0, 0, 255]);
        }
        // frame 1: blue
        for _ in 0..4 {
            rgba.extend_from_slice(&[0, 0, 255, 255]);
        }
        let anim = Animation {
            frametime: 1,
            interpolate: false,
            frames: vec![],
            width: None,
            height: None,
        };
        let frames = split_frames(2, 4, &rgba, &anim).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(&frames[0][..4], &[255, 0, 0, 255]);
        assert_eq!(&frames[1][..4], &[0, 0, 255, 255]);
    }
}
