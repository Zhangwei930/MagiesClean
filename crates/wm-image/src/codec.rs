//! 解码与编码（规格 §8）。
//!
//! 解码时统一应用 EXIF Orientation；编码时按格式保留 ICC / EXIF / DPI，
//! 并把 EXIF Orientation 同步为 1，避免显示时二次旋转。

use crate::format::{check_capability, ImageFormatKind};
use crate::metadata::{
    dpi_from_exif, dpi_from_jfif, dpi_from_png, estimate_jpeg_quality, patch_exif_orientation, png_insert_phys, ImageMetadata, SourceColor,
};
use image::{ColorType, DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Cursor, Read};
use std::path::Path;
use wm_core::msg;
use wm_core::{AppError, ImageBuffer, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    /// 已定向后的宽高。
    pub width: u32,
    pub height: u32,
    pub format: ImageFormatKind,
    pub file_size: u64,
    pub has_alpha: bool,
    pub orientation: u8,
}

impl ImageInfo {
    /// 解码为 RGBA8 所需的字节数估计。
    pub fn decoded_bytes(&self) -> u64 {
        self.width as u64 * self.height as u64 * 4
    }
}

pub struct DecodedImage {
    pub buffer: ImageBuffer,
    pub info: ImageInfo,
    pub metadata: ImageMetadata,
}

fn source_color(c: ColorType) -> Result<SourceColor> {
    Ok(match c {
        ColorType::L8 => SourceColor::L8,
        ColorType::La8 => SourceColor::La8,
        ColorType::Rgb8 => SourceColor::Rgb8,
        ColorType::Rgba8 => SourceColor::Rgba8,
        _ => {
            return Err(AppError::unsupported(msg!(
                "16 位或浮点色深图片暂不支持（为避免静默降低位深，未做处理）",
                "16-bit and floating-point images are not supported yet (left untouched to avoid silently reducing bit depth)"
            )));
        }
    })
}

/// 只读取文件头：尺寸、格式、Orientation。
pub fn probe(path: &Path) -> Result<ImageInfo> {
    let file_size = std::fs::metadata(path)?.len();
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let format = reader
        .format()
        .and_then(ImageFormatKind::from_image_format)
        .ok_or_else(|| AppError::unsupported(msg!("不支持的图片格式", "Unsupported image format")))?;
    let mut dec = reader.into_decoder().map_err(|e| AppError::decode(msg!("无法读取图片", "Could not read the image")).with_detail(e))?;
    let (w, h) = dec.dimensions();
    let has_alpha = dec.color_type().has_alpha();
    let orientation = dec.orientation().map(|o| o.to_exif()).unwrap_or(1);
    let (width, height) = if matches!(orientation, 5..=8) { (h, w) } else { (w, h) };
    Ok(ImageInfo { width, height, format, file_size, has_alpha, orientation })
}

/// 完整解码。`max_bytes` 为本次解码允许的最大 RGBA 占用（内存预算）。
pub fn decode(path: &Path, max_bytes: Option<u64>) -> Result<DecodedImage> {
    let info = probe(path)?;
    check_capability(path, info.format)?;
    if let Some(limit) = max_bytes {
        // RGBA 缓冲 + 解码中间结果，按 2 倍估计
        if info.decoded_bytes() * 2 > limit {
            return Err(AppError::oom(msg!(
                format!("图片过大（{}×{}），超出当前内存预算", info.width, info.height),
                format!("Image is too large ({}×{}) for the current memory budget", info.width, info.height)
            )));
        }
    }
    let raw = std::fs::read(path)?;
    decode_bytes(&raw, info.file_size)
}

