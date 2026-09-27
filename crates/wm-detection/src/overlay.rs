//! 单图叠加层检测（经典图像分析，无需模型）。
//!
//! 寻找“叠加在背景上的细笔画文字行”：形态学顶帽 → 笔画连通域 → 按尺寸与间距聚成行
//! （PCA 求方向，支持倾斜文字）→ 以位置、低饱和度、笔画强度一致性、半透明对比度、
//! 行直线度等证据打分。
//!
//! 这是启发式检测：普通路牌、衣服 Logo、包装文字、招牌同样是“文字”，因此
//! 置信度上限设为 0.82——在标准模式下只会进入 **复核**，不会被自动删除。
//! 页面中文字行很多时（文档正文），只保留与正文明显不同（更淡或倾斜）的行，保护正文。

use wm_core::traits::{Detection, DetectionInput, MaskHint, WatermarkDetector};
use wm_core::{DetectorSource, Evidence, GrayF32, GrayU8, ImageBuffer, PixelRect, Result, WatermarkCandidate, WatermarkType};
use wm_image::ops::{self, Component};

pub const HEURISTIC_MAX_CONFIDENCE: f32 = 0.82;

pub struct OverlayTextDetector {
    pub canvas: u32,
}

impl Default for OverlayTextDetector {
    fn default() -> Self {
        Self { canvas: 1024 }
    }
}

struct Group {
    comps: Vec<usize>,
    rect: PixelRect,
    angle: f32,
    linearity: f32,
    contrast: f32,
    contrast_cv: f32,
    saturation: f32,
    height_cv: f32,
}

impl WatermarkDetector for OverlayTextDetector {
    fn name(&self) -> &'static str {
        "overlay-text"
    }
    fn source(&self) -> DetectorSource {
        DetectorSource::OverlayHeuristic
    }

    fn detect(&self, input: &DetectionInput) -> Result<Vec<Detection>> {
        let (img, _) = ops::thumbnail(input.image, self.canvas);
        let (w, h) = (img.width, img.height);
        if w < 64 || h < 64 {
            return Ok(Vec::new());
        }
        let luma = img.to_luma_f32();
        let sat = ops::saturation(&img);
        let mut out = Vec::new();
        for bright in [true, false] {
            input.cancel.check()?;
            let th = if bright { ops::tophat_bright(&luma, 5) } else { ops::tophat_dark(&luma, 5) };
            let (groups, labels) = self.groups(&th, &sat);
            let groups = protect_body_text(groups);
            for g in groups {
                if let Some(d) = self.to_detection(&g, &th, &labels, &img, bright) {
                    out.push(d);
                }
            }
        }
        out.sort_by(|a, b| b.candidate.confidence.partial_cmp(&a.candidate.confidence).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(6);
        Ok(out)
    }
}

