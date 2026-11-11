//! One-time mip chains and minification-safe sampling for authored material
//! textures.
//!
//! A tiled finish texture (cladding boards, roof seams, tiles) covers far fewer
//! screen pixels than it has texels as soon as the camera pulls back. Sampled
//! from a single mip level, the fine lines alias into moiré: dashed or diagonal
//! bands whose direction follows the surface's angle on screen. The fix belongs
//! at the texture decode boundary, where it runs once per texture, rather than
//! in any per-frame or per-entity path.
//!
//! Bevy 0.20's `bevy_core_pipeline::mip_generation` (GPU single-pass
//! downsampling) was evaluated and not used here: it has no sRGB texture
//! formats, needs compute shaders and storage bindings (unavailable on the
//! WebGL2 target), and is a per-frame phase mechanism for images that change.
//! Authored material textures are static, so the chain is built on the CPU at
//! decode time and uploaded with the base level.

use bevy::{
    image::{Image, ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor},
    render::render_resource::{TextureDimension, TextureFormat},
};

/// Highest anisotropy any backend accepts; wgpu clamps it down, or to 1 where
/// the device has no anisotropic filtering.
const MATERIAL_ANISOTROPY_CLAMP: u16 = 16;
const BYTES_PER_TEXEL: usize = 4;
/// Resolution of the linear→sRGB encode table. 4096 steps keep every 8-bit sRGB
/// code reachable, including the steep dark end of the curve.
const SRGB_ENCODE_STEPS: usize = 4096;

/// Sampler for tiled material textures: repeat addressing (UV scale describes
/// tiling), trilinear filtering across the mip chain, and anisotropic filtering
/// for the oblique views walls, roofs and terrain are normally seen at.
pub fn material_texture_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: MATERIAL_ANISOTROPY_CLAMP,
        ..ImageSamplerDescriptor::default()
    })
}

/// How a format's colour channels are averaged when downsampling.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorEncoding {
    /// Stored values are linear; average them directly.
    Linear,
    /// Stored values are sRGB-encoded; average in linear light so a 50/50 mix
    /// of dark joint and bright board keeps its perceived brightness.
    Srgb,
}

fn color_encoding(format: TextureFormat) -> Option<ColorEncoding> {
    match format {
        TextureFormat::Rgba8UnormSrgb => Some(ColorEncoding::Srgb),
        TextureFormat::Rgba8Unorm => Some(ColorEncoding::Linear),
        _ => None,
    }
}

/// Number of mip levels in a full chain down to 1×1.
pub fn full_mip_level_count(width: u32, height: u32) -> u32 {
    u32::BITS - width.max(height).max(1).leading_zeros()
}

/// Appends a full box-filtered mip chain to a decoded 2D RGBA8 image.
///
/// Images that already carry mips, have no CPU-side data, or use a format this
/// module does not average are returned unchanged; they keep rendering exactly
/// as before.
pub fn with_full_mip_chain(mut image: Image) -> Image {
    let descriptor = &image.texture_descriptor;
    let (width, height) = (descriptor.size.width, descriptor.size.height);
    let Some(encoding) = color_encoding(descriptor.format) else {
        return image;
    };
    if descriptor.dimension != TextureDimension::D2
        || descriptor.size.depth_or_array_layers != 1
        || descriptor.mip_level_count != 1
        || width.max(height) <= 1
    {
        return image;
    }
    let Some(data) = image.data.as_mut() else {
        return image;
    };
    if data.len() != width as usize * height as usize * BYTES_PER_TEXEL {
        return image;
    }

    let levels = full_mip_level_count(width, height);
    let tables = SrgbTables::for_encoding(encoding);
    // A full chain adds one third to the base level.
    data.reserve(data.len() / 3 + BYTES_PER_TEXEL);
    let (mut level_start, mut level_width, mut level_height) = (0, width, height);
    for _ in 1..levels {
        let (next_width, next_height) = ((level_width / 2).max(1), (level_height / 2).max(1));
        let next_start = data.len();
        data.resize(
            next_start + next_width as usize * next_height as usize * BYTES_PER_TEXEL,
            0,
        );
        let (source, target) = data.split_at_mut(next_start);
        downsample_level(
            &source[level_start..],
            (level_width, level_height),
            target,
            (next_width, next_height),
            tables.as_ref(),
        );
        (level_start, level_width, level_height) = (next_start, next_width, next_height);
    }
    image.texture_descriptor.mip_level_count = levels;
    image
}

