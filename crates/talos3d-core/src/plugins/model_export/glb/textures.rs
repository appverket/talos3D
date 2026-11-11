//! Resolve source images, including images whose CPU copy Bevy has released.
use super::*;
use crate::plugins::materials::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use bevy::math::Affine2;
use std::collections::HashMap;

#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Maps(pub [Option<usize>; 5], pub [u32; 3], pub u8, pub u32);

#[derive(Default)]
pub(super) struct Images {
    bytes: Vec<(Vec<u8>, &'static str)>,
    sources: HashMap<String, usize>,
    contents: HashMap<blake3::Hash, usize>,
}
impl Images {
    fn add(&mut self, bytes: Vec<u8>) -> Result<usize, String> {
        let mime = match image::guess_format(&bytes) {
            Ok(image::ImageFormat::Jpeg) => "image/jpeg",
            Ok(image::ImageFormat::Png) => "image/png",
            _ => return Err("Cannot encode this texture format in a portable GLB".into()),
        };
        let hash = blake3::hash(&bytes);
        if let Some(id) = self.contents.get(&hash) {
            return Ok(*id);
        }
        if bytes.len() + self.bytes.iter().map(|(b, _)| b.len()).sum::<usize>() > MAX_BYTES {
            return Err("Texture images exceed the publication size limit".into());
        }
        let id = self.bytes.len();
        self.bytes.push((bytes, mime));
        self.contents.insert(hash, id);
        Ok(id)
    }
    fn source(&mut self, w: &World, t: &TextureRef) -> Result<usize, String> {
        let key = match t {
            TextureRef::TextureAsset { id } => id.0.clone(),
            TextureRef::AssetPath { path } => format!("path:{path}"),
            TextureRef::Embedded { data, .. } => {
                format!("embedded:{}", blake3::hash(data.as_bytes()))
            }
        };
        if let Some(i) = self.sources.get(&key) {
            return Ok(*i);
        }
        let bytes = match t {
            TextureRef::TextureAsset { id } => match &w
                .get_resource::<TextureRegistry>()
                .and_then(|r| r.get(id))
                .ok_or("Referenced texture is missing")?
                .payload
            {
                TexturePayload::Embedded { data, .. } => STANDARD
                    .decode(data)
                    .map_err(|_| "Invalid embedded texture")?,
                TexturePayload::AssetPath(p) => {
                    crate::plugins::materials::portable_asset_bytes(w, p)?
                }
            },
            TextureRef::Embedded { data, .. } => STANDARD
                .decode(data)
                .map_err(|_| "Invalid embedded texture")?,
            TextureRef::AssetPath { path } => {
                crate::plugins::materials::portable_asset_bytes(w, path)?
            }
        };
        let i = self.add(bytes)?;
        self.sources.insert(key, i);
        Ok(i)
    }
    fn runtime(&mut self, w: &World, h: &Handle<Image>) -> Result<usize, String> {
        let key = format!("runtime:{:?}", h.id());
        if let Some(i) = self.sources.get(&key) {
            return Ok(*i);
        }
        let img = w
            .get_resource::<Assets<Image>>()
            .and_then(|a| a.get(h))
            .ok_or("A texture is still loading or unavailable")?;
        let rgba = img
            .clone()
            .try_into_dynamic()
            .map_err(|_| "Unsupported renderer texture")?;
        let mut png = std::io::Cursor::new(Vec::new());
        rgba.write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        let i = self.add(png.into_inner())?;
        self.sources.insert(key, i);
        Ok(i)
    }
    pub(super) fn finish(
        self,
        bin: &mut Vec<u8>,
        views: &mut Vec<Value>,
    ) -> (Vec<Value>, Vec<Value>) {
        let mut images = Vec::new();
        let mut textures = Vec::new();
        for (b, mime) in self.bytes {
            while bin.len() % 4 != 0 {
                bin.push(0)
            }
            let i = views.len();
            views.push(json!({"buffer":0,"byteOffset":bin.len(),"byteLength":b.len()}));
            bin.extend(b);
            textures.push(json!({"source":images.len()}));
            images.push(json!({"bufferView":i,"mimeType":mime}));
        }
        (images, textures)
    }
}
pub(super) struct Surface {
    pub color: [f32; 4],
    pub rough: f32,
    pub metal: f32,
    pub maps: Maps,
    pub uv: Affine2,
}
impl Surface {
    pub fn new(
        w: &World,
        mat: Option<&StandardMaterial>,
        def: Option<&MaterialDef>,
        mapping: Option<TextureMapping>,
        imgs: &mut Images,
    ) -> Result<Self, String> {
        let mut maps = Maps::default();
        let (color, rough, metal, uv, emissive, alpha) = if let Some(d) = def {
            for (i, t) in [
                &d.base_color_texture,
                &d.normal_map_texture,
                &d.metallic_roughness_texture,
                &d.occlusion_texture,
                &d.emissive_texture,
            ]
            .into_iter()
            .enumerate()
            {
                if let Some(t) = t {
                    maps.0[i] = Some(imgs.source(w, t)?)
                }
            }
            let m = mapping.unwrap_or_else(|| d.texture_mapping());
            let uv = if m.renderer_supported() {
                Affine2::from_scale_angle_translation(
                    Vec2::new(
                        if m.flip_u { -m.scale.x } else { m.scale.x },
                        if m.flip_v { -m.scale.y } else { m.scale.y },
                    ),
                    m.rotation,
                    Vec2::new(
                        m.offset.x + u8::from(m.flip_u) as f32,
                        m.offset.y + u8::from(m.flip_v) as f32,
                    ),
                )
            } else {
                Affine2::IDENTITY
            };
            let [r, g, b, a] = d.base_color;
            (
                Color::srgba(r, g, b, a).to_linear().to_f32_array(),
                d.perceptual_roughness,
                d.metallic,
                uv,
                d.emissive,
                d.alpha_mode.to_bevy(d.alpha_cutoff),
            )
        } else if let Some(m) = mat {
            for (i, t) in [
                &m.base_color_texture,
                &m.normal_map_texture,
                &m.metallic_roughness_texture,
                &m.occlusion_texture,
                &m.emissive_texture,
            ]
            .into_iter()
            .enumerate()
            {
                if let Some(h) = t {
                    maps.0[i] = Some(imgs.runtime(w, h)?)
                }
            }
            (
                m.base_color.to_linear().to_f32_array(),
                m.perceptual_roughness,
                m.metallic,
                m.uv_transform,
                [m.emissive.red, m.emissive.green, m.emissive.blue],
                m.alpha_mode,
            )
        } else {
            (
                [0.7, 0.7, 0.7, 1.],
                0.8,
                0.,
                Affine2::IDENTITY,
                [0.; 3],
                AlphaMode::Opaque,
            )
        };
        maps.1 = emissive.map(|v| v.clamp(0., 1.).to_bits());
        match alpha {
            AlphaMode::Opaque => {}
            AlphaMode::Mask(c) => {
                maps.2 = 1;
                maps.3 = c.to_bits()
            }
            _ => maps.2 = 2,
        };
        Ok(Self {
            color,
            rough,
            metal,
            uv,
            maps,
        })
    }
}
impl Maps {
    pub fn apply(self, m: &mut Value) {
        for (index, key) in [
            "baseColorTexture",
            "normalTexture",
            "metallicRoughnessTexture",
            "occlusionTexture",
            "emissiveTexture",
        ]
        .iter()
        .enumerate()
        {
            if let Some(i) = self.0[index] {
                let parent = if index == 0 || index == 2 {
                    &mut m["pbrMetallicRoughness"]
                } else {
                    &mut *m
                };
                parent[*key] = json!({"index":i});
            }
        }
        m["emissiveFactor"] = json!(self.1.map(f32::from_bits));
        m["alphaMode"] = json!(["OPAQUE", "MASK", "BLEND"][self.2 as usize]);
        if self.2 == 1 {
            m["alphaCutoff"] = json!(f32::from_bits(self.3));
        }
    }
}
