//! Batch Persistence Detection（规格 §6）。
//!
//! 学习：从一组不同内容的样本中寻找持续出现的水印特征。
//! 1. 对齐：相对模式（整图缩放到统一画布）或锚定模式（固定像素大小的角落窗口）；
//! 2. 逐像素对各样本的 Sobel 梯度取 **中值**：图像内容的梯度随机、中值趋近 0，
//!    水印的梯度在各样本中一致而被保留（Dekel et al., CVPR 2017 的思路）；
//! 3. 方向一致性筛选 → 连通区域 → 在细节分辨率下建立模板；
//! 4. 以样本均值图估计极性与水印像素支撑，用 Telea 估计各样本背景后
//!    线性回归反推 alpha 与水印颜色，并给出 alpha 估计质量。
//!
//! 匹配：Template Search + 位置修正 + 多尺度，匹配可靠时复用批次特征，不匹配时回到完整检测。
//! 模板不依赖固定绝对坐标。

use rayon::prelude::*;
use wm_core::batch::{Anchor, BatchWatermarkProfile, ProfileTemplate, ScaleMode};
use wm_core::traits::{Detection, DetectionInput, MaskHint, WatermarkDetector};
use wm_core::{
    BoundingBox, CancellationToken, DetectorSource, Evidence, GrayF32, GrayU8, ImageBuffer, PixelRect, Result, WatermarkCandidate,
    WatermarkType,
};
use wm_image::ops;
use wm_removal::{alpha, telea};

/// 学习样本：已定向的检测缩略图 + 原图尺寸。
pub struct LearnSample<'a> {
    pub image: &'a ImageBuffer,
    pub original_width: u32,
    pub original_height: u32,
}

#[derive(Debug, Clone)]
pub struct BatchLearner {
    /// 区域发现画布长边。
    pub canvas: u32,
    /// 模板细节画布长边上限。
    pub detail: u32,
    /// 最少样本数。
    pub min_samples: usize,
}

impl Default for BatchLearner {
    fn default() -> Self {
        Self { canvas: 512, detail: 1024, min_samples: 3 }
    }
}

/// 样本对齐方式。
#[derive(Debug, Clone, Copy)]
enum Align {
    /// 整图缩放到 (w, h)。
    Relative { aspect: f32 },
    /// 共同尺度下的角落窗口：窗口尺寸为参考长边的 `frac`。
    Corner { corner: Anchor, ref_long: u32, frac: f32, aspect: f32 },
}

impl Align {
    /// 在长边 `r` 的尺度下，画布（窗口）的尺寸。
    fn dims(&self, r: u32) -> (u32, u32) {
        match *self {
            Align::Relative { aspect } => fit(aspect, r),
            Align::Corner { frac, aspect, .. } => {
                let (w, h) = fit(aspect, r);
                (((w as f32 * frac).round() as u32).max(16), ((h as f32 * frac).round() as u32).max(16))
            }
        }
    }

    /// 生成样本在长边 `r` 尺度下的对齐画布。
    fn canvas(&self, s: &LearnSample, r: u32) -> ImageBuffer {
        match *self {
            Align::Relative { .. } => {
                let (w, h) = self.dims(r);
                ops::resize(s.image, w, h)
            }
            Align::Corner { corner, ref_long, .. } => {
                // 统一到 “原图像素 × r/ref_long” 的共同尺度
                let orig_long = s.original_width.max(s.original_height) as f32;
                let thumb_long = s.image.width.max(s.image.height) as f32;
                let f = r as f32 / ref_long as f32 * orig_long / thumb_long;
                let (sw, sh) = (((s.image.width as f32 * f).round() as u32).max(1), ((s.image.height as f32 * f).round() as u32).max(1));
                let scaled = ops::resize(s.image, sw, sh);
                let (ww, wh) = self.dims(r);
                let (ww, wh) = (ww.min(sw), wh.min(sh));
                let (x, y) = corner_origin(corner, sw, sh, ww, wh);
                let mut c = scaled.crop(&PixelRect::new(x, y, ww, wh));
                if c.width != self.dims(r).0 || c.height != self.dims(r).1 {
                    // 样本比窗口小：边缘补齐
                    let (dw, dh) = self.dims(r);
                    let mut full = ImageBuffer::filled(dw, dh, [128, 128, 128, 255]);
                    let (px, py) = corner_origin(corner, dw, dh, c.width, c.height);
                    full.paste(&c, px, py);
                    c = full;
                }
                c
            }
        }
    }
}

fn fit(aspect: f32, r: u32) -> (u32, u32) {
    if aspect >= 1.0 {
        (r, ((r as f32 / aspect).round() as u32).max(8))
    } else {
        (((r as f32 * aspect).round() as u32).max(8), r)
    }
}

fn corner_origin(c: Anchor, w: u32, h: u32, ww: u32, wh: u32) -> (u32, u32) {
    let right = w.saturating_sub(ww);
    let bottom = h.saturating_sub(wh);
    match c {
        Anchor::TopLeft => (0, 0),
        Anchor::TopRight => (right, 0),
        Anchor::BottomLeft => (0, bottom),
        Anchor::BottomRight => (right, bottom),
        _ => (right / 2, bottom / 2),
    }
}