/// 从内存字节解码（PDF 中嵌入的图片也走这里）。
pub fn decode_bytes(raw: &[u8], file_size: u64) -> Result<DecodedImage> {
    let mut reader = image::ImageReader::new(Cursor::new(raw)).with_guessed_format()?;
    reader.no_limits();
    let format = reader
        .format()
        .and_then(ImageFormatKind::from_image_format)
        .ok_or_else(|| AppError::unsupported(msg!("不支持的图片格式", "Unsupported image format")))?;
    let mut dec = reader.into_decoder().map_err(|e| AppError::decode(msg!("无法读取图片", "Could not read the image")).with_detail(e))?;
    let color = source_color(dec.color_type())?;
    let orientation = dec.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let icc = dec.icc_profile().ok().flatten();
    let exif = dec.exif_metadata().ok().flatten();
    let mut img = DynamicImage::from_decoder(dec)
        .map_err(|e| AppError::decode(msg!("图片数据损坏，无法解码", "Image data is damaged and cannot be decoded")).with_detail(e))?;
    img.apply_orientation(orientation);
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    let dpi = match format {
        ImageFormatKind::Jpeg => dpi_from_jfif(raw),
        ImageFormatKind::Png => dpi_from_png(raw),
        _ => None,
    }
    .or_else(|| exif.as_deref().and_then(dpi_from_exif));
    let jpeg_quality = (format == ImageFormatKind::Jpeg).then(|| estimate_jpeg_quality(raw)).flatten();

    let buffer = ImageBuffer { width, height, data: rgba.into_raw() };
    let info = ImageInfo { width, height, format, file_size, has_alpha: color.has_alpha(), orientation: orientation.to_exif() };
    let metadata = ImageMetadata { exif, icc, dpi, orientation: info.orientation, color, jpeg_quality };
    Ok(DecodedImage { buffer, info, metadata })
}

/// 判断字节流是否为可识别的图片（供 PDF 嵌入图片使用）。
pub fn sniff_bytes(raw: &[u8]) -> Option<ImageFormatKind> {
    image::guess_format(raw).ok().and_then(ImageFormatKind::from_image_format)
}

#[derive(Debug, Clone)]
pub struct EncodeOptions<'a> {
    pub format: ImageFormatKind,
    pub jpeg_quality: u8,
    /// 保留的元数据（None = 不写入 EXIF/ICC/DPI）。
    pub metadata: Option<&'a ImageMetadata>,
    /// 输出通道布局（None = 按图像内容自动选择）。
    pub color: Option<SourceColor>,
}

/// 编码结果附带说明（例如：JPEG 不支持透明，已合成到白底）。
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub notes: Vec<wm_core::Msg>,
}