/// sRGB decode/encode lookup tables, built only for sRGB-encoded textures.
struct SrgbTables {
    decode: [f32; 256],
    encode: Vec<u8>,
}

impl SrgbTables {
    fn for_encoding(encoding: ColorEncoding) -> Option<Self> {
        (encoding == ColorEncoding::Srgb).then(|| Self {
            decode: std::array::from_fn(|code| srgb_to_linear(code as f32 / 255.0)),
            encode: (0..=SRGB_ENCODE_STEPS)
                .map(|step| {
                    (linear_to_srgb(step as f32 / SRGB_ENCODE_STEPS as f32) * 255.0).round() as u8
                })
                .collect(),
        })
    }
}

fn srgb_to_linear(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// Box-filters one mip level into the next. Each target texel averages the
/// source block it covers; an odd source edge folds its last row or column
/// into the final block so no texel is dropped.
fn downsample_level(
    source: &[u8],
    (source_width, source_height): (u32, u32),
    target: &mut [u8],
    (target_width, target_height): (u32, u32),
    srgb: Option<&SrgbTables>,
) {
    let (source_width, source_height) = (source_width as usize, source_height as usize);
    let (target_width, target_height) = (target_width as usize, target_height as usize);
    let span = |index: usize, target_len: usize, source_len: usize| {
        let start = index * 2;
        let end = if index + 1 == target_len {
            source_len
        } else {
            start + 2
        };
        start.min(source_len - 1)..end.max(start + 1).min(source_len)
    };
    for y in 0..target_height {
        let rows = span(y, target_height, source_height);
        for x in 0..target_width {
            let columns = span(x, target_width, source_width);
            let mut sum = [0.0f32; BYTES_PER_TEXEL];
            for row in rows.clone() {
                for column in columns.clone() {
                    let texel = (row * source_width + column) * BYTES_PER_TEXEL;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        let code = source[texel + channel];
                        // Alpha is coverage, never gamma-encoded.
                        *total += match srgb {
                            Some(tables) if channel < 3 => tables.decode[code as usize],
                            _ => code as f32,
                        };
                    }
                }
            }
            let count = (rows.len() * columns.len()) as f32;
            let out = (y * target_width + x) * BYTES_PER_TEXEL;
            for (channel, total) in sum.iter().enumerate() {
                let mean = total / count;
                target[out + channel] = match srgb {
                    Some(tables) if channel < 3 => {
                        tables.encode[(mean * SRGB_ENCODE_STEPS as f32).round() as usize]
                    }
                    _ => mean.round() as u8,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{asset::RenderAssetUsages, render::render_resource::Extent3d};

    fn rgba_image(
        width: u32,
        height: u32,
        format: TextureFormat,
        texel: impl Fn(u32, u32) -> [u8; 4],
    ) -> Image {
        let data = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| texel(x, y))
            .collect();
        Image::new(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            data,
            format,
            RenderAssetUsages::RENDER_WORLD,
        )
    }

    fn level(image: &Image, index: u32) -> (&[u8], u32, u32) {
        let (mut width, mut height) = (image.width(), image.height());
        let mut start = 0;
        for _ in 0..index {
            start += (width * height) as usize * BYTES_PER_TEXEL;
            (width, height) = ((width / 2).max(1), (height / 2).max(1));
        }
        let data = image.data.as_ref().unwrap();
        (
            &data[start..start + (width * height) as usize * BYTES_PER_TEXEL],
            width,
            height,
        )
    }

    /// The garage cladding texture is 8×64: thin dark joints on bright boards.
    fn cladding(format: TextureFormat) -> Image {
        rgba_image(8, 64, format, |_, y| {
            if y % 8 == 0 {
                [40, 30, 20, 255]
            } else {
                [220, 180, 100, 255]
            }
        })
    }

    #[test]
    fn chain_reaches_one_texel_and_fills_the_buffer_exactly() {
        let image = with_full_mip_chain(cladding(TextureFormat::Rgba8UnormSrgb));
        assert_eq!(image.texture_descriptor.mip_level_count, 7);
        let expected: usize = [(8, 64), (4, 32), (2, 16), (1, 8), (1, 4), (1, 2), (1, 1)]
            .iter()
            .map(|(w, h)| w * h * BYTES_PER_TEXEL)
            .sum();
        assert_eq!(image.data.as_ref().unwrap().len(), expected);
    }

    #[test]
    fn minified_cladding_converges_to_one_even_tone_instead_of_stripes() {
        let image = with_full_mip_chain(cladding(TextureFormat::Rgba8UnormSrgb));
        // Once a mip texel spans a whole board-plus-joint period (8 rows), every
        // texel in the level must hold the same blend: nothing left to alias.
        let (texels, width, height) = level(&image, 3);
        assert_eq!((width, height), (1, 8));
        let first = &texels[..BYTES_PER_TEXEL];
        assert!(texels.chunks(BYTES_PER_TEXEL).all(|texel| texel == first));
        // The blend sits between joint and board, close to the board.
        assert!(first[0] > 40 && first[0] < 220, "red {}", first[0]);
    }

    #[test]
    fn srgb_levels_average_in_linear_light() {
        let checker = |format| {
            with_full_mip_chain(rgba_image(2, 2, format, |x, y| {
                if (x + y) % 2 == 0 {
                    [0, 0, 0, 255]
                } else {
                    [255, 255, 255, 255]
                }
            }))
        };
        let srgb = checker(TextureFormat::Rgba8UnormSrgb);
        let linear = checker(TextureFormat::Rgba8Unorm);
        // Half black, half white is 50% light: sRGB code 188, not 128.
        assert_eq!(level(&srgb, 1).0, [188, 188, 188, 255]);
        assert_eq!(level(&linear, 1).0, [128, 128, 128, 255]);
    }

    #[test]
    fn odd_sizes_keep_every_source_texel_in_the_average() {
        // 3×1: the last block must fold in the third texel.
        let image = with_full_mip_chain(rgba_image(3, 1, TextureFormat::Rgba8Unorm, |x, _| {
            [[0, 0, 0, 255], [0, 0, 0, 255], [255, 255, 255, 255]][x as usize]
        }));
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        assert_eq!(level(&image, 1).0, [85, 85, 85, 255]);
    }

    #[test]
    fn unsupported_or_already_mipped_images_are_left_untouched() {
        let single = rgba_image(1, 1, TextureFormat::Rgba8UnormSrgb, |_, _| [1, 2, 3, 4]);
        assert_eq!(
            with_full_mip_chain(single)
                .texture_descriptor
                .mip_level_count,
            1
        );

        let mut mipped = cladding(TextureFormat::Rgba8UnormSrgb);
        mipped.texture_descriptor.mip_level_count = 2;
        let len = mipped.data.as_ref().unwrap().len();
        assert_eq!(with_full_mip_chain(mipped).data.unwrap().len(), len);

        let mut other = cladding(TextureFormat::Rgba8UnormSrgb);
        other.texture_descriptor.format = TextureFormat::Bgra8UnormSrgb;
        assert_eq!(
            with_full_mip_chain(other)
                .texture_descriptor
                .mip_level_count,
            1
        );
    }

    #[test]
    fn material_sampler_is_trilinear_anisotropic_and_tiling() {
        let ImageSampler::Descriptor(sampler) = material_texture_sampler() else {
            panic!("material sampler must be explicit");
        };
        assert_eq!(sampler.address_mode_u, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_v, ImageAddressMode::Repeat);
        // wgpu rejects anisotropy unless all three filters are linear.
        assert_eq!(sampler.mag_filter, ImageFilterMode::Linear);
        assert_eq!(sampler.min_filter, ImageFilterMode::Linear);
        assert_eq!(sampler.mipmap_filter, ImageFilterMode::Linear);
        assert_eq!(sampler.anisotropy_clamp, MATERIAL_ANISOTROPY_CLAMP);
    }

    /// Decode-time budget for the largest texture in the representative site
    /// model (the 4096² orthophoto). Run in an optimized build:
    /// `cargo test --release -p talos3d-core --lib texture_mipmaps -- --ignored --nocapture`
    #[test]
    #[ignore = "timing evidence; run in release"]
    fn full_chain_for_a_4096_square_texture_stays_within_budget() {
        let image = rgba_image(4096, 4096, TextureFormat::Rgba8UnormSrgb, |x, y| {
            [(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 239) as u8, 255]
        });
        let started = std::time::Instant::now();
        let image = with_full_mip_chain(image);
        let elapsed = started.elapsed();
        println!("4096² sRGB mip chain: {elapsed:?}");
        assert_eq!(image.texture_descriptor.mip_level_count, 13);
        assert!(elapsed.as_millis() < 500, "mip chain took {elapsed:?}");
    }
}