struct Region {
    rect: PixelRect,
    strength: f32,
}

impl BatchLearner {
    /// 学习一个分组（同长宽比桶）的水印模板。
    pub fn learn(&self, group_key: &str, samples: &[LearnSample], cancel: &CancellationToken) -> Result<Vec<BatchWatermarkProfile>> {
        if samples.len() < self.min_samples {
            return Ok(Vec::new());
        }
        let mut aspects: Vec<f32> = samples.iter().map(|s| s.image.width as f32 / s.image.height.max(1) as f32).collect();
        let aspect = ops::median(&mut aspects);
        let mut longs: Vec<f32> = samples.iter().map(|s| s.original_width.max(s.original_height) as f32).collect();
        let ref_long = ops::median(&mut longs).round() as u32;
        let (lo, hi) = (longs.iter().copied().fold(f32::MAX, f32::min), longs.iter().copied().fold(f32::MIN, f32::max));
        let thumb_long = samples.iter().map(|s| s.image.width.max(s.image.height)).min().unwrap_or(512);
        let detail = self.detail.min(thumb_long).max(self.canvas);

        let mut profiles = Vec::new();
        let rel = Align::Relative { aspect };
        profiles.extend(self.learn_aligned(group_key, samples, rel, detail, cancel)?);

        // 尺寸差异明显时，额外尝试“固定像素大小 + 角落锚定”假设
        if hi > lo * 1.08 {
            for corner in [Anchor::TopLeft, Anchor::TopRight, Anchor::BottomLeft, Anchor::BottomRight] {
                cancel.check()?;
                let al = Align::Corner { corner, ref_long, frac: 0.42, aspect };
                profiles.extend(self.learn_aligned(group_key, samples, al, detail, cancel)?);
            }
        }

        // 泛化验证：模板在全部样本上的平均匹配分。对齐假设错误的模板（例如固定像素水印
        // 在相对模式下被部分对齐）在不同尺寸样本上得分偏低。
        let picks = wm_core::batch::spread_indices(samples.len(), 8);
        let keys: std::collections::BTreeSet<(u32, u32)> = profiles.iter().map(canvas_key).collect();
        let canvases: std::collections::HashMap<((u32, u32), usize), MatchCanvas> = keys
            .iter()
            .flat_map(|k| picks.iter().map(move |&i| (*k, i)))
            .collect::<Vec<_>>()
            .into_par_iter()
            .map(|(k, i)| {
                let p = profiles.iter().find(|p| canvas_key(p) == k).unwrap();
                let s = &samples[i];
                ((k, i), prepare_canvas(s.image, (s.original_width, s.original_height), p))
            })
            .collect();
        let fitness: Vec<f32> = profiles
            .par_iter()
            .map(|p| {
                let k = canvas_key(p);
                let total: f32 =
                    picks.iter().map(|&i| search_profile(&canvases[&(k, i)], p, 0.0, 4).map(|m| m.score.max(0.0)).unwrap_or(0.0)).sum();
                total / picks.len() as f32
            })
            .collect();
        let mut scored: Vec<(BatchWatermarkProfile, f32)> = profiles.into_iter().zip(fitness).filter(|(_, f)| *f >= 0.3).collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut kept: Vec<BatchWatermarkProfile> = Vec::new();
        for (mut p, f) in scored {
            // 同一位置的重复模板只保留泛化最好的
            if kept.iter().any(|q| q.anchor == p.anchor && q.normalized_bbox.iou(&p.normalized_bbox) > 0.2) {
                continue;
            }
            p.confidence = (p.confidence * (0.85 + 0.15 * (f / 0.6).min(1.0))).clamp(0.3, 0.97);
            kept.push(p);
        }
        Ok(kept)
    }

