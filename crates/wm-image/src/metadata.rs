//! 元数据：EXIF、ICC Profile、DPI、Orientation 与 JPEG 质量估计（规格 §8 表）。

use serde::{Deserialize, Serialize};

/// 原图的通道布局；导出时尽量保持（例如灰度 PNG 仍输出灰度）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceColor {
    L8,
    La8,
    Rgb8,
    Rgba8,
}

impl SourceColor {
    pub fn has_alpha(&self) -> bool {
        matches!(self, SourceColor::La8 | SourceColor::Rgba8)
    }
    pub fn is_gray(&self) -> bool {
        matches!(self, SourceColor::L8 | SourceColor::La8)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageMetadata {
    #[serde(skip)]
    pub exif: Option<Vec<u8>>,
    #[serde(skip)]
    pub icc: Option<Vec<u8>>,
    pub dpi: Option<(f32, f32)>,
    /// 原始 EXIF Orientation（1..8）。解码后像素已按其旋转。
    pub orientation: u8,
    pub color: SourceColor,
    /// 估计的原 JPEG 质量（仅 JPEG 输入）。
    pub jpeg_quality: Option<u8>,
}

impl ImageMetadata {
    pub fn has_exif(&self) -> bool {
        self.exif.as_ref().is_some_and(|e| !e.is_empty())
    }
    pub fn has_icc(&self) -> bool {
        self.icc.as_ref().is_some_and(|e| !e.is_empty())
    }
}

const EXIF_HEADER: &[u8] = b"Exif\0\0";

/// 去掉可能存在的 `Exif\0\0` 前缀，得到 TIFF 结构数据。
pub fn strip_exif_header(exif: &[u8]) -> &[u8] {
    exif.strip_prefix(EXIF_HEADER).unwrap_or(exif)
}

/// 像素已按 Orientation 旋转后，把 EXIF 中 IFD0 的 Orientation(0x0112) 改为 1，
/// 避免查看器二次旋转。返回是否找到并修改了标签。
pub fn patch_exif_orientation(exif: &mut [u8]) -> bool {
    let off = if exif.starts_with(EXIF_HEADER) { EXIF_HEADER.len() } else { 0 };
    let t = &mut exif[off..];
    if t.len() < 8 {
        return false;
    }
    let le = match &t[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return false,
    };
    let rd16 = |b: &[u8], i: usize| if le { u16::from_le_bytes([b[i], b[i + 1]]) } else { u16::from_be_bytes([b[i], b[i + 1]]) };
    let rd32 = |b: &[u8], i: usize| {
        let a = [b[i], b[i + 1], b[i + 2], b[i + 3]];
        if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    };
    let ifd = rd32(t, 4) as usize;
    if ifd + 2 > t.len() {
        return false;
    }
    let n = rd16(t, ifd) as usize;
    for k in 0..n {
        let e = ifd + 2 + k * 12;
        if e + 12 > t.len() {
            return false;
        }
        if rd16(t, e) == 0x0112 {
            // SHORT 类型，值位于 value 字段的前两个字节
            let v: [u8; 2] = if le { 1u16.to_le_bytes() } else { 1u16.to_be_bytes() };
            t[e + 8] = v[0];
            t[e + 9] = v[1];
            return true;
        }
    }
    false
}

/// 从 EXIF 读取 XResolution / YResolution（单位英寸时返回 DPI）。
pub fn dpi_from_exif(exif: &[u8]) -> Option<(f32, f32)> {
    let data = strip_exif_header(exif).to_vec();
    let ex = exif::Reader::new().read_raw(data).ok()?;
    let unit = ex.get_field(exif::Tag::ResolutionUnit, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)).unwrap_or(2);
    let rational = |tag| {
        ex.get_field(tag, exif::In::PRIMARY).and_then(|f| match &f.value {
            exif::Value::Rational(v) if !v.is_empty() && v[0].denom != 0 => Some(v[0].to_f32()),
            _ => None,
        })
    };
    let (x, y) = (rational(exif::Tag::XResolution)?, rational(exif::Tag::YResolution)?);
    match unit {
        2 => Some((x, y)),
        3 => Some((x * 2.54, y * 2.54)),
        _ => None,
    }
}

/// JPEG：读取 JFIF APP0 像素密度。
pub fn dpi_from_jfif(bytes: &[u8]) -> Option<(f32, f32)> {
    let mut i = 2;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if marker == 0xE0 && i + 4 + 12 <= bytes.len() && &bytes[i + 4..i + 9] == b"JFIF\0" {
            let units = bytes[i + 11];
            let xd = u16::from_be_bytes([bytes[i + 12], bytes[i + 13]]) as f32;
            let yd = u16::from_be_bytes([bytes[i + 14], bytes[i + 15]]) as f32;
            return match units {
                1 => Some((xd, yd)),
                2 => Some((xd * 2.54, yd * 2.54)),
                _ => None,
            };
        }
        if marker == 0xDA {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// PNG：读取 pHYs 块。
pub fn dpi_from_png(bytes: &[u8]) -> Option<(f32, f32)> {
    let mut i = 8;
    while i + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().ok()?) as usize;
        let ty = &bytes[i + 4..i + 8];
        if ty == b"pHYs" && len == 9 && i + 8 + 9 <= bytes.len() {
            let d = &bytes[i + 8..];
            let x = u32::from_be_bytes(d[0..4].try_into().ok()?) as f32;
            let y = u32::from_be_bytes(d[4..8].try_into().ok()?) as f32;
            return (d[8] == 1).then_some((x * 0.0254, y * 0.0254));
        }
        if ty == b"IDAT" {
            return None;
        }
        i += 12 + len;
    }
    None
}

/// 在 PNG 的 IHDR 之后插入 pHYs 块。
pub fn png_insert_phys(png: &[u8], dpi: (f32, f32)) -> Vec<u8> {
    if png.len() < 33 || &png[12..16] != b"IHDR" {
        return png.to_vec();
    }
    let ihdr_end = 8 + 12 + 13;
    let mut chunk = Vec::with_capacity(21);
    chunk.extend_from_slice(&9u32.to_be_bytes());
    let mut body = Vec::with_capacity(13);
    body.extend_from_slice(b"pHYs");
    body.extend_from_slice(&((dpi.0 / 0.0254).round() as u32).to_be_bytes());
    body.extend_from_slice(&((dpi.1 / 0.0254).round() as u32).to_be_bytes());
    body.push(1);
    chunk.extend_from_slice(&body);
    chunk.extend_from_slice(&crc32(&body).to_be_bytes());
    let mut out = Vec::with_capacity(png.len() + chunk.len());
    out.extend_from_slice(&png[..ihdr_end]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[ihdr_end..]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// IJG 标准亮度量化表（顺序无关，仅用于求和比较）。
const STD_LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22,
    37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// 根据 DQT 亮度表估计 JPEG 质量（IJG 标度）。用于“保留质量”编码策略，并非逐字节无损。
pub fn estimate_jpeg_quality(bytes: &[u8]) -> Option<u8> {
    let table = find_luma_dqt(bytes)?;
    let target: u32 = table.iter().map(|&v| v as u32).sum();
    let mut best = (u32::MAX, 75u8);
    for q in 1..=100u32 {
        let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
        let s: u32 = STD_LUMA.iter().map(|&v| ((v as u32 * scale + 50) / 100).clamp(1, 255)).sum();
        let d = s.abs_diff(target);
        if d < best.0 {
            best = (d, q as u8);
        }
    }
    Some(best.1)
}

fn find_luma_dqt(b: &[u8]) -> Option<Vec<u16>> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if marker == 0xDB {
            let mut p = i + 4;
            let end = (i + 2 + len).min(b.len());
            while p < end {
                let pq = b[p] >> 4;
                let tq = b[p] & 0x0F;
                p += 1;
                let n = if pq == 0 { 64 } else { 128 };
                if p + n > end {
                    return None;
                }
                if tq == 0 {
                    return Some(if pq == 0 {
                        b[p..p + 64].iter().map(|&v| v as u16).collect()
                    } else {
                        b[p..p + 128].chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
                    });
                }
                p += n;
            }
        }
        if marker == 0xDA {
            return None;
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiff_exif_with_orientation(v: u16) -> Vec<u8> {
        // II*\0, IFD at 8, 1 entry: 0x0112 SHORT count 1 value v
        let mut b = b"Exif\0\0II\x2a\x00\x08\x00\x00\x00".to_vec();
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&0x0112u16.to_le_bytes());
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&v.to_le_bytes());
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&0u32.to_le_bytes());
        b
    }

