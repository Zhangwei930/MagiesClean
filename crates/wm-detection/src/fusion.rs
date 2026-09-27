//! Candidate Fusion + Confidence Engine（规格 §4.2）。
//!
//! 多个 Detector 统一输出候选，保留来源、空间范围、旋转与各类证据。
//! 重叠候选去重合并；置信度按来源校准权重做 noisy-OR 融合——独立证据相互印证时提高置信度，
//! 单一来源不会被放大。校准权重必须在验证集上确定，这里给出可配置默认值。

use std::collections::HashMap;
use wm_core::traits::Detection;
use wm_core::{DetectorSource, WatermarkType};

#[derive(Debug, Clone)]
pub struct FusionConfig {
    /// 各来源的校准系数（0..1）：原始分数 × 系数 后参与融合。
    pub calibration: HashMap<DetectorSource, f32>,
    pub iou_merge: f32,
    pub containment_merge: f32,
    pub max_confidence: f32,
}

impl Default for FusionConfig {
    fn default() -> Self {
        let calibration = [
            (DetectorSource::PdfNative, 1.0),
            (DetectorSource::BatchPersistence, 1.0),
            (DetectorSource::AiDetector, 0.95),
            (DetectorSource::TextOcr, 0.9),
            (DetectorSource::RepeatedPattern, 1.0),
            (DetectorSource::OverlayHeuristic, 1.0),
            (DetectorSource::InfoStamp, 1.0),
            (DetectorSource::Manual, 1.0),
        ]
        .into_iter()
        .collect();
        Self { calibration, iou_merge: 0.3, containment_merge: 0.7, max_confidence: 0.99 }
    }
}

fn type_priority(t: WatermarkType) -> u8 {
    match t {
        WatermarkType::PdfNative => 7,
        WatermarkType::InfoStamp => 6,
        WatermarkType::Repeated => 5,
        WatermarkType::Transparent => 4,
        WatermarkType::Logo => 3,
        WatermarkType::Text => 2,
        WatermarkType::Unknown => 1,
    }
}

fn hint_priority(s: DetectorSource) -> u8 {
    match s {
        DetectorSource::Manual => 8,
        DetectorSource::BatchPersistence => 7,
        DetectorSource::AiDetector => 6,
        DetectorSource::RepeatedPattern => 5,
        DetectorSource::InfoStamp => 4,
        DetectorSource::TextOcr => 3,
        DetectorSource::OverlayHeuristic => 2,
        DetectorSource::PdfNative => 1,
    }
}

/// 只基于同一组像素特征的启发式来源（彼此高度相关）。
fn is_heuristic(s: DetectorSource) -> bool {
    matches!(s, DetectorSource::OverlayHeuristic | DetectorSource::RepeatedPattern | DetectorSource::InfoStamp)
}

/// 置信度融合：以最高分为基础，其余来源按独立程度加权补充。
/// 两个启发式来源观察的是同一组笔画，彼此不独立，只给很小的增益；
/// 批次特征、PDF 结构、模型、OCR 等独立证据的增益更大。
fn combine(scores: &HashMap<DetectorSource, f32>) -> f32 {
    let mut v: Vec<(DetectorSource, f32)> = scores.iter().map(|(k, s)| (*k, s.clamp(0.0, 1.0))).collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let Some(&(first_src, mut p)) = v.first() else { return 0.0 };
    let mut seen = vec![first_src];
    for &(src, s) in v.iter().skip(1) {
        let correlated = is_heuristic(src) && seen.iter().all(|x| is_heuristic(*x));
        let w = if correlated { 0.15 } else { 0.6 };
        p += (1.0 - p) * s * w;
        seen.push(src);
    }
    // 仅有启发式证据时不超过 0.9
    if seen.iter().all(|x| is_heuristic(*x)) {
        p = p.min(0.9);
    }
    p
}