    fn learn_aligned(
        &self,
        group_key: &str,
        samples: &[LearnSample],
        al: Align,
        detail: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<BatchWatermarkProfile>> {
        let (cw, ch) = al.dims(self.canvas);
        // 1) 各样本梯度
        let grads: Vec<(GrayF32, GrayF32)> = samples.par_iter().map(|s| ops::sobel(&al.canvas(s, self.canvas).to_luma_f32())).collect();
        cancel.check()?;
        let n = grads.len();
        let npx = (cw * ch) as usize;
        // 2) 逐像素中值梯度 + 方向一致性
        let per_px: Vec<(f32, f32, f32)> = (0..npx)
            .into_par_iter()
            .map(|i| {
                let mut xs: Vec<f32> = grads.iter().map(|g| g.0.data[i]).collect();
                let mut ys: Vec<f32> = grads.iter().map(|g| g.1.data[i]).collect();
                let mx = ops::median(&mut xs);
                let my = ops::median(&mut ys);
                let m = (mx * mx + my * my).sqrt();
                let mut agree = 0usize;
                if m > 1e-3 {
                    for g in &grads {
                        let (gx, gy) = (g.0.data[i], g.1.data[i]);
                        let gm = (gx * gx + gy * gy).sqrt();
                        if gm > 0.4 * m && (gx * mx + gy * my) / (gm * m + 1e-6) > 0.6 {
                            agree += 1;
                        }
                    }
                }
                (mx, my, agree as f32 / n as f32)
            })
            .collect();
        let mag: Vec<f32> = per_px.iter().map(|p| (p.0 * p.0 + p.1 * p.1).sqrt()).collect();
        let tau = (ops::percentile(&mag, 0.5) * 4.0).max(2.5);
        let mut pers = GrayU8::new(cw, ch);
        for i in 0..npx {
            if mag[i] > tau && per_px[i].2 >= 0.6 {
                pers.data[i] = 255;
            }
        }
        // 3) 区域
        let regions = self.regions(&pers, &mag);
        if regions.is_empty() {
            return Ok(Vec::new());
        }
        cancel.check()?;

        // 4) 细节模板
        let k = detail as f32 / self.canvas as f32;
        let (dw, dh) = al.dims(detail);
        let detail_canvases: Vec<ImageBuffer> = samples.par_iter().map(|s| al.canvas(s, detail)).collect();
        let mut out = Vec::new();
        for reg in regions {
            cancel.check()?;
            let r = PixelRect::new(
                (reg.rect.x as f32 * k) as u32,
                (reg.rect.y as f32 * k) as u32,
                (reg.rect.width as f32 * k).ceil() as u32,
                (reg.rect.height as f32 * k).ceil() as u32,
            );
            let r = wm_core::buffer::clamp_rect(&r, dw, dh);
            if r.width < 6 || r.height < 6 {
                continue;
            }
            if let Some(p) = self.build_profile(group_key, al, &detail_canvases, r, (dw, dh), detail, reg.strength, n) {
                out.push(p);
            }
        }
        Ok(out)
    }

    fn regions(&self, pers: &GrayU8, mag: &[f32]) -> Vec<Region> {
        let (w, h) = (pers.width, pers.height);
        let joined = ops::dilate(&ops::close(pers, 2.0), 3.0);
        let (labels, comps) = ops::connected_components(&joined);
        let mut out = Vec::new();
        for c in comps {
            // 区域内原始持续边缘数量
            let mut n_edge = 0usize;
            let mut s = 0.0f32;
            for y in c.rect.y..c.rect.bottom() {
                for x in c.rect.x..c.rect.right() {
                    let i = (y * w + x) as usize;
                    if labels[i] == c.label && pers.data[i] > 0 {
                        n_edge += 1;
                        s += mag[i];
                    }
                }
            }
            if n_edge < 12 {
                continue;
            }
            let area_frac = c.rect.area() as f32 / (w * h) as f32;
            if area_frac > 0.5 {
                continue;
            }
            out.push(Region { rect: c.rect.pad(4, w, h), strength: s / n_edge as f32 });
        }
        out.sort_by(|a, b| b.strength.partial_cmp(&a.strength).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(6);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn build_profile(
        &self,
        group_key: &str,
        al: Align,
        canvases: &[ImageBuffer],
        r: PixelRect,
        dims: (u32, u32),
        detail: u32,
        strength: f32,
        n: usize,
    ) -> Option<BatchWatermarkProfile> {
        // 以区域外扩一圈作为背景环带
        let margin = (r.width.min(r.height) / 3).clamp(6, 40);
        let outer = r.pad(margin, dims.0, dims.1);
        let crops: Vec<ImageBuffer> = canvases.iter().map(|c| c.crop(&outer)).collect();
        let (ow, oh) = (outer.width, outer.height);
        let inner = PixelRect::new(r.x - outer.x, r.y - outer.y, r.width, r.height);

        // 中值梯度模板
        let grads: Vec<(GrayF32, GrayF32)> = crops.iter().map(|c| ops::sobel(&c.to_luma_f32())).collect();
        let npx = (ow * oh) as usize;
        let mut gx = GrayF32::new(ow, oh);
        let mut gy = GrayF32::new(ow, oh);
        for i in 0..npx {
            let mut xs: Vec<f32> = grads.iter().map(|g| g.0.data[i]).collect();
            let mut ys: Vec<f32> = grads.iter().map(|g| g.1.data[i]).collect();
            gx.data[i] = ops::median(&mut xs);
            gy.data[i] = ops::median(&mut ys);
        }

        // 均值图 → 极性与支撑
        let mut mean = ImageBuffer::new(ow, oh);
        for i in 0..npx * 4 {
            let s: u32 = crops.iter().map(|c| c.data[i] as u32).sum();
            mean.data[i] = (s / crops.len() as u32) as u8;
        }
        let luma = mean.to_luma_f32();
        let stroke_r = ((r.height.min(r.width) as f32 / 5.0).round() as u32).clamp(2, 12);
        let tb = ops::tophat_bright(&luma, stroke_r);
        let td = ops::tophat_dark(&luma, stroke_r);
        let mut ring_mask = GrayU8::new(ow, oh);
        for y in 0..oh {
            for x in 0..ow {
                if !inner.contains(x, y) {
                    ring_mask.set(x, y, 255);
                }
            }
        }
        let ring: Vec<f32> = luma.data.iter().zip(&ring_mask.data).filter(|(_, &m)| m > 0).map(|(&v, _)| v).collect();
        let (p5, p95) = (ops::percentile(&ring, 0.05), ops::percentile(&ring, 0.95));
        let (mut nb, mut nd) = (0, 0);
        for y in inner.y..inner.bottom() {
            for x in inner.x..inner.right() {
                let v = luma.get(x, y);
                if v > p95 + 4.0 {
                    nb += 1;
                } else if v < p5 - 4.0 {
                    nd += 1;
                }
            }
        }
        let bright = nb >= nd;
        let th = if bright { &tb } else { &td };
        let inner_vals: Vec<f32> =
            (inner.y..inner.bottom()).flat_map(|y| (inner.x..inner.right()).map(move |x| (x, y))).map(|(x, y)| th.get(x, y)).collect();
        let peak = ops::percentile(&inner_vals, 0.98).max(3.0);
        let floor = ops::otsu(&inner_vals).max(2.5) * 0.6;
        let mut support = GrayF32::new(ow, oh);
        for y in inner.y..inner.bottom() {
            for x in inner.x..inner.right() {
                let v = th.get(x, y);
                support.set(x, y, if v <= floor { 0.0 } else { ((v - floor) / (peak - floor).max(1e-3)).clamp(0.0, 1.0) });
            }
        }
        let sup_bin = GrayU8 { width: ow, height: oh, data: support.data.iter().map(|&v| if v > 0.25 { 255 } else { 0 }).collect() };
        let sup_bin = ops::remove_small_components(&sup_bin, 4);
        if sup_bin.count_nonzero() < 8 {
            return None;
        }
        // 收紧到水印像素的实际范围（区域发现阶段带有 padding）
        let tight = sup_bin.nonzero_bounds()?.pad(stroke_r.max(2), ow, oh);
        let inner = PixelRect::new(
            tight.x.max(inner.x),
            tight.y.max(inner.y),
            tight.right().min(inner.right()) - tight.x.max(inner.x),
            tight.bottom().min(inner.bottom()) - tight.y.max(inner.y),
        );
        if inner.width < 4 || inner.height < 4 {
            return None;
        }
        let r = PixelRect::new(outer.x + inner.x, outer.y + inner.y, inner.width, inner.height);

        // alpha / 颜色估计：Telea 估背景 → 逐像素回归
        let hole_mask = ops::dilate(&sup_bin, 1.5);
        let hole: Vec<bool> = hole_mask.data.iter().map(|&v| v > 0).collect();
        let backgrounds: Vec<Vec<Vec<f32>>> = crops
            .par_iter()
            .map(|c| {
                let mut planes: Vec<Vec<f32>> = ops::split_rgb(c).map(|g| g.data).to_vec();
                telea::inpaint_planes(&mut planes, ow as usize, oh as usize, &hole, 4.0);
                planes
            })
            .collect();
        let (color, alpha_map, alpha_quality) = estimate_alpha(&crops, &backgrounds, &sup_bin, bright);

        // 与各样本的一致性（验证）
        let tpl: Vec<f32> = crop_vals(&gx, &inner).into_iter().chain(crop_vals(&gy, &inner)).collect();
        let mut matched = 0usize;
        for g in &grads {
            let v: Vec<f32> = crop_vals(&g.0, &inner).into_iter().chain(crop_vals(&g.1, &inner)).collect();
            if ops::ncc(&tpl, &v) > 0.3 {
                matched += 1;
            }
        }
        let match_frac = matched as f32 / n as f32;
        if match_frac < 0.5 {
            return None;
        }

        let area_frac = r.area() as f32 / (dims.0 * dims.1) as f32;
        let mean_alpha = {
            let v: Vec<f32> = alpha_map.data.iter().zip(&sup_bin.data).filter(|(_, &m)| m > 0).map(|(&a, _)| a).collect();
            if v.is_empty() {
                1.0
            } else {
                v.iter().sum::<f32>() / v.len() as f32
            }
        };
        let mut conf = 0.55 + 0.42 * match_frac * (n as f32 / 6.0).min(1.0);
        if area_frac > 0.25 {
            conf -= 0.15;
        }
        let center = r.to_bbox().to_normalized(dims.0, dims.1).center();
        let centered = Anchor::from_center(center.x, center.y) == Anchor::Center;
        let opaque = alpha_quality > 0.3 && mean_alpha > 0.85;
        let confirmed_transparent = alpha_quality >= 0.5 && mean_alpha < 0.8;
        if centered && !confirmed_transparent {
            // 居中的持续内容只有在确认为半透明叠加时才按水印对待（居中大 Logo 水印通常是半透明的）
            conf = conf.min(0.6);
        } else if opaque && area_frac > 0.08 {
            // 画面中央或大面积的不透明持续内容（招牌、统一边框、界面元素）更可能是版式而非水印
            conf = conf.min(0.6);
        } else if opaque {
            conf -= 0.05;
        }
        if strength < 4.0 {
            conf -= 0.08;
        }
        let conf = conf.clamp(0.3, 0.97);
        if conf < 0.6 {
            return None;
        }

        // 模板（只保留 inner 区域）
        let crop_f = |g: &GrayF32| crop_vals(g, &inner);
        let sup_soft: Vec<f32> = crop_f(&support)
            .into_iter()
            .zip(crop_vals_u8(&sup_bin, &inner))
            .map(|(s, b)| if b > 0 { s.max(0.35) } else { s * 0.5 })
            .collect();
        let alpha_vals = if alpha_quality > 0.0 { crop_f(&alpha_map) } else { Vec::new() };
        let template = ProfileTemplate {
            width: inner.width,
            height: inner.height,
            canvas_long_side: detail,
            grad_x: crop_f(&gx),
            grad_y: crop_f(&gy),
            support: sup_soft,
            alpha: alpha_vals,
            color,
            alpha_quality,
        };

        let (normalized_bbox, anchor, scale_mode) = match al {
            Align::Relative { .. } => {
                let b = r.to_bbox().to_normalized(dims.0, dims.1);
                let c = b.center();
                (b, Anchor::from_center(c.x, c.y), ScaleMode::Relative)
            }
            Align::Corner { corner, ref_long, .. } => {
                // 窗口在共同尺度图中的位置未知（每张图不同），记录相对角点的偏移
                let (ox, oy) = match corner {
                    Anchor::TopLeft => (r.x as f32, r.y as f32),
                    Anchor::TopRight => (r.x as f32 - dims.0 as f32, r.y as f32),
                    Anchor::BottomLeft => (r.x as f32, r.y as f32 - dims.1 as f32),
                    _ => (r.x as f32 - dims.0 as f32, r.y as f32 - dims.1 as f32),
                };
                let (fw, fh) = fit(aspect_of(al), detail);
                let (x0, y0) = anchor_point(corner, fw as f32, fh as f32);
                let b = BoundingBox::new(x0 + ox, y0 + oy, r.width as f32, r.height as f32).to_normalized(fw, fh);
                (b, corner, ScaleMode::Anchored { ref_long_side: ref_long, offset_x: ox, offset_y: oy })
            }
        };
        let wm_type = if alpha_quality >= 0.5 && mean_alpha < 0.8 {
            WatermarkType::Transparent
        } else if r.width as f32 > r.height as f32 * 2.5 {
            WatermarkType::Text
        } else {
            WatermarkType::Logo
        };
        let embedding = orientation_hist(&template.grad_x, &template.grad_y);
        let hash = template_hash(&template);
        Some(BatchWatermarkProfile {
            id: wm_common::new_id(),
            normalized_bbox,
            template_hash: hash,
            feature_embedding: embedding,
            confidence: conf,
            sample_count: n,
            group_key: group_key.to_string(),
            anchor,
            scale_mode,
            watermark_type: wm_type,
            template,
        })
    }
}

fn aspect_of(al: Align) -> f32 {
    match al {
        Align::Relative { aspect } | Align::Corner { aspect, .. } => aspect,
    }
}

fn anchor_point(c: Anchor, w: f32, h: f32) -> (f32, f32) {
    match c {
        Anchor::TopLeft => (0.0, 0.0),
        Anchor::TopRight => (w, 0.0),
        Anchor::BottomLeft => (0.0, h),
        _ => (w, h),
    }
}

fn crop_vals(g: &GrayF32, r: &PixelRect) -> Vec<f32> {
    let mut v = Vec::with_capacity(r.area() as usize);
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            v.push(g.get(x, y));
        }
    }
    v
}

fn crop_vals_u8(g: &GrayU8, r: &PixelRect) -> Vec<u8> {
    let mut v = Vec::with_capacity(r.area() as usize);
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            v.push(g.get(x, y));
        }
    }
    v
}

