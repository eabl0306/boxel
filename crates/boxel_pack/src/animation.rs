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

impl Animation {
    /// Frame size of a `width` x `height` strip, Minecraft's rule: both mcmeta sizes when set,
    /// one of them with the image's other side, else squares of the shorter side.
    pub fn frame_size(&self, width: u32, height: u32) -> (u32, u32) {
        match (self.width, self.height) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, height),
            (None, Some(h)) => (width, h),
            (None, None) => (width.min(height), width.min(height)),
        }
    }

    /// The frame sequence over a strip of `frame_count` frames, by Minecraft's rules
    /// (`SpriteContents.createAnimatedTexture`): no `frames` list plays every strip frame in
    /// order for `frametime` ticks; a list keeps its entries (a bare index or a frame without
    /// `time` lasts `frametime`) and drops those with a time under 1 tick or an index outside
    /// the strip, each with a warning. `None` when one frame or none is left: the texture is
    /// static and shows strip frame 0, as in Minecraft.
    pub fn timeline(&self, frame_count: u32, warnings: &mut Vec<String>) -> Option<Timeline> {
        let frames: Vec<TimedFrame> = if self.frames.is_empty() {
            (0..frame_count)
                .map(|index| TimedFrame {
                    index,
                    time: self.frametime,
                })
                .filter(|f| f.time > 0)
                .collect()
        } else {
            self.frames
                .iter()
                .enumerate()
                .filter_map(|(n, f)| {
                    let time = f.time.unwrap_or(self.frametime);
                    if time == 0 {
                        warnings.push(format!("animation frame {n}: invalid duration 0"));
                        None
                    } else if f.index >= frame_count {
                        warnings.push(format!(
                            "animation frame {n}: index {} outside the {frame_count} frames of \
                             the strip",
                            f.index
                        ));
                        None
                    } else {
                        Some(TimedFrame {
                            index: f.index,
                            time,
                        })
                    }
                })
                .collect()
        };
        if self.frames.is_empty() && self.frametime == 0 && frame_count > 1 {
            warnings.push("animation frametime 0: static".to_string());
        }
        (frames.len() > 1).then(|| Timeline {
            period: frames.iter().map(|f| u64::from(f.time)).sum(),
            frames,
            interpolate: self.interpolate,
        })
    }
}

/// One entry of a [`Timeline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedFrame {
    /// Index into the strip.
    pub index: u32,
    /// Duration in game ticks (at least 1).
    pub time: u32,
}

/// An [`Animation`] resolved against its strip ([`Animation::timeline`]): at least two frames,
/// played in a loop of [`Timeline::period`] game ticks (20 a second).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline {
    frames: Vec<TimedFrame>,
    interpolate: bool,
    period: u64,
}

/// What an animated texture shows at a game tick ([`Timeline::at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameAt {
    /// Strip index of the current frame.
    pub current: u32,
    /// Strip index of the frame after it in the sequence (wrapping), the one an interpolated
    /// animation blends toward.
    pub next: u32,
    /// Ticks already spent in the current frame (`0..time`).
    pub sub: u32,
    /// Duration of the current frame in ticks.
    pub time: u32,
}

impl FrameAt {
    /// Weight of the current frame in Minecraft's interpolation, `1 - sub / time` (1 on the
    /// tick the frame starts).
    pub fn weight(&self) -> f64 {
        1.0 - f64::from(self.sub) / f64::from(self.time)
    }
}

impl Timeline {
    /// The frame sequence.
    pub fn frames(&self) -> &[TimedFrame] {
        &self.frames
    }

    /// Whether the texture blends each frame into the next ([`mix_pixel`]).
    pub fn interpolate(&self) -> bool {
        self.interpolate
    }

    /// Ticks of one loop (the sum of the frame times).
    pub fn period(&self) -> u64 {
        self.period
    }

    /// The frame `tick` game ticks after the texture loaded. Minecraft's ticker starts on the
    /// first frame and advances one tick per client tick, so this is a pure function of the tick
    /// count: the loop position is `tick % period`.
    pub fn at(&self, tick: u64) -> FrameAt {
        let mut left = tick % self.period;
        for (n, frame) in self.frames.iter().enumerate() {
            let time = u64::from(frame.time);
            if left < time {
                let next = self.frames[(n + 1) % self.frames.len()].index;
                return FrameAt {
                    current: frame.index,
                    next,
                    sub: left as u32,
                    time: frame.time,
                };
            }
            left -= time;
        }
        unreachable!("the loop position is below the period")
    }

    /// The strip indices the sequence shows, sorted, each once.
    pub fn indices(&self) -> Vec<u32> {
        let mut indices: Vec<u32> = self.frames.iter().map(|f| f.index).collect();
        indices.sort_unstable();
        indices.dedup();
        indices
    }
}

/// One interpolated channel, Minecraft's `(int) (d * a + (1 - d) * b)` in `f64` (truncated).
pub fn mix_channel(weight: f64, a: u8, b: u8) -> u8 {
    (weight * f64::from(a) + (1.0 - weight) * f64::from(b)) as u8
}

/// One texel of an interpolated frame: the color channels blended with [`mix_channel`] and the
/// alpha of the current frame `a`, as Minecraft does on every mip level of the two frames.
pub fn mix_pixel(weight: f64, a: [u8; 4], b: [u8; 4]) -> [u8; 4] {
    [
        mix_channel(weight, a[0], b[0]),
        mix_channel(weight, a[1], b[1]),
        mix_channel(weight, a[2], b[2]),
        a[3],
    ]
}

