//! # wm-segmentation
//!
//! WatermarkSegmenter（规格 §5）：候选区域 → Binary / Soft Mask，只覆盖真实水印像素。
//!
//! Mask 后处理顺序（图 5-1）：
//! Raw Mask → Threshold + Noise Removal → Edge Refinement + Small Component Removal
//! → Dilation + Feathering → Final Mask。参数按分辨率换算。
//!
//! `ClassicSegmenter` 基于逐通道形态学顶帽（叠加在背景上的细笔画结构）：
//! - 无提示：在候选框内做 Otsu + 滞后阈值，边缘由弱响应连通扩展；
//! - 有提示（批次模板 / 重复图案 / 模型输出）：以提示为先验限制范围，在原图分辨率下精修。
//! 模型分割器（ONNX）在 wm-ai 中以同一 trait 实现。

use wm_core::settings::MaskParams;
use wm_core::traits::{MaskHint, SegmentContext, WatermarkSegmenter};
use wm_core::{BoundingBox, GrayF32, GrayU8, MaskRegion, PixelRect, Result, WatermarkCandidate, WatermarkType};
use wm_image::ops;

/// 最低可分割对比度（0..255）：低于此值视为背景噪声。
const MIN_CONTRAST: f32 = 7.0;

pub struct ClassicSegmenter;

impl WatermarkSegmenter for ClassicSegmenter {
    fn name(&self) -> &'static str {
        "classic-tophat"
    }

    fn segment(&self, candidate: &WatermarkCandidate, ctx: &SegmentContext) -> Result<Option<MaskRegion>> {
        let img = ctx.image;
        let (cw, ch) = (img.width, img.height);
        if cw < 3 || ch < 3 {
            return Ok(None);
        }
        let scale = MaskParams::scale_for(ctx.full_width.max(ctx.full_height));

        // 候选框在裁剪坐标下的位置
        let bbox = local_rect(&candidate.bbox, ctx);
        let short = bbox.width.min(bbox.height).max(1) as f32;

        // 先验：提示 → 裁剪坐标的软概率图
        let prior = ctx.hint.map(|h| hint_to_crop(h, ctx));
        if candidate.watermark_type == WatermarkType::InfoStamp {
            if let Some(p) = &prior {
                return Ok(segment_info_stamp(candidate, ctx, p, scale));
            }
        }

        // 笔画尺度：由提示的细结构或候选框短边估计
        let stroke_r = match &prior {
            Some(p) => estimate_stroke_radius(p).max(2),
            None => ((short / 5.0).round() as u32).clamp(2, 16),
        };
        let stroke_r = stroke_r.min(24);

        let (tb, td) = ops::tophat_rgb(img, stroke_r);

        // 感兴趣区域
        let roi: GrayU8 = match &prior {
            Some(p) => {
                let bin = threshold_f(p, 0.2);
                ops::dilate(&bin, (3.0 * scale).max(2.0) + stroke_r as f32 * 0.5)
            }
            None => {
                let pad = (short * 0.08).max(2.0) as u32;
                let r = bbox.pad(pad, cw, ch);
                let mut m = GrayU8::new(cw, ch);
                for y in r.y..r.bottom() {
                    for x in r.x..r.right() {
                        m.set(x, y, 255);
                    }
                }
                m
            }
        };

        // 极性：只取主导极性。亮文字的笔画间隙在暗顶帽中同样有强响应，
        // 若同时取两种极性会把字母间隙误判为水印。
        let bright = match polarity_from_ring(img, &roi) {
            Some(b) => b,
            None => {
                let (eb, ed) = energy_in(&tb, &td, &roi);
                eb >= ed
            }
        };
        let src = if bright { &tb } else { &td };
        let mut resp = GrayF32::new(cw, ch);
        for i in 0..resp.data.len() {
            resp.data[i] = if roi.data[i] > 0 { src.data[i] } else { 0.0 };
        }

        // 阈值 + 滞后边缘扩展
        let vals: Vec<f32> = resp.data.iter().zip(&roi.data).filter(|(_, &r)| r > 0).map(|(&v, _)| v).collect();
        let mut classic = GrayU8::new(cw, ch);
        if !vals.is_empty() {
            let t_hi = ops::otsu(&vals).max(MIN_CONTRAST * 1.6);
            let t_lo = (t_hi * 0.6).max(MIN_CONTRAST);
            classic = hysteresis(&resp, t_lo, t_hi);
        }

        // 与先验融合
        let mut mask = match &prior {
            Some(p) => {
                let core = threshold_f(p, ctx.params.mask_threshold.max(0.3));
                let core_n = core.count_nonzero();
                let classic_n = classic.count_nonzero();
                if core_n > 0 && classic_n < core_n / 3 {
                    // 局部对比度不足（例如很淡的水印）：以先验为主
                    core
                } else {
                    let mut m = classic;
                    for i in 0..m.data.len() {
                        if roi.data[i] == 0 {
                            m.data[i] = 0;
                        }
                    }
                    m
                }
            }
            None => classic,
        };

        // 小连通域清理（按分辨率换算，避免丢失细文字）
        let min_area = (ctx.params.min_component_size * scale * scale).round().max(2.0) as u32;
        mask = ops::remove_small_components(&mask, min_area);
        if mask.count_nonzero() == 0 {
            return Ok(None);
        }
        // 候选框内的覆盖率过高通常意味着把背景块当成了水印（Bounding Box 不能直接作为 Mask）
        if prior.is_none() {
            let inside = count_in_rect(&mask, &bbox);
            if inside as f32 > 0.85 * bbox.area() as f32 && bbox.area() > 400 {
                tracing::debug!(candidate = %candidate.id, "mask covers almost whole bbox, rejecting as block fill");
                return Ok(None);
            }
        }
        // 膨胀 + 羽化
        let dil = ctx.params.mask_dilation * scale;
        if dil >= 0.25 {
            mask = ops::dilate(&mask, dil.max(1.0));
        }
        mask = ops::feather(&mask, (ctx.params.mask_feather * scale).max(0.0));

        Ok(Some(MaskRegion::new(ctx.crop, mask, Some(candidate.id.clone()))))
    }
}