/// 由各样本观测 O 与估计背景 B 反推水印颜色 C 与逐像素 alpha。
/// 返回（颜色, alpha 图, 估计质量 0..1）。
fn estimate_alpha(crops: &[ImageBuffer], bgs: &[Vec<Vec<f32>>], support: &GrayU8, bright: bool) -> ([f32; 3], GrayF32, f32) {
    let (w, h) = (support.width, support.height);
    let n = crops.len();
    let mut alpha_map = GrayF32::new(w, h);
    if n < 3 {
        return ([255.0; 3], alpha_map, 0.0);
    }
    // 每个支撑像素：O = (1-a)·B + a·C 的逐通道最小二乘 → slope=1-a, intercept=a·C
    let mut num = [0.0f64; 3];
    let mut den = 0.0f64;
    for i in 0..(w * h) as usize {
        if support.data[i] == 0 {
            continue;
        }
        let mut a_acc = 0.0f64;
        let mut t_acc = [0.0f64; 3];
        let mut ok = true;
        for c in 0..3 {
            let xs: Vec<f64> = bgs.iter().map(|b| b[c][i] as f64).collect();
            let ys: Vec<f64> = crops.iter().map(|im| im.data[i * 4 + c] as f64).collect();
            let mx = xs.iter().sum::<f64>() / n as f64;
            let my = ys.iter().sum::<f64>() / n as f64;
            let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
            if sxx < 200.0 {
                ok = false;
                break;
            }
            let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
            let slope = (sxy / sxx).clamp(0.0, 1.0);
            a_acc += 1.0 - slope;
            t_acc[c] = my - slope * mx;
        }
        if !ok {
            continue;
        }
        let a = a_acc / 3.0;
        if a > 0.08 {
            for c in 0..3 {
                num[c] += t_acc[c];
            }
            den += a;
        }
    }
    let color = if den > 1.0 {
        [(num[0] / den).clamp(0.0, 255.0) as f32, (num[1] / den).clamp(0.0, 255.0) as f32, (num[2] / den).clamp(0.0, 255.0) as f32]
    } else if bright {
        [255.0; 3]
    } else {
        [0.0; 3]
    };
    // 已知颜色后逐像素稳健估计 alpha，并计算模型残差。
    // 质量以“相对残差”衡量：模型能解释多少水印造成的偏差（|O−B|），
    // 而不是绝对误差——后者主要受背景估计（Telea）在纹理区域的误差影响。
    let mut valid = 0usize;
    let mut total = 0usize;
    let mut resid = Vec::new();
    let mut effect = Vec::new();
    for i in 0..(w * h) as usize {
        if support.data[i] == 0 {
            continue;
        }
        total += 1;
        let mut a_c = Vec::new();
        for c in 0..3 {
            let obs: Vec<f32> = crops.iter().map(|im| im.data[i * 4 + c] as f32).collect();
            let bg: Vec<f32> = bgs.iter().map(|b| b[c][i]).collect();
            if let Some(a) = alpha::estimate_alpha_from_pairs(&obs, &bg, color[c]) {
                a_c.push(a);
            }
        }
        if a_c.is_empty() {
            continue;
        }
        let a = ops::median(&mut a_c);
        alpha_map.data[i] = a;
        valid += 1;
        for (k, im) in crops.iter().enumerate() {
            for c in 0..3 {
                let o = im.data[i * 4 + c] as f32;
                let b = bgs[k][c][i];
                resid.push((o - (b * (1.0 - a) + color[c] * a)).abs());
                effect.push((o - b).abs());
            }
        }
    }
    if total == 0 || valid == 0 {
        return (color, alpha_map, 0.0);
    }
    let err = ops::median(&mut resid);
    let eff = ops::median(&mut effect).max(1.0);
    let explained = (1.0 - err / eff).clamp(0.0, 1.0);
    let valid_frac = valid as f32 / total as f32;
    let sample_factor = if n >= 5 { 1.0 } else { 0.6 };
    let quality = (explained * valid_frac * sample_factor).clamp(0.0, 1.0);
    tracing::debug!(err, eff, explained, valid_frac, quality, "alpha estimate");
    (color, alpha_map, quality)
}

