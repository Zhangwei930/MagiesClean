//! 时间、地点信息块检测（经典图像分析，无需模型）。
//!
//! 相机或打卡类应用会在画面上下边缘叠加一整块信息：大号时间、日期、地址、认证标签、
//! 防伪码等。它们是不透明的白色粗笔画（常带阴影），并由多行组成——单行叠加文字检测会把
//! 这种多行、高对比度的文字当作正文保护掉，所以单独检测：
//! 白色笔画（高亮度、低饱和度、亮顶帽响应）→ 字形连通域 → 按间距聚成块 →
//! 以边缘位置、行数、字形数与白度打分。
//!
//! 同样是启发式：招牌、车身文字也可能是白色多行文字，因此只在上下边缘带内寻找，
//! 置信度上限与叠加文字相同，标准模式下进入 **复核**。

use crate::overlay::HEURISTIC_MAX_CONFIDENCE;
use wm_core::traits::{Detection, DetectionInput, MaskHint, WatermarkDetector};
use wm_core::{DetectorSource, Evidence, GrayF32, GrayU8, ImageBuffer, PixelRect, Result, WatermarkCandidate, WatermarkType};
use wm_image::ops::{self, Component};

/// 白色笔画：亮度下限（0..255）、饱和度上限（0..1）、亮顶帽响应下限。
const WHITE_LUMA: f32 = 222.0;
const WHITE_SAT: f32 = 0.22;
const WHITE_TOPHAT: f32 = 28.0;
/// 只在上下边缘带内寻找（占图高的比例）。
const TOP_BAND: f32 = 0.3;
const BOTTOM_BAND: f32 = 0.42;

pub struct InfoStampDetector {
    pub canvas: u32,
}

impl Default for InfoStampDetector {
    fn default() -> Self {
        Self { canvas: 1024 }
    }
}

impl WatermarkDetector for InfoStampDetector {
    fn name(&self) -> &'static str {
        "info-stamp"
    }
    fn source(&self) -> DetectorSource {
        DetectorSource::InfoStamp
    }

    fn detect(&self, input: &DetectionInput) -> Result<Vec<Detection>> {
        let (img, _) = ops::thumbnail(input.image, self.canvas);
        let (w, h) = (img.width, img.height);
        if w < 128 || h < 128 {
            return Ok(Vec::new());
        }
        let luma = img.to_luma_f32();
        let sat = ops::saturation(&img);
        input.cancel.check()?;
        let r = (w.max(h) as f32 / 1024.0 * 7.0).round().max(3.0) as u32;
        let tb = ops::tophat_bright(&luma, r);
        let white = GrayU8 {
            width: w,
            height: h,
            data: (0..(w * h) as usize)
                .map(|i| if luma.data[i] >= WHITE_LUMA && sat.data[i] <= WHITE_SAT && tb.data[i] >= WHITE_TOPHAT { 255 } else { 0 })
                .collect(),
        };
        let (labels, comps) = ops::connected_components(&white);
        let glyphs: Vec<usize> = (0..comps.len()).filter(|&i| is_glyph(&comps[i], w, h)).collect();
        if glyphs.len() < 6 || glyphs.len() > 4000 {
            return Ok(Vec::new());
        }
        input.cancel.check()?;
        let td = ops::tophat_dark(&luma, r);
        let mut out = Vec::new();
        for members in link(&comps, &glyphs) {
            if let Some(d) = evaluate(&members, &comps, &labels, &img, &luma, &sat, &tb, &td) {
                out.push(d);
            }
        }
        out.sort_by(|a, b| b.candidate.confidence.partial_cmp(&a.candidate.confidence).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(4);
        Ok(out)
    }
}

