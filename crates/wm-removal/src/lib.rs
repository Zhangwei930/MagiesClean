//! # wm-removal
//!
//! 去除引擎实现（规格 §7）：`AlphaRestoreRemover`、`FastInpaintRemover`（Telea）、
//! `TextureSynthesisRemover`（PatchMatch，无 AI 模型时的复杂纹理路径）、`AiInpaintRemover`。
//! PDF 原生对象移除由 wm-pdf 通过 PDF 契约执行，不转换为 ImageBuffer。
//!
//! 所有实现都只裁剪 Mask 所在局部区域处理，并按软 Mask 合成回原图：Mask 外像素保持不变。

pub mod ai;
pub mod alpha;
pub mod patchmatch;
pub mod quality;
pub mod region;
pub mod telea;

use std::sync::Arc;
use wm_core::decision::RemovalRoute;
use wm_core::msg;
use wm_core::settings::QualityMode;
use wm_core::traits::{AiInpaintBackend, RemovalOptions, WatermarkRemover};
use wm_core::{AppError, GrayU8, ImageBuffer, PixelRect, Result, WatermarkMask};
use wm_image::ops;

/// 对 Mask 的每个局部块调用 `algo`（输入 RGB 平面与洞标记），再按软 Mask 合成。
fn inpaint_clusters(
    image: &mut ImageBuffer,
    mask: &WatermarkMask,
    pad_for: impl Fn(&PixelRect) -> u32,
    options: &RemovalOptions,
    mut algo: impl FnMut(&mut [Vec<f32>], usize, usize, &[bool]) -> Result<()>,
) -> Result<()> {
    // 先用最小 padding 聚类，再按各簇大小决定实际 padding
    for core in region::clusters(mask, 1) {
        options.cancel.check()?;
        let rect = core.pad(pad_for(&core), image.width, image.height);
        let m = mask.rasterize(&rect);
        let hole = region::hard(&m, 1);
        if !hole.iter().any(|&b| b) {
            continue;
        }
        let crop = image.crop(&rect);
        let mut planes = ops::split_rgb(&crop).map(|g| g.data).to_vec();
        algo(&mut planes, rect.width as usize, rect.height as usize, &hole)?;
        let mut patch = crop;
        for (i, px) in patch.data.chunks_exact_mut(4).enumerate() {
            for c in 0..3 {
                px[c] = planes[c][i].round().clamp(0.0, 255.0) as u8;
            }
        }
        region::composite(image, &rect, &patch, &m);
    }
    Ok(())
}

/// OpenCV Telea 等价实现：小水印、纯色与简单纹理。
pub struct FastInpaintRemover;

impl FastInpaintRemover {
    fn radius(q: QualityMode) -> f32 {
        match q {
            QualityMode::Fast => 3.0,
            QualityMode::Balanced => 5.0,
            QualityMode::Best => 7.0,
        }
    }
}

impl WatermarkRemover for FastInpaintRemover {
    fn route(&self) -> RemovalRoute {
        RemovalRoute::FastInpaint
    }
    fn remove(&self, image: &mut ImageBuffer, mask: &WatermarkMask, options: &RemovalOptions) -> Result<()> {
        let r = Self::radius(options.quality);
        inpaint_clusters(
            image,
            mask,
            |_| (r * 2.0) as u32 + 2,
            options,
            |p, w, h, hole| {
                telea::inpaint_planes(p, w, h, hole, r);
                Ok(())
            },
        )
    }
}

/// 纹理合成修复（PatchMatch）：复杂纹理、较大水印，且未安装 AI 修复模型时使用。
pub struct TextureSynthesisRemover;

impl WatermarkRemover for TextureSynthesisRemover {
    fn route(&self) -> RemovalRoute {
        RemovalRoute::TextureSynthesis
    }
    fn remove(&self, image: &mut ImageBuffer, mask: &WatermarkMask, options: &RemovalOptions) -> Result<()> {
        let params = match options.quality {
            QualityMode::Fast => patchmatch::SynthParams { iters_coarse: 5, iters_fine: 2, ..Default::default() },
            QualityMode::Balanced => patchmatch::SynthParams::default(),
            QualityMode::Best => patchmatch::SynthParams { patch: 9, iters_coarse: 10, iters_fine: 4, ..Default::default() },
        };
        let cancel = options.cancel.clone();
        inpaint_clusters(
            image,
            mask,
            |r| region::padding_for(r, 24, 0.9).min(320),
            options,
            |p, w, h, hole| patchmatch::synthesize(p, w, h, hole, params, &cancel),
        )
    }
}

/// 半透明水印反推；不稳定像素与未被 alpha 覆盖的 Mask 像素用 Telea 兜底。
pub struct AlphaRestoreRemover;