fn orientation_hist(gx: &[f32], gy: &[f32]) -> Vec<f32> {
    let mut h = vec![0.0f32; 16];
    for (x, y) in gx.iter().zip(gy) {
        let m = (x * x + y * y).sqrt();
        if m < 1.0 {
            continue;
        }
        let a = y.atan2(*x) + std::f32::consts::PI;
        let b = ((a / std::f32::consts::TAU) * 16.0) as usize % 16;
        h[b] += m;
    }
    let n = h.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
    h.iter().map(|v| v / n).collect()
}

fn template_hash(t: &ProfileTemplate) -> String {
    let mut bytes = Vec::with_capacity(t.grad_x.len() * 2 + 8);
    bytes.extend_from_slice(&t.width.to_le_bytes());
    bytes.extend_from_slice(&t.height.to_le_bytes());
    for (x, y) in t.grad_x.iter().zip(&t.grad_y) {
        bytes.push((x.clamp(-127.0, 127.0) as i8) as u8);
        bytes.push((y.clamp(-127.0, 127.0) as i8) as u8);
    }
    wm_common::sha256_bytes(&bytes)[..16].to_string()
}

// ───────────────────────────── 匹配 ─────────────────────────────

/// 匹配结果（在检测输入缩略图的归一化坐标中）。
#[derive(Debug, Clone)]
pub struct ProfileMatch {
    pub bbox: BoundingBox,
    pub score: f32,
    pub scale: f32,
}