/// 信息块（时间、地点）：白字、阴影、彩色标签与分隔条是一个整体，两种极性都要覆盖。
/// 先验已包含这些元素；在原图分辨率下补齐先验附近的强笔画边缘，再整体膨胀。
fn segment_info_stamp(candidate: &WatermarkCandidate, ctx: &SegmentContext, prior: &GrayF32, scale: f32) -> Option<MaskRegion> {
    let mut mask = threshold_f(prior, 0.3);
    if mask.count_nonzero() == 0 {
        return None;
    }
    let roi = ops::dilate(&mask, (3.0 * scale).max(2.0));
    let (tb, td) = ops::tophat_rgb(ctx.image, estimate_stroke_radius(prior).max(2));
    for i in 0..mask.data.len() {
        if roi.data[i] > 0 && (tb.data[i] > 40.0 || td.data[i] > 40.0) {
            mask.data[i] = 255;
        }
    }
    let min_area = (ctx.params.min_component_size * scale * scale).round().max(2.0) as u32;
    mask = ops::remove_small_components(&mask, min_area);
    if mask.count_nonzero() == 0 {
        return None;
    }
    // 比普通文字多膨胀约 1 px（按分辨率换算），盖住阴影外缘
    mask = ops::dilate(&mask, (ctx.params.mask_dilation * scale + scale).max(1.0));
    mask = ops::feather(&mask, (ctx.params.mask_feather * scale).max(0.0));
    Some(MaskRegion::new(ctx.crop, mask, Some(candidate.id.clone())))
}

/// 候选框（归一化）→ 裁剪坐标像素矩形。
fn local_rect(b: &BoundingBox, ctx: &SegmentContext) -> PixelRect {
    let px = b.to_pixels(ctx.full_width, ctx.full_height);
    let shifted = BoundingBox::new(px.x - ctx.crop.x as f32, px.y - ctx.crop.y as f32, px.width, px.height);
    shifted.to_pixel_rect(ctx.crop.width, ctx.crop.height)
}