/// 字形：尺寸合理、不是细长斜线（电线、栏杆）且位于上下边缘带内。
fn is_glyph(c: &Component, w: u32, h: u32) -> bool {
    let s = w.max(h) as f32;
    let fill = c.area as f32 / c.rect.area().max(1) as f32;
    let in_band = c.cy <= h as f32 * TOP_BAND || c.cy >= h as f32 * (1.0 - BOTTOM_BAND);
    c.area >= 4
        && (c.rect.height as f32) <= 0.11 * s
        && (c.rect.width as f32) <= 0.16 * s
        && (fill >= 0.12 || c.rect.area() <= 16)
        && in_band
}

/// 按字形间距聚类：水平、垂直间隙都不超过较大字形高度的 0.9 倍时相连。
/// 大号时间与下方的小号地址行因此能连成同一块，远处的独立文字则不会。
fn link(comps: &[Component], glyphs: &[usize]) -> Vec<Vec<usize>> {
    let n = glyphs.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            p[r] = p[p[r]];
            r = p[r];
        }
        r
    }
    let max_h = glyphs.iter().map(|&g| comps[g].rect.height).max().unwrap_or(0) as f32;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| comps[glyphs[i]].rect.x);
    for (oi, &a) in order.iter().enumerate() {
        let ra = comps[glyphs[a]].rect;
        for &b in order.iter().skip(oi + 1) {
            let rb = comps[glyphs[b]].rect;
            if rb.x as f32 > ra.right() as f32 + 0.9 * max_h + 3.0 {
                break;
            }
            let reach = 0.9 * ra.height.max(rb.height) as f32 + 3.0;
            let gap_x = (rb.x as i64 - ra.right() as i64).max(ra.x as i64 - rb.right() as i64).max(0) as f32;
            let gap_y = (rb.y as i64 - ra.bottom() as i64).max(ra.y as i64 - rb.bottom() as i64).max(0) as f32;
            if gap_x <= reach && gap_y <= reach {
                let (pa, pb) = (find(&mut parent, a), find(&mut parent, b));
                if pa != pb {
                    parent[pa] = pb;
                }
            }
        }
    }
    let mut buckets: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for (i, &g) in glyphs.iter().enumerate() {
        let r = find(&mut parent, i);
        buckets.entry(r).or_default().push(g);
    }
    buckets.into_values().filter(|m| m.len() >= 6).collect()
}