/// 匹配画布：按模板的缩放方式把图片缩放到学习尺度，并预先计算梯度。
pub struct MatchCanvas {
    width: u32,
    height: u32,
    gx: GrayF32,
    gy: GrayF32,
}

/// 画布缓存键：相对模式只依赖长边；锚定模式还依赖参考长边。
fn canvas_key(p: &BatchWatermarkProfile) -> (u32, u32) {
    match p.scale_mode {
        ScaleMode::Relative => (p.template.canvas_long_side, 0),
        ScaleMode::Anchored { ref_long_side, .. } => (p.template.canvas_long_side, ref_long_side),
    }
}

pub fn prepare_canvas(img: &ImageBuffer, original: (u32, u32), p: &BatchWatermarkProfile) -> MatchCanvas {
    let r = p.template.canvas_long_side;
    let canvas = match p.scale_mode {
        ScaleMode::Relative => {
            let (w, h) = fit(img.width as f32 / img.height.max(1) as f32, r);
            ops::resize(img, w, h)
        }
        ScaleMode::Anchored { ref_long_side, .. } => {
            let orig_long = original.0.max(original.1) as f32;
            let thumb_long = img.width.max(img.height) as f32;
            let f = r as f32 / ref_long_side as f32 * orig_long / thumb_long;
            let (w, h) = (((img.width as f32 * f).round() as u32).max(1), ((img.height as f32 * f).round() as u32).max(1));
            ops::resize(img, w, h)
        }
    };
    let (gx, gy) = ops::sobel(&canvas.to_luma_f32());
    MatchCanvas { width: canvas.width, height: canvas.height, gx, gy }
}