pub fn encode(buf: &ImageBuffer, opts: &EncodeOptions) -> Result<Encoded> {
    let mut notes = Vec::new();
    let has_alpha_px = buf.has_transparency();
    let want = opts.color.unwrap_or(if has_alpha_px { SourceColor::Rgba8 } else { SourceColor::Rgb8 });
    let mut color = want;
    if !opts.format.supports_alpha() && color.has_alpha() {
        if has_alpha_px {
            notes.push(msg!(
                "JPEG 不支持透明通道，透明区域已合成到白色背景",
                "JPEG has no transparency; transparent areas were flattened onto white"
            ));
        }
        color = if color.is_gray() { SourceColor::L8 } else { SourceColor::Rgb8 };
    }
    if has_alpha_px && !color.has_alpha() && opts.format.supports_alpha() {
        color = if color.is_gray() { SourceColor::La8 } else { SourceColor::Rgba8 };
    }
    // BMP 编码器不支持 La8
    if opts.format == ImageFormatKind::Bmp && color == SourceColor::La8 {
        color = SourceColor::Rgba8;
    }
    let (pixels, ect) = convert(buf, color);

    let exif = opts.metadata.and_then(|m| m.exif.clone()).map(|mut e| {
        patch_exif_orientation(&mut e);
        e
    });
    let icc = opts.metadata.and_then(|m| m.icc.clone());
    let dpi = opts.metadata.and_then(|m| m.dpi);
    let (w, h) = (buf.width, buf.height);
    let map = |e: image::ImageError| AppError::encode(msg!("图片编码失败", "Image encoding failed")).with_detail(e);

    let mut out = Vec::new();
    match opts.format {
        ImageFormatKind::Jpeg => {
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, opts.jpeg_quality.clamp(1, 100));
            if let Some((x, y)) = dpi {
                enc.set_pixel_density(image::codecs::jpeg::PixelDensity {
                    density: (x.round().clamp(1.0, 65535.0) as u16, y.round().clamp(1.0, 65535.0) as u16),
                    unit: image::codecs::jpeg::PixelDensityUnit::Inches,
                });
            }
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            if let Some(e) = exif {
                let _ = enc.set_exif_metadata(strip(&e));
            }
            enc.write_image(&pixels, w, h, ect).map_err(map)?;
        }
        ImageFormatKind::Png => {
            let mut enc = image::codecs::png::PngEncoder::new_with_quality(
                &mut out,
                image::codecs::png::CompressionType::Default,
                image::codecs::png::FilterType::Adaptive,
            );
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            if let Some(e) = exif {
                let _ = enc.set_exif_metadata(strip(&e));
            }
            enc.write_image(&pixels, w, h, ect).map_err(map)?;
            if let Some(d) = dpi {
                out = png_insert_phys(&out, d);
            }
        }
        ImageFormatKind::Webp => {
            let mut enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            if let Some(e) = exif {
                let _ = enc.set_exif_metadata(strip(&e));
            }
            let (pixels, ect) = if color.is_gray() {
                convert(buf, if color.has_alpha() { SourceColor::Rgba8 } else { SourceColor::Rgb8 })
            } else {
                (pixels, ect)
            };
            enc.write_image(&pixels, w, h, ect).map_err(map)?;
            notes.push(msg!("WebP 以无损方式导出", "WebP is exported losslessly"));
        }
        ImageFormatKind::Bmp => {
            let mut enc = image::codecs::bmp::BmpEncoder::new(&mut out);
            enc.encode(&pixels, w, h, ect).map_err(map)?;
            if opts.metadata.is_some_and(|m| m.has_exif() || m.has_icc()) {
                notes.push(msg!("BMP 不支持 EXIF / ICC，元数据未保留", "BMP cannot store EXIF or ICC, so metadata was not kept"));
            }
        }
        ImageFormatKind::Tiff => {
            let mut cur = Cursor::new(Vec::new());
            let mut enc = image::codecs::tiff::TiffEncoder::new(&mut cur);
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            enc.write_image(&pixels, w, h, ect).map_err(map)?;
            out = cur.into_inner();
            if opts.metadata.is_some_and(|m| m.has_exif()) {
                notes.push(msg!("TIFF 导出暂不写回 EXIF", "EXIF is not written back to TIFF output yet"));
            }
        }
    }
    Ok(Encoded { bytes: out, notes })
}

fn strip(e: &[u8]) -> Vec<u8> {
    crate::metadata::strip_exif_header(e).to_vec()
}

fn convert(buf: &ImageBuffer, color: SourceColor) -> (Vec<u8>, ExtendedColorType) {
    let n = buf.width as usize * buf.height as usize;
    match color {
        SourceColor::Rgba8 => (buf.data.clone(), ExtendedColorType::Rgba8),
        SourceColor::Rgb8 => {
            let mut v = Vec::with_capacity(n * 3);
            for p in buf.data.chunks_exact(4) {
                let a = p[3] as u32;
                if a == 255 {
                    v.extend_from_slice(&p[..3]);
                } else {
                    // 合成到白底
                    for c in 0..3 {
                        v.push(((p[c] as u32 * a + 255 * (255 - a)) / 255) as u8);
                    }
                }
            }
            (v, ExtendedColorType::Rgb8)
        }
        SourceColor::L8 => {
            let v = buf
                .data
                .chunks_exact(4)
                .map(|p| {
                    let a = p[3] as u32;
                    let l = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as u32;
                    ((l * a + 255 * (255 - a)) / 255) as u8
                })
                .collect();
            (v, ExtendedColorType::L8)
        }
        SourceColor::La8 => {
            let mut v = Vec::with_capacity(n * 2);
            for p in buf.data.chunks_exact(4) {
                v.push((0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as u8);
                v.push(p[3]);
            }
            (v, ExtendedColorType::La8)
        }
    }
}

/// 灰度源图在处理后仍是灰度吗？（修复可能引入彩色像素）
pub fn is_grayscale(buf: &ImageBuffer) -> bool {
    buf.data.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2])
}