impl WatermarkRemover for AlphaRestoreRemover {
    fn route(&self) -> RemovalRoute {
        RemovalRoute::AlphaRestore
    }
    fn remove(&self, image: &mut ImageBuffer, mask: &WatermarkMask, options: &RemovalOptions) -> Result<()> {
        if options.alpha.is_empty() {
            return Err(AppError::internal(msg!(
                "缺少 alpha 估计，无法执行透明度反推",
                "No alpha estimate is available for alpha restore"
            )));
        }
        // 需要兜底修复的像素
        let mut fallback = WatermarkMask::new(format!("{}-fallback", mask.id), mask.file_id.clone(), mask.width, mask.height);
        for matte in &options.alpha {
            options.cancel.check()?;
            let rect = wm_core::buffer::clamp_rect(&matte.rect, image.width, image.height);
            let m = mask.rasterize(&rect);
            // 只在 Mask 覆盖处反推，保证 Mask 外不变
            let mut limited = matte.clone();
            for (i, a) in limited.alpha.data.iter_mut().enumerate() {
                if i < m.data.len() && m.data[i] == 0 {
                    *a = 0.0;
                }
            }
            let out = alpha::restore(image, &limited);
            // 笔画边缘（alpha 梯度大）处的反推对亚像素错位与抗锯齿非常敏感，会留下轮廓光晕：
            // 边缘带交给 Inpaint，内部保留反推结果。细笔画水印因此基本走 Inpaint。
            let mut unstable = out.unstable.clone();
            let (agx, agy) = ops::sobel(&limited.alpha);
            for y in 0..unstable.height.min(limited.alpha.height) {
                for x in 0..unstable.width.min(limited.alpha.width) {
                    let i = limited.alpha.idx(x, y);
                    let g = (agx.data[i] * agx.data[i] + agy.data[i] * agy.data[i]).sqrt();
                    if limited.alpha.data[i] > 0.01 && g > 0.04 {
                        unstable.set(x, y, 255);
                    }
                }
            }
            if unstable.count_nonzero() > 0 {
                let dil = ops::dilate(&unstable, 1.0);
                // 膨胀后仍限制在 Mask 内
                let mut d = GrayU8::new(dil.width, dil.height);
                for i in 0..d.data.len() {
                    d.data[i] = if dil.data[i] > 0 && m.data[i] > 0 { 255 } else { 0 };
                }
                fallback.regions.push(wm_core::MaskRegion::new(out.rect, d, None));
            }
        }
        // Mask 中不在任何 matte 覆盖范围内的部分
        for r in &mask.regions {
            let mut rest = r.data.clone();
            for matte in &options.alpha {
                for y in 0..r.rect.height {
                    for x in 0..r.rect.width {
                        let (gx, gy) = (r.rect.x + x, r.rect.y + y);
                        if matte.rect.contains(gx, gy) {
                            let a = matte.alpha.get(gx - matte.rect.x, gy - matte.rect.y);
                            if a > 0.01 {
                                rest.set(x, y, 0);
                            }
                        }
                    }
                }
            }
            if rest.count_nonzero() > 0 {
                fallback.regions.push(wm_core::MaskRegion::new(r.rect, rest, None));
            }
        }
        if !fallback.is_empty() {
            FastInpaintRemover.remove(image, &fallback, options)?;
        }
        Ok(())
    }
}

pub use ai::{AiInpaintRemover, PatchCache};

/// 按路由选择具体实现。AI 路由需要注入后端；`cache` 为区域修复结果缓存（可选）。
pub fn remover_for(
    route: RemovalRoute,
    ai: Option<Arc<dyn AiInpaintBackend>>,
    cache: Option<Arc<PatchCache>>,
) -> Result<Box<dyn WatermarkRemover>> {
    Ok(match route {
        RemovalRoute::FastInpaint => Box::new(FastInpaintRemover),
        RemovalRoute::TextureSynthesis => Box::new(TextureSynthesisRemover),
        RemovalRoute::AlphaRestore => Box::new(AlphaRestoreRemover),
        RemovalRoute::AiInpaint => match ai {
            Some(backend) => Box::new(AiInpaintRemover::new(backend, cache)),
            None => return Err(AppError::model(msg!("AI 修复模型未安装", "The AI inpainting model is not installed"))),
        },
        RemovalRoute::PdfObjectRemove => {
            return Err(AppError::internal(msg!(
                "PDF 原生对象应由 PDF 处理链移除",
                "Native PDF objects must be removed by the PDF pipeline"
            )))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_core::MaskRegion;

    fn mask_block(w: u32, h: u32, r: PixelRect) -> WatermarkMask {
        let mut m = WatermarkMask::new("m".into(), "f".into(), w, h);
        let mut d = GrayU8::new(r.width, r.height);
        d.data.fill(255);
        m.regions.push(MaskRegion::new(r, d, None));
        m
    }

    #[test]
    fn removers_never_touch_pixels_outside_mask() {
        let mut img = ImageBuffer::new(80, 60);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = if i % 4 == 3 { 255 } else { (i * 31 % 251) as u8 };
        }
        let rect = PixelRect::new(30, 20, 20, 10);
        let mask = mask_block(80, 60, rect);
        for route in [RemovalRoute::FastInpaint, RemovalRoute::TextureSynthesis] {
            let mut out = img.clone();
            remover_for(route, None, None).unwrap().remove(&mut out, &mask, &RemovalOptions::default()).unwrap();
            for y in 0..60 {
                for x in 0..80 {
                    if !rect.contains(x, y) {
                        assert_eq!(out.get(x, y), img.get(x, y), "{route:?} changed ({x},{y})");
                    }
                }
            }
        }
    }
}