/// 在图片中搜索模板。返回最佳匹配（分数 < `min_score` 时为 None）。
pub fn match_profile(img: &ImageBuffer, original: (u32, u32), p: &BatchWatermarkProfile, min_score: f32) -> Option<ProfileMatch> {
    search_profile(&prepare_canvas(img, original, p), p, min_score, 3)
}

/// 在预先计算的画布上搜索模板（Template Search + Position Correction + 多尺度）。
pub fn search_profile(c: &MatchCanvas, p: &BatchWatermarkProfile, min_score: f32, coarse_step: i64) -> Option<ProfileMatch> {
    let t = &p.template;
    let (cw, ch) = (c.width, c.height);
    let (expect, scales): (BoundingBox, &[f32]) = match p.scale_mode {
        ScaleMode::Relative => (p.normalized_bbox.to_pixels(cw, ch), &[0.9, 1.0, 1.1]),
        ScaleMode::Anchored { offset_x, offset_y, .. } => {
            let (ax, ay) = anchor_point(p.anchor, cw as f32, ch as f32);
            (BoundingBox::new(ax + offset_x, ay + offset_y, t.width as f32, t.height as f32), &[0.95, 1.0, 1.05])
        }
    };
    let base = ((expect.width * expect.height) / (t.width as f32 * t.height as f32)).sqrt().max(0.05);
    let tx = GrayF32 { width: t.width, height: t.height, data: t.grad_x.clone() };
    let ty = GrayF32 { width: t.width, height: t.height, data: t.grad_y.clone() };

    let mut best: Option<ProfileMatch> = None;
    for &s in scales {
        let sc = base * s;
        let (tw, th) = (((t.width as f32 * sc).round() as u32).max(4), ((t.height as f32 * sc).round() as u32).max(4));
        if tw >= cw || th >= ch {
            continue;
        }
        let rtx = ops::resize_gray(&tx, tw, th);
        let rty = ops::resize_gray(&ty, tw, th);
        let tpl: Vec<f32> = rtx.data.iter().chain(&rty.data).copied().collect();
        let cx = expect.x + (expect.width - tw as f32) / 2.0;
        let cy = expect.y + (expect.height - th as f32) / 2.0;
        let search = (0.06 * cw.max(ch) as f32).max(12.0) as i64;
        let mut local_best = (f32::MIN, 0i64, 0i64);
        let eval = |ox: i64, oy: i64| -> f32 {
            let x0 = (cx as i64 + ox).clamp(0, (cw - tw) as i64) as u32;
            let y0 = (cy as i64 + oy).clamp(0, (ch - th) as i64) as u32;
            let r = PixelRect::new(x0, y0, tw, th);
            let v: Vec<f32> = crop_vals(&c.gx, &r).into_iter().chain(crop_vals(&c.gy, &r)).collect();
            ops::ncc(&tpl, &v)
        };
        let mut oy = -search;
        while oy <= search {
            let mut ox = -search;
            while ox <= search {
                let v = eval(ox, oy);
                if v > local_best.0 {
                    local_best = (v, ox, oy);
                }
                ox += coarse_step;
            }
            oy += coarse_step;
        }
        let (_, bx, by) = local_best;
        let fine = coarse_step - 1;
        for dy in -fine..=fine {
            for dx in -fine..=fine {
                let v = eval(bx + dx, by + dy);
                if v > local_best.0 {
                    local_best = (v, bx + dx, by + dy);
                }
            }
        }
        let (score, fx, fy) = local_best;
        let x0 = (cx as i64 + fx).clamp(0, (cw - tw) as i64) as f32;
        let y0 = (cy as i64 + fy).clamp(0, (ch - th) as i64) as f32;
        if best.as_ref().is_none_or(|b| score > b.score) {
            best = Some(ProfileMatch { bbox: BoundingBox::new(x0, y0, tw as f32, th as f32).to_normalized(cw, ch), score, scale: sc });
        }
    }
    best.filter(|b| b.score >= min_score)
}

/// 批次特征检测器：在单个文件中匹配已学习的模板。
pub struct BatchPersistenceDetector {
    pub min_score: f32,
}

impl Default for BatchPersistenceDetector {
    fn default() -> Self {
        Self { min_score: 0.3 }
    }
}

