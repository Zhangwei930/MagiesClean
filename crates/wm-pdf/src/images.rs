//! 嵌入图片（Image XObject）的提取与替换（规格 §9：扫描型 PDF 优先替换原始 Image XObject）。
//!
//! V1 支持：DCTDecode（JPEG，Gray/RGB）、FlateDecode / 无压缩的 8 位 DeviceGray / DeviceRGB /
//! ICCBased(N=1|3)。其它编码（JBIG2、CCITT、JPX、CMYK、Indexed、ImageMask）显式报告不支持。

use flate2::write::ZlibEncoder;
use lopdf::{Document, Object, ObjectId};
use std::io::Write;
use wm_core::msg;
use wm_core::{AppError, ImageBuffer, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageEncoding {
    Jpeg { gray: bool, quality: u8 },
    Raw { channels: u8 },
}

fn filters(dict: &lopdf::Dictionary) -> Vec<Vec<u8>> {
    match dict.get(b"Filter") {
        Ok(Object::Name(n)) => vec![n.clone()],
        Ok(Object::Array(a)) => a.iter().filter_map(|o| o.as_name().ok().map(|n| n.to_vec())).collect(),
        _ => Vec::new(),
    }
}

/// 颜色通道数（仅支持的颜色空间）。
fn channels(doc: &Document, dict: &lopdf::Dictionary) -> Option<u8> {
    let cs = dict.get(b"ColorSpace").ok()?;
    let cs = doc.dereference(cs).ok()?.1;
    match cs {
        Object::Name(n) if n == b"DeviceRGB" || n == b"CalRGB" => Some(3),
        Object::Name(n) if n == b"DeviceGray" || n == b"CalGray" => Some(1),
        Object::Array(a) if a.first().and_then(|o| o.as_name().ok()) == Some(b"ICCBased") => {
            let s = a.get(1).and_then(|o| doc.dereference(o).ok()).and_then(|(_, o)| o.as_stream().ok())?;
            match s.dict.get(b"N").ok().and_then(|o| o.as_i64().ok()) {
                Some(1) => Some(1),
                Some(3) => Some(3),
                _ => None,
            }
        }
        _ => None,
    }
}

/// 返回（编码描述, 是否支持）。
pub fn describe(doc: &Document, id: ObjectId) -> (String, bool) {
    let Ok(Object::Stream(s)) = doc.get_object(id) else { return ("Unknown".into(), false) };
    let f = filters(&s.dict);
    let name = if f.is_empty() {
        "None".to_string()
    } else {
        f.iter().map(|x| String::from_utf8_lossy(x).to_string()).collect::<Vec<_>>().join("+")
    };
    let image_mask = s.dict.get(b"ImageMask").ok().and_then(|o| o.as_bool().ok()).unwrap_or(false);
    let bpc = s.dict.get(b"BitsPerComponent").ok().and_then(|o| o.as_i64().ok()).unwrap_or(8);
    let has_decode = s.dict.get(b"Decode").is_ok();
    let supported = !image_mask
        && !has_decode
        && bpc == 8
        && channels(doc, &s.dict).is_some()
        && (f.is_empty() || f == [b"DCTDecode".to_vec()] || f == [b"FlateDecode".to_vec()]);
    (name, supported)
}

/// 提取嵌入图片为 RGBA。
pub fn extract(doc: &Document, id: ObjectId) -> Result<(ImageBuffer, ImageEncoding)> {
    let (_, supported) = describe(doc, id);
    if !supported {
        return Err(AppError::unsupported(msg!(
            "该嵌入图片的编码暂不支持（例如 JBIG2 / CCITT / JPX / CMYK）",
            "This embedded image encoding is not supported yet (e.g. JBIG2, CCITT, JPX, CMYK)"
        )));
    }
    let Ok(Object::Stream(s)) = doc.get_object(id) else {
        return Err(AppError::pdf(msg!("找不到嵌入图片对象", "Embedded image object not found")));
    };
    let w = s.dict.get(b"Width").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0) as u32;
    let h = s.dict.get(b"Height").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0) as u32;
    let ch = channels(doc, &s.dict).unwrap_or(3);
    let f = filters(&s.dict);
    if f == [b"DCTDecode".to_vec()] {
        let d = wm_image::decode_bytes(&s.content, s.content.len() as u64)?;
        if d.buffer.width != w || d.buffer.height != h {
            return Err(AppError::pdf(msg!("嵌入 JPEG 尺寸与声明不一致", "Embedded JPEG size does not match its declaration")));
        }
        let q = d.metadata.jpeg_quality.unwrap_or(90).clamp(80, 98);
        return Ok((d.buffer, ImageEncoding::Jpeg { gray: ch == 1, quality: q }));
    }
    let raw = if f.is_empty() {
        s.content.clone()
    } else {
        s.decompressed_content()
            .map_err(|e| AppError::pdf(msg!("嵌入图片解压失败", "Failed to decompress the embedded image")).with_detail(e))?
    };
    let n = (w * h) as usize;
    if raw.len() < n * ch as usize {
        return Err(AppError::pdf(msg!("嵌入图片数据长度不足", "Embedded image data is truncated")));
    }
    let mut img = ImageBuffer::new(w, h);
    for i in 0..n {
        let px = if ch == 1 { [raw[i], raw[i], raw[i], 255] } else { [raw[i * 3], raw[i * 3 + 1], raw[i * 3 + 2], 255] };
        img.data[i * 4..i * 4 + 4].copy_from_slice(&px);
    }
    Ok((img, ImageEncoding::Raw { channels: ch }))
}

/// 用处理后的像素替换嵌入图片（尺寸必须不变）。
pub fn replace(doc: &mut Document, id: ObjectId, img: &ImageBuffer, enc: ImageEncoding) -> Result<()> {
    let Ok(Object::Stream(s)) = doc.get_object_mut(id) else {
        return Err(AppError::pdf(msg!("找不到嵌入图片对象", "Embedded image object not found")));
    };
    let w = s.dict.get(b"Width").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0) as u32;
    let h = s.dict.get(b"Height").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0) as u32;
    if (w, h) != (img.width, img.height) {
        return Err(AppError::pdf(msg!("替换图片尺寸与原图不一致", "Replacement image size does not match the original")));
    }
    match enc {
        ImageEncoding::Jpeg { gray, quality } => {
            let e = wm_image::encode(
                img,
                &wm_image::EncodeOptions {
                    format: wm_image::ImageFormatKind::Jpeg,
                    jpeg_quality: quality,
                    metadata: None,
                    color: Some(if gray { wm_image::SourceColor::L8 } else { wm_image::SourceColor::Rgb8 }),
                },
            )?;
            s.dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
            s.dict.remove(b"DecodeParms");
            s.set_content(e.bytes);
        }
        ImageEncoding::Raw { channels } => {
            let mut raw = Vec::with_capacity((w * h) as usize * channels as usize);
            for p in img.data.chunks_exact(4) {
                if channels == 1 {
                    raw.push(((p[0] as u32 + p[1] as u32 + p[2] as u32) / 3) as u8);
                } else {
                    raw.extend_from_slice(&p[..3]);
                }
            }
            let mut z = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(&raw).map_err(|e| AppError::encode(msg!("图片压缩失败", "Image compression failed")).with_detail(e))?;
            let bytes = z.finish().map_err(|e| AppError::encode(msg!("图片压缩失败", "Image compression failed")).with_detail(e))?;
            s.dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
            s.dict.remove(b"DecodeParms");
            s.set_content(bytes);
        }
    }
    Ok(())
}