/// Splits an animation strip into individual RGBA frames, row by row
/// ([`Animation::frame_size`]). An error when the image is not a whole number of frames.
pub fn split_frames(
    width: u32,
    height: u32,
    rgba: &[u8],
    anim: &Animation,
) -> Result<Vec<Vec<u8>>, String> {
    let (fw, fh) = anim.frame_size(width, height);
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

    fn anim(frametime: u32, interpolate: bool, frames: &[(u32, Option<u32>)]) -> Animation {
        Animation {
            frametime,
            interpolate,
            frames: frames
                .iter()
                .map(|&(index, time)| AnimationFrame { index, time })
                .collect(),
            width: None,
            height: None,
        }
    }

    #[test]
    fn frame_size_follows_minecraft() {
        let mut a = anim(1, false, &[]);
        assert_eq!(a.frame_size(16, 64), (16, 16));
        // a horizontal strip: squares of the height
        assert_eq!(a.frame_size(64, 16), (16, 16));
        a.width = Some(8);
        assert_eq!(a.frame_size(16, 64), (8, 64));
        a.height = Some(4);
        assert_eq!(a.frame_size(16, 64), (8, 4));
        a.width = None;
        assert_eq!(a.frame_size(16, 64), (16, 4));
    }

    /// No `frames` list: every strip frame in order, `frametime` ticks each, looping.
    #[test]
    fn every_strip_frame_plays_for_frametime() {
        let mut warnings = Vec::new();
        let t = anim(3, false, &[])
            .timeline(4, &mut warnings)
            .expect("animated");
        assert!(warnings.is_empty());
        assert_eq!(t.period(), 12);
        let shown: Vec<u32> = (0..14).map(|tick| t.at(tick).current).collect();
        assert_eq!(shown, [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 0, 0]);
        assert_eq!(
            t.at(4),
            FrameAt {
                current: 1,
                next: 2,
                sub: 1,
                time: 3
            }
        );
        // the last frame blends toward the first
        assert_eq!(t.at(11).next, 0);
        assert_eq!(t.indices(), [0, 1, 2, 3]);
    }

    /// An explicit list: its order and times (a bare index lasts `frametime`), repeats allowed.
    #[test]
    fn explicit_frames_keep_their_order_and_times() {
        let mut warnings = Vec::new();
        let t = anim(2, true, &[(2, None), (0, Some(5)), (2, Some(1))])
            .timeline(3, &mut warnings)
            .expect("animated");
        assert!(warnings.is_empty());
        assert_eq!(t.period(), 8);
        assert!(t.interpolate());
        let shown: Vec<(u32, u32)> = (0..9)
            .map(|tick| (t.at(tick).current, t.at(tick).sub))
            .collect();
        assert_eq!(
            shown,
            [
                (2, 0),
                (2, 1),
                (0, 0),
                (0, 1),
                (0, 2),
                (0, 3),
                (0, 4),
                (2, 0),
                (2, 0)
            ]
        );
        assert_eq!(t.indices(), [0, 2]);
        // far ticks wrap exactly (u64, no float)
        let far = (u64::MAX / 8) * 8 + 3;
        assert_eq!(t.at(far), t.at(3));
    }

    /// Minecraft drops frames with a bad index or no duration, and one frame left (or none) is
    /// a static texture.
    #[test]
    fn invalid_frames_are_dropped_and_one_frame_is_static() {
        let mut warnings = Vec::new();
        let t = anim(2, false, &[(0, None), (7, None), (1, Some(0)), (1, None)])
            .timeline(2, &mut warnings)
            .expect("two valid frames");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(t.frames().len(), 2);
        assert_eq!(t.period(), 4);
        let mut warnings = Vec::new();
        assert_eq!(
            anim(2, false, &[(1, None), (9, None)]).timeline(2, &mut warnings),
            None
        );
        assert_eq!(warnings.len(), 1);
        // a one-frame strip, or a zero frametime
        assert_eq!(anim(2, false, &[]).timeline(1, &mut Vec::new()), None);
        let mut warnings = Vec::new();
        assert_eq!(anim(0, false, &[]).timeline(4, &mut warnings), None);
        assert_eq!(warnings.len(), 1);
    }

    /// Minecraft's blend: `(int) (d * a + (1 - d) * b)` with `d = 1 - sub / time`, the alpha of
    /// the current frame.
    #[test]
    fn interpolation_matches_minecraft() {
        let at = |sub, time| FrameAt {
            current: 0,
            next: 1,
            sub,
            time,
        };
        assert_eq!(at(0, 4).weight(), 1.0);
        assert_eq!(at(1, 4).weight(), 0.75);
        assert_eq!(mix_channel(1.0, 200, 10), 200);
        // 0.75 * 200 + 0.25 * 10 = 152.5, truncated
        assert_eq!(mix_channel(0.75, 200, 10), 152);
        // 1/3 * 10 + 2/3 * 255 = 173.33...
        assert_eq!(mix_channel(at(2, 3).weight(), 10, 255), 173);
        assert_eq!(
            mix_pixel(0.5, [0, 100, 255, 7], [100, 0, 255, 200]),
            [50, 50, 255, 7]
        );
        // every byte pair at every weight of a 7-tick frame stays between its ends
        for sub in 0..7 {
            let w = at(sub, 7).weight();
            for a in (0..=255u8).step_by(5) {
                for b in (0..=255u8).step_by(3) {
                    let m = mix_channel(w, a, b);
                    assert!(m >= a.min(b) && m <= a.max(b), "{a} {b} {w}");
                }
            }
        }
    }
}