    #[test]
    fn patch_orientation_sets_one() {
        let mut e = tiff_exif_with_orientation(6);
        assert!(patch_exif_orientation(&mut e));
        let r = exif::Reader::new().read_raw(strip_exif_header(&e).to_vec()).unwrap();
        let o = r.get_field(exif::Tag::Orientation, exif::In::PRIMARY).unwrap().value.get_uint(0);
        assert_eq!(o, Some(1));
    }

    #[test]
    fn jpeg_quality_estimate_roundtrip() {
        for q in [60u8, 85, 95] {
            let img = image::RgbImage::from_fn(32, 32, |x, y| image::Rgb([(x * 7) as u8, (y * 5) as u8, 100]));
            let mut buf = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, q).encode_image(&img).unwrap();
            let est = estimate_jpeg_quality(&buf).unwrap();
            assert!((est as i32 - q as i32).abs() <= 2, "q={q} est={est}");
        }
    }

    #[test]
    fn png_phys_roundtrip() {
        let img = image::RgbImage::new(4, 4);
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf).write_image(img.as_raw(), 4, 4, image::ExtendedColorType::Rgb8).unwrap();
        let with = png_insert_phys(&buf, (300.0, 300.0));
        let dpi = dpi_from_png(&with).unwrap();
        assert!((dpi.0 - 300.0).abs() < 0.5);
        image::load_from_memory(&with).unwrap();
    }

    use image::ImageEncoder;
}