/// 把提示（归一化 bbox + 软图）采样到裁剪坐标。
fn hint_to_crop(h: &MaskHint, ctx: &SegmentContext) -> GrayF32 {
    let mut out = GrayF32::new(ctx.crop.width, ctx.crop.height);
    let px = h.bbox.to_pixels(ctx.full_width, ctx.full_height);
    if px.width < 1.0 || px.height < 1.0 || h.data.width == 0 {
        return out;
    }
    let src = GrayF32 { width: h.data.width, height: h.data.height, data: h.data.data.iter().map(|&v| v as f32 / 255.0).collect() };
    for y in 0..ctx.crop.height {
        let gy = (ctx.crop.y + y) as f32 + 0.5;
        let v = (gy - px.y) / px.height * src.height as f32 - 0.5;
        if v < -0.5 || v > src.height as f32 - 0.5 {
            continue;
        }
        for x in 0..ctx.crop.width {
            let gx = (ctx.crop.x + x) as f32 + 0.5;
            let u = (gx - px.x) / px.width * src.width as f32 - 0.5;
            if u < -0.5 || u > src.width as f32 - 0.5 {
                continue;
            }
            out.set(x, y, bilinear(&src, u, v));
        }
    }
    out
}

fn bilinear(g: &GrayF32, u: f32, v: f32) -> f32 {
    let x0 = u.floor() as i64;
    let y0 = v.floor() as i64;
    let (tx, ty) = (u - x0 as f32, v - y0 as f32);
    let p = |x: i64, y: i64| g.get_clamped(x, y);
    (p(x0, y0) * (1.0 - tx) + p(x0 + 1, y0) * tx) * (1.0 - ty) + (p(x0, y0 + 1) * (1.0 - tx) + p(x0 + 1, y0 + 1) * tx) * ty
}

fn threshold_f(g: &GrayF32, t: f32) -> GrayU8 {
    GrayU8 { width: g.width, height: g.height, data: g.data.iter().map(|&v| if v >= t { 255 } else { 0 }).collect() }
}

/// 由先验估计笔画半径：内部像素到边界距离的高分位数。
fn estimate_stroke_radius(p: &GrayF32) -> u32 {
    let bin = threshold_f(p, 0.5);
    if bin.count_nonzero() == 0 {
        return 3;
    }
    let inv = GrayU8 { width: bin.width, height: bin.height, data: bin.data.iter().map(|&v| if v > 0 { 0 } else { 255 }).collect() };
    let d = ops::distance_to_nonzero(&inv);
    let inside: Vec<f32> = d.data.iter().zip(&bin.data).filter(|(_, &b)| b > 0).map(|(&v, _)| v).collect();
    (ops::percentile(&inside, 0.9) * 1.6 + 1.0).round().clamp(2.0, 24.0) as u32
}

/// 用 ROI 外环带估计本地背景分布：统计 ROI 内明显亮于背景 P95 / 暗于背景 P5 的像素数。
/// 字母间隙本身是背景值，不会被计入，因此能区分“白字”与“字间暗隙”。
fn polarity_from_ring(img: &wm_core::ImageBuffer, roi: &GrayU8) -> Option<bool> {
    let d = ops::distance_to_nonzero(roi);
    let luma = img.to_luma_f32();
    let ring: Vec<f32> = d.data.iter().zip(&luma.data).filter(|(&dv, _)| dv > 0.0 && dv <= 24.0).map(|(_, &v)| v).collect();
    if ring.len() < 64 {
        return None;
    }
    let (p5, p95) = (ops::percentile(&ring, 0.05), ops::percentile(&ring, 0.95));
    let (mut nb, mut nd) = (0usize, 0usize);
    for (i, &r) in roi.data.iter().enumerate() {
        if r == 0 {
            continue;
        }
        let v = luma.data[i];
        if v > p95 + MIN_CONTRAST {
            nb += 1;
        } else if v < p5 - MIN_CONTRAST {
            nd += 1;
        }
    }
    if nb + nd < 16 {
        return None;
    }
    Some(nb >= nd)
}

fn energy_in(a: &GrayF32, b: &GrayF32, roi: &GrayU8) -> (f32, f32) {
    let (mut ea, mut eb) = (0.0f64, 0.0f64);
    for i in 0..roi.data.len() {
        if roi.data[i] > 0 {
            ea += a.data[i] as f64;
            eb += b.data[i] as f64;
        }
    }
    (ea as f32, eb as f32)
}

/// 滞后阈值：保留与强响应相连的弱响应像素。
fn hysteresis(resp: &GrayF32, lo: f32, hi: f32) -> GrayU8 {
    let weak = GrayU8 { width: resp.width, height: resp.height, data: resp.data.iter().map(|&v| if v >= lo { 255 } else { 0 }).collect() };
    let (labels, comps) = ops::connected_components(&weak);
    let mut strong_label = vec![false; comps.len() + 1];
    for (i, &l) in labels.iter().enumerate() {
        if l > 0 && resp.data[i] >= hi {
            strong_label[l as usize] = true;
        }
    }
    GrayU8 {
        width: resp.width,
        height: resp.height,
        data: labels.iter().map(|&l| if l > 0 && strong_label[l as usize] { 255 } else { 0 }).collect(),
    }
}