impl WatermarkDetector for BatchPersistenceDetector {
    fn name(&self) -> &'static str {
        "batch-persistence"
    }
    fn source(&self) -> DetectorSource {
        DetectorSource::BatchPersistence
    }
    fn detect(&self, input: &DetectionInput) -> Result<Vec<Detection>> {
        let mut out = Vec::new();
        let mut canvases: std::collections::HashMap<(u32, u32), MatchCanvas> = std::collections::HashMap::new();
        for p in input.profiles {
            input.cancel.check()?;
            let canvas = canvases
                .entry(canvas_key(p))
                .or_insert_with(|| prepare_canvas(input.image, (input.original_width, input.original_height), p));
            let Some(m) = search_profile(canvas, p, self.min_score, 3) else {
                continue;
            };
            let strength = ((m.score - self.min_score) / 0.3).clamp(0.0, 1.0);
            let conf = p.confidence * (0.8 + 0.2 * strength);
            let mut c = WatermarkCandidate::new(wm_common::new_id(), p.watermark_type, conf, m.bbox, DetectorSource::BatchPersistence);
            c.batch_score = m.score;
            c.batch_profile_id = Some(p.id.clone());
            c.opacity = (p.template.alpha_quality > 0.3 && !p.template.alpha.is_empty()).then(|| {
                let s: f32 = p.template.alpha.iter().sum();
                s / p.template.alpha.len() as f32
            });
            c.evidence = vec![
                Evidence::new(DetectorSource::BatchPersistence, "batch_consistency", p.confidence),
                Evidence::new(DetectorSource::BatchPersistence, "template_match", m.score),
                Evidence::new(DetectorSource::BatchPersistence, "sample_coverage", (p.sample_count as f32 / 10.0).min(1.0)),
            ];
            let sup = GrayU8 {
                width: p.template.width,
                height: p.template.height,
                data: p.template.support.iter().map(|&v| (v * 255.0).round().clamp(0.0, 255.0) as u8).collect(),
            };
            out.push(Detection { candidate: c, hint: Some(MaskHint { bbox: m.bbox, data: sup }) });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_image::synth;

    /// 构造一批不同内容、相同右下角半透明文字水印的图片。
    fn batch(n: usize, sizes: &[(u32, u32)], fixed_px: bool) -> Vec<(ImageBuffer, GrayU8)> {
        (0..n)
            .map(|i| {
                let (w, h) = sizes[i % sizes.len()];
                let mut img = synth::photo_like(w, h, 1000 + i as u64);
                let mut t = synth::Overlay::new(w, h);
                let px = if fixed_px { 3.0 } else { w as f32 / 220.0 };
                let a = synth::render_text("SHOP.EXAMPLE", px);
                let x = w as i64 - a.width as i64 - (w as i64 / 25);
                let y = h as i64 - a.height as i64 - (h as i64 / 25);
                let (x, y) = if fixed_px { (w as i64 - a.width as i64 - 20, h as i64 - a.height as i64 - 20) } else { (x, y) };
                synth::overlay(&mut img, &mut t, &a, x, y, [255, 255, 255], 0.45);
                (img, t.mask)
            })
            .collect()
    }

    #[test]
    fn learns_and_matches_relative_watermark() {
        let imgs = batch(10, &[(800, 600)], false);
        let samples: Vec<LearnSample> =
            imgs.iter().map(|(im, _)| LearnSample { image: im, original_width: im.width, original_height: im.height }).collect();
        let profiles = BatchLearner::default().learn("g", &samples, &CancellationToken::new()).unwrap();
        assert!(!profiles.is_empty(), "no profile learned");
        let p = profiles.iter().max_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap()).unwrap();
        assert!(p.confidence >= 0.85, "conf {}", p.confidence);
        assert_eq!(p.anchor, Anchor::BottomRight);
        assert!(p.template.alpha_quality > 0.5, "alpha q {}", p.template.alpha_quality);
        assert_eq!(p.watermark_type, WatermarkType::Transparent);
        let c = p.template.color;
        assert!(c[0] > 200.0 && c[1] > 200.0 && c[2] > 200.0, "color {c:?}");

        // 新图（未参与学习）
        let fresh = batch(12, &[(800, 600)], false);
        let (im, truth) = &fresh[11];
        let m = match_profile(im, (im.width, im.height), p, 0.3).expect("match");
        let tb = truth.nonzero_bounds().unwrap().to_bbox().to_normalized(im.width, im.height);
        assert!(m.bbox.iou(&tb) > 0.5, "iou {} {:?} vs {:?}", m.bbox.iou(&tb), m.bbox, tb);
    }

    #[test]
    fn no_profile_from_unrelated_images() {
        let imgs: Vec<ImageBuffer> = (0..8).map(|i| synth::photo_like(640, 480, 50 + i)).collect();
        let samples: Vec<LearnSample> =
            imgs.iter().map(|im| LearnSample { image: im, original_width: 640, original_height: 480 }).collect();
        let profiles = BatchLearner::default().learn("g", &samples, &CancellationToken::new()).unwrap();
        assert!(profiles.iter().all(|p| p.confidence < 0.85), "{:?}", profiles.iter().map(|p| p.confidence).collect::<Vec<_>>());
    }

    #[test]
    fn matches_fixed_pixel_watermark_across_sizes() {
        let imgs = batch(9, &[(900, 600), (600, 400), (750, 500)], true);
        let samples: Vec<LearnSample> =
            imgs.iter().map(|(im, _)| LearnSample { image: im, original_width: im.width, original_height: im.height }).collect();
        let profiles = BatchLearner::default().learn("g", &samples, &CancellationToken::new()).unwrap();
        assert!(!profiles.is_empty());
        let (im, truth) = &batch(4, &[(660, 440)], true)[3];
        let tb = truth.nonzero_bounds().unwrap().to_bbox().to_normalized(im.width, im.height);

        let best = profiles
            .iter()
            .filter_map(|p| match_profile(im, (im.width, im.height), p, 0.3))
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
            .expect("match");
        assert!(best.bbox.iou(&tb) > 0.4, "iou {}", best.bbox.iou(&tb));
    }
}