/// 融合候选。输入可来自多个检测器；输出按置信度降序。
pub fn fuse(dets: Vec<Detection>, cfg: &FusionConfig) -> Vec<Detection> {
    // (检测, 各来源的校准分, 当前 Mask 提示的来源)
    let mut out: Vec<(Detection, HashMap<DetectorSource, f32>, Option<DetectorSource>)> = Vec::new();
    for d in dets {
        let src = d.candidate.primary_source();
        let calibrated = d.candidate.confidence * cfg.calibration.get(&src).copied().unwrap_or(1.0);
        // 不同页的候选不合并
        let target = out.iter_mut().find(|(o, _, _)| {
            let (a, b) = (&o.candidate.bbox, &d.candidate.bbox);
            // 包含关系只在尺度相近时合并：全图平铺水印与角落的小水印是两个不同的对象
            let area_ratio = a.area().min(b.area()) / a.area().max(b.area()).max(1e-9);
            // 信息块与其内部的文字行 / 叠加文字碎片由 absorb_into_info_stamps 统一处理
            let (os, ds) = (o.candidate.watermark_type == WatermarkType::InfoStamp, d.candidate.watermark_type == WatermarkType::InfoStamp);
            let stamp_part = (os && !ds && absorbable(&d.candidate)) || (ds && !os && absorbable(&o.candidate));
            !stamp_part
                && o.candidate.page == d.candidate.page
                && (a.iou(b) >= cfg.iou_merge || (a.containment(b) >= cfg.containment_merge && area_ratio >= 0.25))
        });
        match target {
            Some((o, scores, hint_src)) => {
                let oc = &mut o.candidate;
                let dc = d.candidate;
                // 同一来源的重复检测：取较大者，不叠加
                if !oc.detector_sources.contains(&src) {
                    oc.detector_sources.push(src);
                }
                let e = scores.entry(src).or_insert(0.0);
                *e = e.max(calibrated);
                oc.bbox = oc.bbox.union(&dc.bbox);
                if type_priority(dc.watermark_type) > type_priority(oc.watermark_type) {
                    oc.watermark_type = dc.watermark_type;
                }
                oc.evidence.extend(dc.evidence);
                oc.repeat_score = oc.repeat_score.max(dc.repeat_score);
                oc.batch_score = oc.batch_score.max(dc.batch_score);
                oc.text = oc.text.take().or(dc.text);
                oc.opacity = oc.opacity.or(dc.opacity);
                oc.pdf_object_ref = oc.pdf_object_ref.take().or(dc.pdf_object_ref);
                oc.batch_profile_id = oc.batch_profile_id.take().or(dc.batch_profile_id);
                // 采用优先级更高来源的 Mask 提示
                let keep_new = d.hint.is_some() && hint_src.is_none_or(|h| hint_priority(src) > hint_priority(h));
                if keep_new {
                    o.hint = d.hint;
                    *hint_src = Some(src);
                    oc.rotation = dc.rotation;
                }
            }
            None => {
                let hs = d.hint.is_some().then_some(src);
                out.push((d, HashMap::from([(src, calibrated)]), hs));
            }
        }
    }
    let mut fused: Vec<Detection> = out
        .into_iter()
        .map(|(mut d, scores, _)| {
            d.candidate.confidence = combine(&scores).min(cfg.max_confidence);
            d
        })
        .collect();
    absorb_into_info_stamps(&mut fused);
    fused.sort_by(|a, b| b.candidate.confidence.partial_cmp(&a.candidate.confidence).unwrap_or(std::cmp::Ordering::Equal));
    fused
}

/// 只有单图叠加层分析或文字识别证据的候选：可被信息块吸收。
fn absorbable(c: &wm_core::WatermarkCandidate) -> bool {
    c.detector_sources.iter().all(|s| matches!(s, DetectorSource::OverlayHeuristic | DetectorSource::TextOcr))
}