fn count_in_rect(m: &GrayU8, r: &PixelRect) -> usize {
    let mut n = 0;
    for y in r.y..r.bottom().min(m.height) {
        for x in r.x..r.right().min(m.width) {
            if m.get(x, y) > 0 {
                n += 1;
            }
        }
    }
    n
}

/// 分割质量评估（IoU / Dice），用于 benchmark 与 Golden 测试。
pub fn mask_iou(pred: &GrayU8, truth: &GrayU8) -> (f32, f32) {
    let (mut inter, mut p, mut t) = (0usize, 0usize, 0usize);
    for (a, b) in pred.data.iter().zip(&truth.data) {
        let (a, b) = (*a >= 128, *b >= 128);
        inter += (a && b) as usize;
        p += a as usize;
        t += b as usize;
    }
    let union = p + t - inter;
    let iou = if union == 0 { 1.0 } else { inter as f32 / union as f32 };
    let dice = if p + t == 0 { 1.0 } else { 2.0 * inter as f32 / (p + t) as f32 };
    (iou, dice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_core::{DetectorSource, WatermarkType};
    use wm_image::synth;

    fn candidate(b: BoundingBox) -> WatermarkCandidate {
        WatermarkCandidate::new("c1".into(), WatermarkType::Text, 0.9, b, DetectorSource::OverlayHeuristic)
    }

    #[test]
    fn segments_semi_transparent_text_not_whole_box() {
        let (w, h) = (640u32, 420u32);
        let mut img = synth::photo_like(w, h, 11);
        let mut truth = synth::Overlay::new(w, h);
        let a = synth::render_text("MYSHOP.COM", 3.0);
        let (x, y) = (360i64, 360i64);
        synth::overlay(&mut img, &mut truth, &a, x, y, [255, 255, 255], 0.55);

        let bbox_px = BoundingBox::new(x as f32 - 4.0, y as f32 - 4.0, a.width as f32 + 8.0, a.height as f32 + 8.0);
        let crop = bbox_px.expand(20.0, 20.0).to_pixel_rect(w, h);
        let sub = img.crop(&crop);
        let params = MaskParams { mask_dilation: 0.0, mask_feather: 0.0, ..Default::default() };
        let ctx =
            SegmentContext { image: &sub, crop, full_width: w, full_height: h, params: &params, hint: None, quality: Default::default() };
        let c = candidate(bbox_px.to_normalized(w, h));
        let region = ClassicSegmenter.segment(&c, &ctx).unwrap().expect("mask");
        let pred = region.data;
        let t = truth.mask.crop(&crop);
        let (iou, _) = mask_iou(&pred, &t);
        if let Ok(dir) = std::env::var("WM_DEBUG_DIR") {
            let save = |m: &GrayU8, n: &str| {
                image::GrayImage::from_raw(m.width, m.height, m.data.clone()).unwrap().save(format!("{dir}/{n}.png")).unwrap()
            };
            save(&pred, "pred");
            save(&t, "truth");
            let s = image::RgbaImage::from_raw(sub.width, sub.height, sub.data.clone()).unwrap();
            s.save(format!("{dir}/crop.png")).unwrap();
            let p = pred.count_nonzero();
            let tt = t.count_nonzero();
            eprintln!("pred={p} truth={tt}");
        }
        assert!(iou > 0.6, "iou={iou}");
        // 不是整块矩形
        let box_area = (a.width * a.height) as usize;
        assert!(pred.count_nonzero() < box_area * 3 / 4);
    }

    #[test]
    fn low_contrast_flat_region_yields_nothing() {
        let img = wm_core::ImageBuffer::filled(200, 100, [120, 120, 120, 255]);
        let crop = PixelRect::new(0, 0, 200, 100);
        let params = MaskParams::default();
        let ctx = SegmentContext {
            image: &img,
            crop,
            full_width: 200,
            full_height: 100,
            params: &params,
            hint: None,
            quality: Default::default(),
        };
        let c = candidate(BoundingBox::new(0.2, 0.2, 0.5, 0.5));
        assert!(ClassicSegmenter.segment(&c, &ctx).unwrap().is_none());
    }
}
