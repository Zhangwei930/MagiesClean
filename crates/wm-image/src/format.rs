//! 图片格式与能力矩阵（规格 §8.4）。
//!
//! 已支持的变体正常处理；不支持的变体（动画 WebP、多页 TIFF、16 位色深）
//! 给出明确原因，不静默截取首帧或降低位深。

use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use wm_core::msg;
use wm_core::{AppError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormatKind {
    Jpeg,
    Png,
    Webp,
    Bmp,
    Tiff,
}

impl ImageFormatKind {
    pub fn from_image_format(f: image::ImageFormat) -> Option<Self> {
        Some(match f {
            image::ImageFormat::Jpeg => Self::Jpeg,
            image::ImageFormat::Png => Self::Png,
            image::ImageFormat::WebP => Self::Webp,
            image::ImageFormat::Bmp => Self::Bmp,
            image::ImageFormat::Tiff => Self::Tiff,
            _ => return None,
        })
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
            Self::Bmp => "BMP",
            Self::Tiff => "TIFF",
        }
    }

    pub fn supports_alpha(&self) -> bool {
        !matches!(self, Self::Jpeg)
    }

    pub fn from_output(f: wm_core::settings::OutputFormat, input: ImageFormatKind) -> ImageFormatKind {
        use wm_core::settings::OutputFormat as O;
        match f {
            O::Same => input,
            O::Jpeg => Self::Jpeg,
            O::Png => Self::Png,
            O::Webp => Self::Webp,
            O::Bmp => Self::Bmp,
            O::Tiff => Self::Tiff,
        }
    }
}

/// 按文件内容识别格式（不信任扩展名）。
pub fn sniff(path: &Path) -> Result<ImageFormatKind> {
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    match reader.format().and_then(ImageFormatKind::from_image_format) {
        Some(f) => Ok(f),
        None => Err(AppError::unsupported(msg!("不支持的图片格式", "Unsupported image format"))),
    }
}

/// 检查格式子类型是否在 V1 能力范围内。
pub fn check_capability(path: &Path, fmt: ImageFormatKind) -> Result<()> {
    match fmt {
        ImageFormatKind::Webp => {
            let f = std::io::BufReader::new(std::fs::File::open(path)?);
            let dec = image::codecs::webp::WebPDecoder::new(f)
                .map_err(|e| AppError::decode(msg!("无法读取 WebP 图片", "Could not read the WebP image")).with_detail(e))?;
            if dec.has_animation() {
                return Err(AppError::unsupported(msg!(
                    "动画 WebP 暂不支持（为避免只处理首帧，未做处理）",
                    "Animated WebP is not supported yet (left untouched to avoid processing only the first frame)"
                )));
            }
        }
        ImageFormatKind::Tiff => {
            if tiff_page_count(path)? > 1 {
                return Err(AppError::unsupported(msg!(
                    "多页 TIFF 暂不支持（为避免只处理首页，未做处理）",
                    "Multi-page TIFF is not supported yet (left untouched to avoid processing only the first page)"
                )));
            }
        }
        _ => {}
    }
    Ok(())
}

/// 读取 TIFF IFD 链长度（支持 Classic TIFF 与 BigTIFF）。上限 2 以快速判断多页。
pub fn tiff_page_count(path: &Path) -> Result<usize> {
    let mut f = std::fs::File::open(path)?;
    let mut hdr = [0u8; 16];
    f.read_exact(&mut hdr[..8]).map_err(|_| AppError::decode(msg!("TIFF 文件头损坏", "The TIFF header is damaged")))?;
    let le = match &hdr[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Err(AppError::decode(msg!("TIFF 文件头损坏", "The TIFF header is damaged"))),
    };
    let u16_at = |b: &[u8]| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) };
    let u32_at = |b: &[u8]| {
        let a = [b[0], b[1], b[2], b[3]];
        if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    };
    let u64_at = |b: &[u8]| {
        let a: [u8; 8] = b[..8].try_into().unwrap();
        if le {
            u64::from_le_bytes(a)
        } else {
            u64::from_be_bytes(a)
        }
    };
    let magic = u16_at(&hdr[2..4]);
    let big = match magic {
        42 => false,
        43 => true,
        _ => return Err(AppError::decode(msg!("TIFF 文件头损坏", "The TIFF header is damaged"))),
    };
    let mut offset = if big {
        f.read_exact(&mut hdr[8..16]).map_err(|_| AppError::decode(msg!("TIFF 文件头损坏", "The TIFF header is damaged")))?;
        u64_at(&hdr[8..16])
    } else {
        u32_at(&hdr[4..8]) as u64
    };
    let mut pages = 0;
    while offset != 0 && pages < 2 {
        pages += 1;
        f.seek(SeekFrom::Start(offset))?;
        let (count, entry, next_len) = if big {
            let mut b = [0u8; 8];
            f.read_exact(&mut b)?;
            (u64_at(&b), 20u64, 8usize)
        } else {
            let mut b = [0u8; 2];
            f.read_exact(&mut b)?;
            (u16_at(&b) as u64, 12u64, 4usize)
        };
        f.seek(SeekFrom::Current((count * entry) as i64))?;
        let mut nb = [0u8; 8];
        if f.read_exact(&mut nb[..next_len]).is_err() {
            break;
        }
        offset = if big { u64_at(&nb) } else { u32_at(&nb[..4]) as u64 };
    }
    Ok(pages)
}
