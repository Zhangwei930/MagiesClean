//! # wm-image
//!
//! Decode、Encode、Metadata、Resize、Mask、Coordinate（规格 §3 模块职责）。

pub mod codec;
pub mod format;
pub mod maskops;
pub mod metadata;
pub mod ops;
pub mod output;
pub mod synth;

pub use codec::{decode, decode_bytes, encode, probe, DecodedImage, EncodeOptions, Encoded, ImageInfo};
pub use format::ImageFormatKind;
pub use metadata::{ImageMetadata, SourceColor};

use std::path::Path;
use wm_core::{AppError, ImageBuffer, Result};

/// 检测阶段使用的缩略图最大边长（规格 §8.1）。
pub const DETECTION_MAX_SIDE: u32 = 2048;
/// 界面预览图最大边长。
pub const PREVIEW_MAX_SIDE: u32 = 2560;

/// 写入界面预览 JPEG（不含元数据）。返回预览尺寸。
pub fn write_preview_jpeg(img: &ImageBuffer, path: &Path, max_side: u32) -> Result<(u32, u32)> {
    let (thumb, _) = ops::thumbnail(img, max_side);
    let enc = encode(&thumb, &EncodeOptions { format: ImageFormatKind::Jpeg, jpeg_quality: 88, metadata: None, color: None })?;
    write_file(path, &enc.bytes)?;
    Ok((thumb.width, thumb.height))
}

/// 写入无损 PNG（用于缓存结果与 Mask 叠加层）。
pub fn write_png(img: &ImageBuffer, path: &Path) -> Result<()> {
    let enc =
        encode(img, &EncodeOptions { format: ImageFormatKind::Png, jpeg_quality: 95, metadata: None, color: Some(SourceColor::Rgba8) })?;
    write_file(path, &enc.bytes)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", wm_common::new_id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        AppError::from(e)
    })
}