/// 文字行数：成员像素的垂直投影中被空行隔开的段数（只计含 2 个以上字形的段）。
fn count_rows(members: &[usize], comps: &[Component], rect: &PixelRect) -> usize {
    let mut cover = vec![0u32; rect.height as usize];
    for &m in members {
        let r = comps[m].rect;
        for y in r.y..r.bottom() {
            cover[(y - rect.y) as usize] += 1;
        }
    }
    let mut rows = 0;
    let mut y = 0;
    while y < cover.len() {
        if cover[y] == 0 {
            y += 1;
            continue;
        }
        let start = y;
        while y < cover.len() && cover[y] > 0 {
            y += 1;
        }
        let glyphs = members.iter().filter(|&&m| {
            let cy = comps[m].cy - rect.y as f32;
            cy >= start as f32 && cy < y as f32
        });
        if glyphs.count() >= 2 {
            rows += 1;
        }
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn evaluate(
    members: &[usize],
    comps: &[Component],
    labels: &[u32],
    img: &ImageBuffer,
    luma: &GrayF32,
    sat: &GrayF32,
    tb: &GrayF32,
    td: &GrayF32,
) -> Option<Detection> {
    let (w, h) = (img.width, img.height);
    let mut rect = comps[members[0]].rect;
    for &m in &members[1..] {
        rect = rect.union(&comps[m].rect);
    }
    // 信息块只占画面一角或一条：过宽、过高的是画面内容
    if rect.width as f32 > 0.85 * w as f32 || rect.height as f32 > 0.35 * h as f32 {
        return None;
    }
    let edge_v = rect.y.min(h - rect.bottom()) as f32 / h as f32;
    if edge_v > 0.2 {
        return None;
    }
    let rows = count_rows(members, comps, &rect);
    if rows < 2 && members.len() < 10 {
        return None;
    }
    let area: u32 = members.iter().map(|&m| comps[m].area).sum();
    let coverage = area as f32 / rect.area().max(1) as f32;
    if !(0.04..=0.6).contains(&coverage) {
        return None;
    }
    let labels_in: std::collections::HashSet<u32> = members.iter().map(|&m| comps[m].label).collect();
    let mut lum = 0.0f64;
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let i = (y * w + x) as usize;
            if labels_in.contains(&labels[i]) {
                lum += luma.data[i] as f64;
            }
        }
    }
    let mean_luma = (lum / area.max(1) as f64) as f32;

    let edge_h = rect.x.min(w - rect.right()) as f32 / w as f32;
    let e_edge_v = if edge_v <= 0.06 {
        1.0
    } else if edge_v <= 0.12 {
        0.75
    } else {
        0.5
    };
    let e_edge_h = if edge_h <= 0.08 {
        1.0
    } else if edge_h <= 0.2 {
        0.7
    } else {
        0.45
    };
    let e_rows = ((rows as f32 - 1.0) / 2.0).clamp(0.0, 1.0);
    let e_count = ((members.len() as f32 - 5.0) / 15.0).clamp(0.0, 1.0);
    let e_white = ((mean_luma - WHITE_LUMA) / 25.0).clamp(0.0, 1.0);
    let s = 0.3 * e_edge_v + 0.15 * e_edge_h + 0.2 * e_rows + 0.2 * e_count + 0.15 * e_white;
    let conf = (0.55 + 0.3 * s).min(HEURISTIC_MAX_CONFIDENCE);
    if conf < 0.62 {
        return None;
    }

    // 提示：白色笔画（闭运算补上标签内的深色小字）为 255，
    // 笔画外 2~3 px 的阴影与抗锯齿边为 150，块内的彩色笔画（分隔条、彩色标题）为 255。
    let mut glyph_h: Vec<f32> = members.iter().map(|&m| comps[m].rect.height as f32).collect();
    let pad = (ops::median(&mut glyph_h) * 0.3).clamp(3.0, 12.0) as u32;
    let prect = rect.pad(pad, w, h);
    let mut core = GrayU8::new(prect.width, prect.height);
    for y in prect.y..prect.bottom() {
        for x in prect.x..prect.right() {
            let i = (y * w + x) as usize;
            if labels_in.contains(&labels[i]) {
                core.set(x - prect.x, y - prect.y, 255);
            }
        }
    }
    let core = fill_enclosed(&ops::close(&core, 2.0));
    let ring = ops::dilate(&core, 2.5);
    let mut hint = GrayU8::new(prect.width, prect.height);
    for y in 0..prect.height {
        for x in 0..prect.width {
            let (gx, gy) = (prect.x + x, prect.y + y);
            let i = (gy * w + gx) as usize;
            let colored = rect.contains(gx, gy) && sat.data[i] >= 0.4 && (tb.data[i] >= 25.0 || td.data[i] >= 25.0);
            let v = if core.get(x, y) > 0 || colored {
                255
            } else if ring.get(x, y) > 0 {
                150
            } else {
                0
            };
            hint.set(x, y, v);
        }
    }

    let bbox = prect.to_bbox().to_normalized(w, h);
    let mut cand = WatermarkCandidate::new(wm_common::new_id(), WatermarkType::InfoStamp, conf, bbox, DetectorSource::InfoStamp);
    cand.evidence = vec![
        Evidence::new(DetectorSource::InfoStamp, "edge_band", e_edge_v),
        Evidence::new(DetectorSource::InfoStamp, "text_rows", e_rows),
        Evidence::new(DetectorSource::InfoStamp, "glyph_count", e_count),
        Evidence::new(DetectorSource::InfoStamp, "opaque_white_text", e_white),
    ];
    Some(Detection { candidate: cand, hint: Some(MaskHint { bbox, data: hint }) })
}

