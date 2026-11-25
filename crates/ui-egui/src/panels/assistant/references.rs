//! Reference images the person attaches to a message ("make it look like this"): picked from a
//! file, pasted from the clipboard, or taken from another open document. They go to Claude as
//! images beside the message and never into the document.

use base64::Engine as _;
use serde_json::{Value, json};

/// The longest side a reference is sent at: larger images are scaled down to it first.
pub(crate) const MAX_SIDE: u32 = 1568;
/// The longest side of the thumbnail the panel shows.
const THUMB_SIDE: u32 = 96;
/// The largest file read as a reference.
const MAX_FILE: usize = 200 * 1024 * 1024;

/// One attached image, encoded for sending.
#[derive(Clone)]
pub(crate) struct Reference {
    pub name: String,
    media_type: &'static str,
    bytes: Vec<u8>,
    /// The thumbnail's pixels, uploaded as a texture when first shown.
    thumb: Option<egui::ColorImage>,
    texture: Option<egui::TextureHandle>,
}

impl std::fmt::Debug for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reference").field("name", &self.name).field("media_type", &self.media_type).field("bytes", &self.bytes.len()).finish()
    }
}

impl PartialEq for Reference {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.bytes == other.bytes
    }
}

impl Reference {
    /// A reference from an image file's contents (PNG, JPEG, WebP, TIFF, GIF, …).
    pub(crate) fn from_file(name: &str, bytes: &[u8]) -> Result<Self, String> {
        let short = short_name(name);
        if bytes.len() > MAX_FILE {
            return Err(crate::i18n::fmt(tl!("{name} is too large to attach."), &[("name", short.as_str())]));
        }
        let options = photocraft_codecs::DecodeOptions {
            limits: photocraft_codecs::Limits { max_width: 30_000, max_height: 30_000, max_pixels: 300_000_000, max_alloc: 1 << 31 },
            ..Default::default()
        };
        let image = photocraft_codecs::decode_with(bytes, &options)
            .map_err(|e| crate::i18n::fmt(tl!("{name} isn't an image PhotoCraft can read ({why})."), &[("name", short.as_str()), ("why", e.to_string().as_str())]))?;
        let rgba = image.convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8);
        Self::from_rgba(&short, rgba.width(), rgba.height(), rgba.data())
    }

    /// A reference from straight RGBA8 pixels (a clipboard image, a document's composite).
    pub(crate) fn from_rgba(name: &str, width: u32, height: u32, pixels: &[u8]) -> Result<Self, String> {
        if width == 0 || height == 0 || pixels.len() != width as usize * height as usize * 4 {
            return Err(crate::i18n::fmt(tl!("{name} has no pixels to attach."), &[("name", name)]));
        }
        let (w, h, px) = fit(width, height, pixels, MAX_SIDE);
        let opaque = px.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
        // Photos travel as JPEG, which keeps them well inside the size a message may carry; images
        // with transparency stay PNG.
        let (format, media_type, layout, data) = if opaque {
            let rgb: Vec<u8> = px.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
            (photocraft_codecs::Format::Jpeg, "image/jpeg", photocraft_codecs::ChannelLayout::Rgb, rgb)
        } else {
            (photocraft_codecs::Format::Png, "image/png", photocraft_codecs::ChannelLayout::Rgba, px.clone())
        };
        let image = photocraft_codecs::Image::from_u8(w, h, layout, data).map_err(|e| e.to_string())?;
        let options = photocraft_codecs::EncodeOptions { jpeg_quality: 88, ..Default::default() };
        let bytes = photocraft_codecs::encode(&image, format, &options).map_err(|e| e.to_string())?;
        let (tw, th, tpx) = fit(w, h, &px, THUMB_SIDE);
        let thumb = egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], &tpx);
        Ok(Self { name: name.to_owned(), media_type, bytes, thumb: Some(thumb), texture: None })
    }

    /// The Messages API image block.
    pub(crate) fn block(&self) -> Value {
        let data = base64::engine::general_purpose::STANDARD.encode(&self.bytes);
        json!({"type": "image", "source": {"type": "base64", "media_type": self.media_type, "data": data}})
    }

    /// The thumbnail texture once [`Self::texture`] uploaded it.
    pub(crate) fn shown(&self) -> Option<&egui::TextureHandle> {
        self.texture.as_ref()
    }

    /// The thumbnail texture, uploaded on first use.
    pub(crate) fn texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.texture.is_none()
            && let Some(thumb) = self.thumb.take()
        {
            self.texture = Some(ctx.load_texture(format!("assistant-reference-{}", self.name), thumb, egui::TextureOptions::LINEAR));
        }
        self.texture.clone()
    }
}

/// A path's file name, or the name as it is.
fn short_name(name: &str) -> String {
    std::path::Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or(name).to_owned()
}

/// `pixels` scaled down by area averaging so the longer side is at most `max` (as they are when
/// they already fit).
fn fit(width: u32, height: u32, pixels: &[u8], max: u32) -> (u32, u32, Vec<u8>) {
    let long = width.max(height);
    if long <= max {
        return (width, height, pixels.to_vec());
    }
    let scale = f64::from(max) / f64::from(long);
    let w = ((f64::from(width) * scale).round() as u32).max(1);
    let h = ((f64::from(height) * scale).round() as u32).max(1);
    let mut out = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h {
        let y0 = (u64::from(y) * u64::from(height) / u64::from(h)) as u32;
        let y1 = ((u64::from(y + 1) * u64::from(height)).div_ceil(u64::from(h)) as u32).clamp(y0 + 1, height);
        for x in 0..w {
            let x0 = (u64::from(x) * u64::from(width) / u64::from(w)) as u32;
            let x1 = ((u64::from(x + 1) * u64::from(width)).div_ceil(u64::from(w)) as u32).clamp(x0 + 1, width);
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                let row = sy as usize * width as usize;
                for sx in x0..x1 {
                    let i = (row + sx as usize) * 4;
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += u64::from(pixels[i + c]);
                    }
                }
            }
            let n = u64::from((y1 - y0) * (x1 - x0));
            let o = (y as usize * w as usize + x as usize) * 4;
            for c in 0..4 {
                out[o + c] = (sum[c] / n) as u8;
            }
        }
    }
    (w, h, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A large reference is scaled to fit what Claude reads, keeping its proportions; photos go as
    // JPEG and images with transparency as PNG.
    #[test]
    fn references_fit_and_keep_their_shape() {
        let (w, h) = (4000, 1000);
        let opaque: Vec<u8> = [200u8, 120, 40, 255].repeat(w * h);
        let r = Reference::from_rgba("wide", w as u32, h as u32, &opaque).unwrap();
        assert_eq!(r.media_type, "image/jpeg");
        let img = photocraft_codecs::decode_with(&r.bytes, &Default::default()).unwrap();
        assert!(img.width() <= MAX_SIDE && img.height() <= MAX_SIDE);
        assert!(img.width() > img.height() * 3, "the 4:1 shape survives scaling");
        let mut clear = vec![0u8; 8 * 8 * 4];
        clear[3] = 255;
        assert_eq!(Reference::from_rgba("clear", 8, 8, &clear).unwrap().media_type, "image/png");
        assert!(Reference::from_file("notes.txt", b"not an image").is_err());
    }
}