impl OverlayTextDetector {
    /// 返回（文字行分组, 连通域标签图）。
    fn groups(&self, th: &GrayF32, sat: &GrayF32) -> (Vec<Group>, Vec<u32>) {
        let (w, h) = (th.width, th.height);
        let vals: Vec<f32> = th.data.iter().copied().filter(|v| *v > 3.0).collect();
        if vals.len() < 30 {
            return (Vec::new(), Vec::new());
        }
        let t = ops::otsu(&vals).max(14.0);
        let strong = GrayU8 { width: w, height: h, data: th.data.iter().map(|&v| if v > t { 255 } else { 0 }).collect() };
        let (labels, comps) = ops::connected_components(&strong);
        let max_area = (w * h) as f32 * 0.004;
        // 候选笔画（字形）
        let glyphs: Vec<usize> = comps
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let fill = c.area as f32 / c.rect.area() as f32;
                c.area >= 5
                    && (c.area as f32) < max_area
                    && c.rect.height >= 4
                    && c.rect.height <= 90
                    && c.rect.width <= 120
                    && (0.08..=0.95).contains(&fill)
            })
            .map(|(i, _)| i)
            .collect();
        if glyphs.len() < 3 {
            return (Vec::new(), labels);
        }
        // 并查集聚类：中心距离 < 1.5 × 较大尺寸，且尺寸比 < 2.2
        let mut parent: Vec<usize> = (0..glyphs.len()).collect();
        fn find(p: &mut [usize], i: usize) -> usize {
            let mut r = i;
            while p[r] != r {
                p[r] = p[p[r]];
                r = p[r];
            }
            r
        }
        let size = |c: &Component| c.rect.width.max(c.rect.height) as f32;
        // 按 x 排序以加速邻域查找
        let mut order: Vec<usize> = (0..glyphs.len()).collect();
        order.sort_by(|&a, &b| comps[glyphs[a]].cx.partial_cmp(&comps[glyphs[b]].cx).unwrap());
        for (oi, &a) in order.iter().enumerate() {
            let ca = &comps[glyphs[a]];
            for &b in order.iter().skip(oi + 1) {
                let cb = &comps[glyphs[b]];
                let reach = 1.5 * size(ca).max(size(cb));
                if cb.cx - ca.cx > reach {
                    break;
                }
                let d = ((ca.cx - cb.cx).powi(2) + (ca.cy - cb.cy).powi(2)).sqrt();
                let ratio = size(ca).max(size(cb)) / size(ca).min(size(cb)).max(1.0);
                if d < reach && ratio < 2.2 {
                    let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                    if ra != rb {
                        parent[ra] = rb;
                    }
                }
            }
        }
        let mut buckets: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
        for i in 0..glyphs.len() {
            let r = find(&mut parent, i);
            buckets.entry(r).or_default().push(glyphs[i]);
        }
        let mut groups = Vec::new();
        for (_, members) in buckets {
            if members.len() < 3 || members.len() > 80 {
                continue;
            }
            let cs: Vec<&Component> = members.iter().map(|&i| &comps[i]).collect();
            // PCA：方向与直线度
            let n = cs.len() as f32;
            let mx = cs.iter().map(|c| c.cx).sum::<f32>() / n;
            let my = cs.iter().map(|c| c.cy).sum::<f32>() / n;
            let (mut sxx, mut syy, mut sxy) = (0.0f32, 0.0f32, 0.0f32);
            for c in &cs {
                sxx += (c.cx - mx).powi(2);
                syy += (c.cy - my).powi(2);
                sxy += (c.cx - mx) * (c.cy - my);
            }
            let tr = sxx + syy;
            let det = sxx * syy - sxy * sxy;
            let disc = (tr * tr / 4.0 - det).max(0.0).sqrt();
            let (l1, l2) = (tr / 2.0 + disc, (tr / 2.0 - disc).max(0.0));
            let linearity = if l1 <= 1e-6 { 0.0 } else { 1.0 - (l2 / l1).sqrt() };
            let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
            let mut rect = cs[0].rect;
            for c in &cs[1..] {
                rect = rect.union(&c.rect);
            }
            // 笔画像素统计
            let mut contrast = Vec::new();
            let mut sats = Vec::new();
            let member_set: std::collections::HashSet<u32> = members.iter().map(|&i| comps[i].label).collect();
            for y in rect.y..rect.bottom() {
                for x in rect.x..rect.right() {
                    let i = (y * w + x) as usize;
                    if member_set.contains(&labels[i]) {
                        contrast.push(th.data[i]);
                        sats.push(sat.data[i]);
                    }
                }
            }
            let mc = contrast.iter().sum::<f32>() / contrast.len().max(1) as f32;
            let sd = (contrast.iter().map(|v| (v - mc).powi(2)).sum::<f32>() / contrast.len().max(1) as f32).sqrt();
            let heights: Vec<f32> = cs.iter().map(|c| c.rect.height as f32).collect();
            let mh = heights.iter().sum::<f32>() / n;
            let hsd = (heights.iter().map(|v| (v - mh).powi(2)).sum::<f32>() / n).sqrt();
            groups.push(Group {
                comps: members,
                rect,
                angle: angle.to_degrees(),
                linearity,
                contrast: mc,
                contrast_cv: sd / mc.max(1e-3),
                saturation: sats.iter().sum::<f32>() / sats.len().max(1) as f32,
                height_cv: hsd / mh.max(1e-3),
            });
        }
        (groups, labels)
    }

    fn to_detection(&self, g: &Group, th: &GrayF32, labels: &[u32], img: &ImageBuffer, bright: bool) -> Option<Detection> {
        let (w, h) = (img.width as f32, img.height as f32);
        if g.linearity < 0.75 && g.comps.len() < 6 {
            return None;
        }
        let c = g.rect.to_bbox();
        let cx = (c.x + c.width / 2.0) / w;
        let cy = (c.y + c.height / 2.0) / h;
        let edge_x = cx.min(1.0 - cx);
        let edge_y = cy.min(1.0 - cy);
        let e_pos = if edge_x < 0.25 && edge_y < 0.25 {
            1.0
        } else if edge_x < 0.12 || edge_y < 0.12 {
            0.75
        } else {
            0.45
        };
        let e_sat = (1.0 - g.saturation / 0.3).clamp(0.0, 1.0);
        let e_uniform = (1.0 - g.contrast_cv / 0.8).clamp(0.0, 1.0);
        // 半透明叠加的对比度通常中等；非常高的对比度更像实物文字
        let e_contrast = if g.contrast < 14.0 {
            0.3
        } else if g.contrast <= 90.0 {
            1.0
        } else {
            (1.0 - (g.contrast - 90.0) / 100.0).clamp(0.2, 1.0)
        };
        let e_line = g.linearity.clamp(0.0, 1.0);
        let e_height = (1.0 - g.height_cv / 0.6).clamp(0.0, 1.0);
        let e_count = ((g.comps.len() as f32 - 2.0) / 6.0).clamp(0.0, 1.0);
        let s = 0.22 * e_pos + 0.2 * e_sat + 0.14 * e_uniform + 0.12 * e_contrast + 0.14 * e_line + 0.08 * e_height + 0.10 * e_count;
        let conf = (0.3 + 0.55 * s).min(HEURISTIC_MAX_CONFIDENCE);
        if conf < 0.5 {
            return None;
        }
        let pad = (g.rect.height as f32 * 0.35).max(3.0) as u32;
        let rect = g.rect.pad(pad, img.width, img.height);
        let bbox = rect.to_bbox().to_normalized(img.width, img.height);

        // 提示：组内笔画像素 + 同位置较弱的顶帽响应（便于覆盖抗锯齿边缘）
        // 组件标签 = 组件下标 + 1
        let mut hint = GrayU8::new(rect.width, rect.height);
        let set: std::collections::HashSet<usize> = g.comps.iter().copied().collect();
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                let i = (y * img.width + x) as usize;
                let lab = labels[i];
                if lab > 0 && set.contains(&(lab as usize - 1)) {
                    hint.set(x - rect.x, y - rect.y, 255);
                } else if th.data[i] > g.contrast * 0.4 {
                    hint.set(x - rect.x, y - rect.y, 110);
                }
            }
        }
        let mut cand = WatermarkCandidate::new(wm_common::new_id(), WatermarkType::Text, conf, bbox, DetectorSource::OverlayHeuristic);
        cand.rotation = g.angle;
        cand.evidence = vec![
            Evidence::new(DetectorSource::OverlayHeuristic, "position_prior", e_pos),
            Evidence::new(DetectorSource::OverlayHeuristic, "low_saturation", e_sat),
            Evidence::new(DetectorSource::OverlayHeuristic, "uniform_opacity", e_uniform),
            Evidence::new(DetectorSource::OverlayHeuristic, "overlay_contrast", e_contrast),
            Evidence::new(DetectorSource::OverlayHeuristic, "text_line", e_line),
            Evidence::new(DetectorSource::OverlayHeuristic, if bright { "bright_overlay" } else { "dark_overlay" }, 1.0),
        ];
        Some(Detection { candidate: cand, hint: Some(MaskHint { bbox, data: hint }) })
    }
}