/// 填充被笔画完全包围的空洞：白色标签里的深色小字、字形内部的封闭区域。
/// 否则修复后标签里的字会留在原处，像幽灵一样浮在填充出的背景上。
fn fill_enclosed(m: &GrayU8) -> GrayU8 {
    let (w, h) = (m.width as usize, m.height as usize);
    if w == 0 || h == 0 {
        return m.clone();
    }
    let mut outside = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    for x in 0..w {
        stack.extend([x, (h - 1) * w + x]);
    }
    for y in 0..h {
        stack.extend([y * w, y * w + w - 1]);
    }
    while let Some(i) = stack.pop() {
        if outside[i] || m.data[i] > 0 {
            continue;
        }
        outside[i] = true;
        let (x, y) = (i % w, i / w);
        if x > 0 {
            stack.push(i - 1);
        }
        if x + 1 < w {
            stack.push(i + 1);
        }
        if y > 0 {
            stack.push(i - w);
        }
        if y + 1 < h {
            stack.push(i + w);
        }
    }
    GrayU8 { width: m.width, height: m.height, data: outside.iter().map(|&o| if o { 0 } else { 255 }).collect() }
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
        InfoStampDetector::default().detect(&input).unwrap()
    }

    /// 底部左侧：大号时间 + 下方一行小字，白色不透明、带深色阴影。
    fn stamp_photo(w: u32, h: u32, seed: u64) -> (ImageBuffer, PixelRect) {
        let mut img = synth::photo_like(w, h, seed);
        let mut t = synth::Overlay::new(w, h);
        let big = synth::render_text("09:47 2026/07/06", 5.0);
        let small = synth::render_text("SHANGHAI PUDONG ROAD 88", 2.5);
        let x = 24i64;
        let y_small = (h - small.height - 24) as i64;
        let y_big = y_small - big.height as i64 - 14;
        for (a, y) in [(&big, y_big), (&small, y_small)] {
            synth::overlay(&mut img, &mut t, a, x + 2, y + 2, [20, 20, 20], 0.8);
            synth::overlay(&mut img, &mut t, a, x, y, [255, 255, 255], 1.0);
        }
        (img, t.mask.nonzero_bounds().unwrap())
    }

    #[test]
    fn finds_bottom_time_and_location_block_for_review() {
        let (w, h) = (1200u32, 1600u32);
        let (img, truth) = stamp_photo(w, h, 7);
        let d = run(&img);
        let tb = truth.to_bbox().to_normalized(w, h);
        let hit = d.iter().find(|x| x.candidate.bbox.iou(&tb) > 0.5).expect("info block detected");
        assert_eq!(hit.candidate.watermark_type, WatermarkType::InfoStamp);
        assert!(hit.candidate.confidence <= HEURISTIC_MAX_CONFIDENCE);
        assert!(hit.candidate.confidence >= 0.7, "conf {}", hit.candidate.confidence);
        assert!(hit.hint.is_some());
    }

    #[test]
    fn white_text_in_the_middle_of_the_frame_is_not_a_stamp() {
        let (w, h) = (1200u32, 1600u32);
        let mut img = synth::photo_like(w, h, 9);
        let mut t = synth::Overlay::new(w, h);
        let a = synth::render_text("SHOP OPEN", 5.0);
        let b = synth::render_text("DAILY 9 TO 5", 3.0);
        synth::overlay(&mut img, &mut t, &a, 300, 700, [255, 255, 255], 1.0);
        synth::overlay(&mut img, &mut t, &b, 300, 700 + a.height as i64 + 12, [255, 255, 255], 1.0);
        assert!(run(&img).is_empty());
    }

    #[test]
    fn plain_photo_has_no_stamp() {
        for seed in [1, 2, 3] {
            let img = synth::photo_like(1000, 750, seed);
            assert!(run(&img).is_empty(), "seed {seed}");
        }
    }
}