/// 信息块内部的叠加文字碎片、识别出的文字行与信息块是同一组笔画：由信息块整体处理，不再单独列出。
/// 文字识别是独立证据：被吸收的文字行提高信息块的置信度，识别出的文字按阅读顺序记到信息块上。
/// 带有其它独立证据（批次、模型等）的候选保留。
fn absorb_into_info_stamps(fused: &mut Vec<Detection>) {
    let stamps: Vec<usize> = (0..fused.len()).filter(|&i| fused[i].candidate.watermark_type == WatermarkType::InfoStamp).collect();
    if stamps.is_empty() {
        return;
    }
    let mut absorbed = vec![false; fused.len()];
    for &si in &stamps {
        let (sb, page) = (fused[si].candidate.bbox, fused[si].candidate.page);
        let mut texts: Vec<(f32, f32, String)> = Vec::new();
        let mut ocr_best = 0.0f32;
        for (i, d) in fused.iter().enumerate() {
            let c = &d.candidate;
            if i == si || absorbed[i] || c.watermark_type == WatermarkType::InfoStamp || !absorbable(c) {
                continue;
            }
            if c.page != page || sb.area() < c.bbox.area() || sb.containment(&c.bbox) < 0.6 {
                continue;
            }
            absorbed[i] = true;
            if c.detector_sources.contains(&DetectorSource::TextOcr) {
                ocr_best = ocr_best.max(c.confidence);
                if let Some(t) = &c.text {
                    texts.push((c.bbox.y, c.bbox.x, t.clone()));
                }
            }
        }
        let s = &mut fused[si].candidate;
        if ocr_best > 0.0 {
            // 与 combine() 中独立来源的权重一致
            s.confidence = (s.confidence + (1.0 - s.confidence) * ocr_best * 0.6).min(0.95);
            if !s.detector_sources.contains(&DetectorSource::TextOcr) {
                s.detector_sources.push(DetectorSource::TextOcr);
            }
            s.evidence.push(wm_core::Evidence::new(DetectorSource::TextOcr, "recognized_text", ocr_best));
        }
        if !texts.is_empty() && s.text.is_none() {
            // 同一行（纵向相差不到 3% 图高）按从左到右
            texts.sort_by(|a, b| {
                if (a.0 - b.0).abs() < 0.03 {
                    a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                }
            });
            s.text = Some(texts.into_iter().map(|t| t.2).collect::<Vec<_>>().join(" "));
        }
    }
    let mut i = 0;
    fused.retain(|_| {
        let keep = !absorbed[i];
        i += 1;
        keep
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_core::{BoundingBox, WatermarkCandidate};

    fn det(conf: f32, b: BoundingBox, s: DetectorSource, t: WatermarkType) -> Detection {
        Detection::new(WatermarkCandidate::new(wm_common::new_id(), t, conf, b, s))
    }

    #[test]
    fn overlay_fragments_inside_an_info_stamp_are_absorbed() {
        let stamp = BoundingBox::new(0.02, 0.85, 0.5, 0.13);
        let out = fuse(
            vec![
                det(0.8, stamp, DetectorSource::InfoStamp, WatermarkType::InfoStamp),
                det(0.74, BoundingBox::new(0.15, 0.86, 0.05, 0.03), DetectorSource::OverlayHeuristic, WatermarkType::Text),
                det(0.74, BoundingBox::new(0.6, 0.2, 0.05, 0.03), DetectorSource::OverlayHeuristic, WatermarkType::Text),
            ],
            &FusionConfig::default(),
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].candidate.watermark_type, WatermarkType::InfoStamp);
        assert!(out.iter().any(|d| d.candidate.bbox.y < 0.5), "fragment outside the stamp is kept");
    }

    #[test]
    fn recognized_text_inside_an_info_stamp_raises_its_confidence() {
        let stamp = BoundingBox::new(0.02, 0.85, 0.5, 0.13);
        let mut t1 = det(0.6, BoundingBox::new(0.03, 0.86, 0.2, 0.06), DetectorSource::TextOcr, WatermarkType::Text);
        t1.candidate.text = Some("09:47".into());
        let mut t2 = det(0.6, BoundingBox::new(0.3, 0.86, 0.18, 0.03), DetectorSource::TextOcr, WatermarkType::Text);
        t2.candidate.text = Some("2026/07/06".into());
        let out = fuse(vec![det(0.82, stamp, DetectorSource::InfoStamp, WatermarkType::InfoStamp), t2, t1], &FusionConfig::default());
        assert_eq!(out.len(), 1);
        let c = &out[0].candidate;
        assert!(c.confidence >= 0.85, "{}", c.confidence);
        assert_eq!(c.text.as_deref(), Some("09:47 2026/07/06"));
        assert!(c.detector_sources.contains(&DetectorSource::TextOcr));
    }

    #[test]
    fn independent_sources_reinforce() {
        let b = BoundingBox::new(0.7, 0.9, 0.2, 0.05);
        let out = fuse(
            vec![
                det(0.8, b, DetectorSource::OverlayHeuristic, WatermarkType::Text),
                det(0.8, BoundingBox::new(0.71, 0.9, 0.2, 0.05), DetectorSource::BatchPersistence, WatermarkType::Transparent),
            ],
            &FusionConfig::default(),
        );
        assert_eq!(out.len(), 1);
        let c = &out[0].candidate;
        assert!(c.confidence > 0.88, "{}", c.confidence);
        assert_eq!(c.watermark_type, WatermarkType::Transparent);
        assert_eq!(c.detector_sources.len(), 2);
    }

    #[test]
    fn same_source_duplicates_do_not_inflate() {
        let b = BoundingBox::new(0.1, 0.1, 0.2, 0.05);
        let out = fuse(
            vec![
                det(0.7, b, DetectorSource::OverlayHeuristic, WatermarkType::Text),
                det(0.72, b, DetectorSource::OverlayHeuristic, WatermarkType::Text),
            ],
            &FusionConfig::default(),
        );
        assert_eq!(out.len(), 1);
        assert!((out[0].candidate.confidence - 0.72).abs() < 1e-5);
    }

    #[test]
    fn correlated_heuristics_barely_reinforce() {
        let b = BoundingBox::new(0.4, 0.4, 0.2, 0.1);
        let out = fuse(
            vec![
                det(0.7, b, DetectorSource::RepeatedPattern, WatermarkType::Repeated),
                det(0.65, b, DetectorSource::OverlayHeuristic, WatermarkType::Text),
            ],
            &FusionConfig::default(),
        );
        assert!(out[0].candidate.confidence < 0.8, "{}", out[0].candidate.confidence);
    }

    #[test]
    fn small_candidate_inside_full_image_tile_stays_separate() {
        let out = fuse(
            vec![
                det(0.9, BoundingBox::new(0.0, 0.0, 1.0, 1.0), DetectorSource::RepeatedPattern, WatermarkType::Repeated),
                det(0.9, BoundingBox::new(0.8, 0.9, 0.15, 0.05), DetectorSource::BatchPersistence, WatermarkType::Text),
            ],
            &FusionConfig::default(),
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn distant_candidates_stay_separate() {
        let out = fuse(
            vec![
                det(0.9, BoundingBox::new(0.0, 0.0, 0.1, 0.1), DetectorSource::OverlayHeuristic, WatermarkType::Text),
                det(0.9, BoundingBox::new(0.8, 0.8, 0.1, 0.1), DetectorSource::OverlayHeuristic, WatermarkType::Text),
            ],
            &FusionConfig::default(),
        );
        assert_eq!(out.len(), 2);
    }
}