/// 读取文件前 `n` 字节。
pub fn read_head(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    let mut f = BufReader::new(std::fs::File::open(path)?);
    let mut v = vec![0; n];
    let k = f.read(&mut v)?;
    v.truncate(k);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wmc-codec-{}", wm_common::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn png_alpha_roundtrip_is_lossless() {
        let mut b = ImageBuffer::new(8, 8);
        for (i, v) in b.data.iter_mut().enumerate() {
            *v = (i * 13 % 256) as u8;
        }
        let e = encode(&b, &EncodeOptions { format: ImageFormatKind::Png, jpeg_quality: 95, metadata: None, color: None }).unwrap();
        let p = tmp("a.png");
        std::fs::write(&p, &e.bytes).unwrap();
        let d = decode(&p, None).unwrap();
        assert_eq!(d.buffer.data, b.data);
        assert!(d.info.has_alpha);
    }

    #[test]
    fn jpeg_exif_orientation_applied_and_reset() {
        // 构造 4x2 图像并标记 Orientation=6（顺时针 90°）
        let img = image::RgbImage::from_fn(4, 2, |x, _| if x == 0 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        let mut exif = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        exif.extend_from_slice(&1u16.to_le_bytes());
        exif.extend_from_slice(&0x0112u16.to_le_bytes());
        exif.extend_from_slice(&3u16.to_le_bytes());
        exif.extend_from_slice(&1u32.to_le_bytes());
        exif.extend_from_slice(&6u16.to_le_bytes());
        exif.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut bytes = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 100);
        enc.set_exif_metadata(exif).unwrap();
        enc.write_image(img.as_raw(), 4, 2, ExtendedColorType::Rgb8).unwrap();
        let p = tmp("o.jpg");
        std::fs::write(&p, &bytes).unwrap();

        let d = decode(&p, None).unwrap();
        assert_eq!((d.buffer.width, d.buffer.height), (2, 4), "像素应已旋转");
        assert_eq!(d.info.orientation, 6);
        let out =
            encode(&d.buffer, &EncodeOptions { format: ImageFormatKind::Jpeg, jpeg_quality: 95, metadata: Some(&d.metadata), color: None })
                .unwrap();
        let p2 = tmp("o2.jpg");
        std::fs::write(&p2, &out.bytes).unwrap();
        let d2 = decode(&p2, None).unwrap();
        assert_eq!((d2.buffer.width, d2.buffer.height), (2, 4), "不能二次旋转");
        assert_eq!(d2.info.orientation, 1);
    }

    #[test]
    fn budget_rejects_huge_decode() {
        let img = image::RgbImage::new(100, 100);
        let p = tmp("big.png");
        img.save(&p).unwrap();
        let e = decode(&p, Some(1000)).err().unwrap();
        assert_eq!(e.kind, wm_core::ErrorKind::OutOfMemory);
    }

    #[test]
    fn sixteen_bit_is_rejected_explicitly() {
        let img = image::ImageBuffer::<image::Rgb<u16>, _>::new(4, 4);
        let p = tmp("16.png");
        img.save(&p).unwrap();
        let e = decode(&p, None).err().unwrap();
        assert_eq!(e.kind, wm_core::ErrorKind::UnsupportedFormat);
    }
}