/// 正文保护：文字行很多时（文档正文、密集招牌），只保留与多数行明显不同的行
/// ——更淡（对比度 < 多数的 60%）或明显倾斜（与多数方向相差 > 15°）。
fn protect_body_text(groups: Vec<Group>) -> Vec<Group> {
    if groups.len() < 6 {
        return groups;
    }
    let mut cs: Vec<f32> = groups.iter().map(|g| g.contrast).collect();
    let mut an: Vec<f32> = groups.iter().map(|g| g.angle).collect();
    let mc = ops::median(&mut cs);
    let ma = ops::median(&mut an);
    groups.into_iter().filter(|g| g.contrast < mc * 0.6 || angle_diff(g.angle, ma) > 15.0).collect()
}

fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(180.0);
    d.min(180.0 - d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_core::CancellationToken;
    use wm_image::synth;

    fn run(img: &ImageBuffer) -> Vec<Detection> {
        let cancel = CancellationToken::new();
        let input = DetectionInput {
            file_id: "f",
            image: img,
            original_width: img.width,
            original_height: img.height,
            profiles: &[],
            quality: Default::default(),
            cancel: &cancel,
        };
        OverlayTextDetector::default().detect(&input).unwrap()
    }

    #[test]
    fn finds_corner_watermark_text_for_review() {
        let (w, h) = (1000u32, 700u32);
        let mut img = synth::photo_like(w, h, 42);
        let mut t = synth::Overlay::new(w, h);
        let a = synth::render_text("@PHOTOGRAPHER", 3.0);
        synth::overlay(&mut img, &mut t, &a, (w - a.width - 30) as i64, (h - a.height - 30) as i64, [255, 255, 255], 0.6);
        let d = run(&img);
        let tb = t.mask.nonzero_bounds().unwrap().to_bbox().to_normalized(w, h);
        let hit = d.iter().find(|x| x.candidate.bbox.iou(&tb) > 0.3).expect("corner text detected");
        assert!(hit.candidate.confidence <= HEURISTIC_MAX_CONFIDENCE);
        assert!(hit.candidate.confidence >= 0.6, "conf {}", hit.candidate.confidence);
    }

    #[test]
    fn plain_photo_has_no_confident_text() {
        let img = synth::photo_like(900, 600, 5);
        let d = run(&img);
        assert!(d.iter().all(|x| x.candidate.confidence < 0.7), "{:?}", d.iter().map(|x| x.candidate.confidence).collect::<Vec<_>>());
    }

    #[test]
    fn document_body_text_is_protected() {
        // 白底黑字的多行正文
        let (w, h) = (900u32, 1200u32);
        let mut img = ImageBuffer::filled(w, h, [250, 250, 250, 255]);
        let mut t = synth::Overlay::new(w, h);
        let line = synth::render_text("THE QUICK BROWN FOX JUMPS", 2.0);
        for k in 0..14 {
            synth::overlay(&mut img, &mut t, &line, 60, 80 + k * 70, [20, 20, 20], 1.0);
        }
        let d = run(&img);
        assert!(d.is_empty(), "body text should not become candidates: {}", d.len());
    }
}
