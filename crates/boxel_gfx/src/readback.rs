/// Tightly packed RGBA8 image (sRGB-encoded bytes as stored in the texture).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// `width * height * 4` bytes, row-major, top row first.
    pub data: Vec<u8>,
}

/// Pixel comparison result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageDiff {
    /// Pixels whose max channel difference exceeds the tolerance.
    pub differing_pixels: usize,
    /// Total pixels.
    pub total_pixels: usize,
    /// Largest channel difference seen.
    pub max_channel_diff: u8,
}

impl ImageDiff {
    /// `differing_pixels / total_pixels`.
    pub fn fraction(&self) -> f64 {
        if self.total_pixels == 0 {
            0.0
        } else {
            self.differing_pixels as f64 / self.total_pixels as f64
        }
    }
}

impl RgbaImage {
    /// Compares RGB channels (alpha ignored). Panics if sizes differ.
    pub fn compare(&self, other: &RgbaImage, tolerance: u8) -> ImageDiff {
        assert_eq!(
            (self.width, self.height),
            (other.width, other.height),
            "image sizes differ"
        );
        let mut diff = ImageDiff {
            differing_pixels: 0,
            total_pixels: (self.width * self.height) as usize,
            max_channel_diff: 0,
        };
        for (a, b) in self
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .zip(other.data.as_chunks::<4>().0)
        {
            let d = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0);
            diff.max_channel_diff = diff.max_channel_diff.max(d);
            if d > tolerance {
                diff.differing_pixels += 1;
            }
        }
        diff
    }

    /// Fraction of pixels whose RGB differs (> 2) from the top-left pixel (sky check).
    pub fn non_background_fraction(&self) -> f64 {
        let Some(bg) = self.data.get(0..4) else {
            return 0.0;
        };
        let total = self.data.len() / 4;
        let differing = self
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| (0..3).any(|c| p[c].abs_diff(bg[c]) > 2))
            .count();
        if total == 0 {
            0.0
        } else {
            differing as f64 / total as f64
        }
    }

    /// Writes an RGBA PNG.
    #[cfg(feature = "png")]
    pub fn save_png(&self, path: &std::path::Path) -> std::io::Result<()> {
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut encoder = png::Encoder::new(file, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
        writer
            .write_image_data(&self.data)
            .map_err(std::io::Error::other)?;
        writer.finish().map_err(std::io::Error::other)
    }

    /// Reads a PNG written by [`RgbaImage::save_png`] (or any 8-bit RGB/RGBA/gray PNG; 16-bit
    /// is stripped, palettes expanded, missing alpha set to 255). Used by `voxel_world
    /// --compare-to` and image tests.
    #[cfg(feature = "png")]
    pub fn load_png(path: &std::path::Path) -> std::io::Result<RgbaImage> {
        let file = std::io::BufReader::new(std::fs::File::open(path)?);
        let mut decoder = png::Decoder::new(file);
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().map_err(std::io::Error::other)?;
        let size = reader
            .output_buffer_size()
            .ok_or_else(|| std::io::Error::other("png too large"))?;
        let mut buf = vec![0; size];
        let info = reader.next_frame(&mut buf).map_err(std::io::Error::other)?;
        buf.truncate(info.buffer_size());
        let data = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            png::ColorType::GrayscaleAlpha => buf
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => {
                return Err(std::io::Error::other("indexed png was not expanded"));
            }
        };
        Ok(RgbaImage {
            width: info.width,
            height: info.height,
            data,
        })
    }
}

/// Copies a 2D `Rgba8Unorm[Srgb]` or `Bgra8Unorm[Srgb]` texture (usage must include `COPY_SRC`)
/// to CPU memory, handling the 256-byte row alignment and swizzling BGRA to RGBA. Blocks.
pub fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<RgbaImage, String> {
    use wgpu::TextureFormat as F;
    let swizzle = match texture.format() {
        F::Rgba8Unorm | F::Rgba8UnormSrgb => false,
        F::Bgra8Unorm | F::Bgra8UnormSrgb => true,
        other => return Err(format!("read_texture_rgba8: unsupported format {other:?}")),
    };
    let (width, height) = (texture.width(), texture.height());
    let unpadded = width * 4;
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("boxel readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| e.to_string())?;
    rx.recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let mapped = slice.get_mapped_range().map_err(|e| e.to_string())?;
    let mut data = Vec::with_capacity((unpadded * height) as usize);
    for row in mapped.chunks_exact(padded as usize) {
        data.extend_from_slice(&row[..unpadded as usize]);
    }
    drop(mapped);
    buffer.unmap();
    if swizzle {
        for px in data.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }
    Ok(RgbaImage {
        width,
        height,
        data,
    })
}
