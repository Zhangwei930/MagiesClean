//! OCR 文字候选与 WatermarkTextClassifier（规格 §4.3）。
//!
//! 综合位置、透明度、重复程度、旋转、跨图片/跨页一致性、文字内容与边缘位置判断水印属性。
//! 检测到文字不代表可删除：普通路牌、衣服 Logo、包装文字、招牌与文件正文必须进入负样本测试。
//! 以下权重为可配置的初始值，需在验证集上校准，不能当作固定规则。

use std::sync::Arc;
use wm_core::traits::{Detection, DetectionInput, OcrEngine, OcrLine, WatermarkDetector};
use wm_core::{BoundingBox, DetectorSource, Evidence, Result, WatermarkCandidate, WatermarkType};

/// 分类上下文（由调用方统计）。
#[derive(Debug, Clone, Copy, Default)]
pub struct TextContext {
    /// 图片宽高（与 OcrLine.bbox 同一坐标系）。
    pub width: f32,
    pub height: f32,
    /// 同一文字在本批次其它文件 / 其它页出现的次数。
    pub repeat_count: usize,
    /// 估计的不透明度（0..1），未知为 None。
    pub opacity: Option<f32>,
}

const URL_MARKERS: &[&str] = &["www.", "http", ".com", ".cn", ".net", ".org", ".io", ".co", ".me", ".xyz", ".shop", ".top"];
const COPYRIGHT_MARKERS: &[&str] = &["©", "(c)", "copyright", "all rights reserved", "版权", "版權", "禁止转载", "未经授权", "仅供"];
const PLATFORM_MARKERS: &[&str] = &[
    "photo by",
    "photography",
    "摄影",
    "watermark",
    "水印",
    "水印相机",
    "打卡",
    "防伪",
    "真实时间",
    "sample",
    "preview",
    "proof",
    "shutterstock",
    "getty",
    "istock",
    "alamy",
    "dreamstime",
    "123rf",
    "depositphotos",
    "adobe stock",
    "小红书",
    "抖音",
    "快手",
    "微博",
    "weibo",
    "douyin",
    "xiaohongshu",
    "@",
    "id:",
    "id：",
];

/// 时间（09:47、9：05:30）或日期（2026/07/06、2026-7-6、2026.07.06、2026年7月6日）。
/// 相机、打卡类应用叠加的时间戳最常见的形式；单独出现的普通数字不算。
pub fn is_time_or_date(text: &str) -> bool {
    let c: Vec<char> = text.chars().collect();
    let digits = |i: usize, n: usize| i + n <= c.len() && c[i..i + n].iter().all(|d| d.is_ascii_digit());
    let digits_1_2 = |i: usize| -> Option<usize> {
        if digits(i, 2) {
            Some(2)
        } else if digits(i, 1) {
            Some(1)
        } else {
            None
        }
    };
    for i in 0..c.len() {
        if i > 0 && c[i - 1].is_ascii_digit() {
            continue;
        }
        // 时间：H(H):MM
        if let Some(n) = digits_1_2(i) {
            if matches!(c.get(i + n), Some(':') | Some('：')) && digits(i + n + 1, 2) {
                return true;
            }
        }
        // 日期：YYYY sep M(M) sep D(D)
        if digits(i, 4) {
            let j = i + 4;
            if let Some(sep) = c.get(j).copied().filter(|s| matches!(s, '/' | '-' | '.' | '年')) {
                if let Some(m) = digits_1_2(j + 1) {
                    let k = j + 1 + m;
                    let sep2_ok = match sep {
                        '年' => c.get(k) == Some(&'月'),
                        _ => c.get(k) == Some(&sep),
                    };
                    if sep2_ok && digits_1_2(k + 1).is_some() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// 返回 (水印概率, 证据列表)。
pub fn classify_text(line: &OcrLine, ctx: &TextContext) -> (f32, Vec<Evidence>) {
    let text = line.text.trim();
    let lower = text.to_lowercase();
    let mut ev = Vec::new();
    let mut score = 0.15f32;
    let mut add = |name: &str, v: f32, w: f32, score: &mut f32| {
        if v > 0.0 {
            ev.push(Evidence::new(DetectorSource::TextOcr, name, v));
            *score += v * w;
        }
    };
    let has = |ms: &[&str]| ms.iter().any(|m| lower.contains(m));
    add("url_like", has(URL_MARKERS) as u8 as f32, 0.35, &mut score);
    add("copyright", has(COPYRIGHT_MARKERS) as u8 as f32, 0.35, &mut score);
    add("platform_or_username", has(PLATFORM_MARKERS) as u8 as f32, 0.3, &mut score);
    add("time_or_date", is_time_or_date(text) as u8 as f32, 0.3, &mut score);

    if ctx.width > 0.0 && ctx.height > 0.0 {
        let cx = (line.bbox.x + line.bbox.width / 2.0) / ctx.width;
        let cy = (line.bbox.y + line.bbox.height / 2.0) / ctx.height;
        let ex = cx.min(1.0 - cx);
        let ey = cy.min(1.0 - cy);
        let pos = if ex < 0.2 && ey < 0.2 {
            1.0
        } else if ex < 0.1 || ey < 0.1 {
            0.6
        } else {
            0.0
        };
        add("edge_position", pos, 0.15, &mut score);
    }
    let rot = line.rotation.abs() % 180.0;
    let tilted = rot > 10.0 && rot < 170.0;
    add("rotated", tilted as u8 as f32, 0.1, &mut score);
    add("repeated", ((ctx.repeat_count as f32 - 1.0) / 3.0).clamp(0.0, 1.0), 0.2, &mut score);
    if let Some(o) = ctx.opacity {
        add("semi_transparent", ((0.85 - o) / 0.5).clamp(0.0, 1.0), 0.15, &mut score);
    }
    // 长句且没有任何水印标记：更像正文 / 招牌
    let chars = text.chars().count();
    let markers = ev.iter().any(|e| matches!(e.name.as_str(), "url_like" | "copyright" | "platform_or_username" | "time_or_date"));
    if chars > 40 && !markers {
        score -= 0.25;
        ev.push(Evidence::new(DetectorSource::TextOcr, "long_body_text", 1.0));
    }
    (score.clamp(0.0, 0.98), ev)
}

/// 基于 OCR 引擎的文字水印检测器。OCR 模型未安装时不注册该检测器。
pub struct TextOcrDetector {
    pub engine: Arc<dyn OcrEngine>,
    pub min_score: f32,
}

impl WatermarkDetector for TextOcrDetector {
    fn name(&self) -> &'static str {
        "text-ocr"
    }
    fn source(&self) -> DetectorSource {
        DetectorSource::TextOcr
    }
    fn detect(&self, input: &DetectionInput) -> Result<Vec<Detection>> {
        let lines = self.engine.recognize(input.image)?;
        let (w, h) = (input.image.width as f32, input.image.height as f32);
        let mut out = Vec::new();
        for l in &lines {
            let repeat = lines.iter().filter(|o| o.text.trim() == l.text.trim()).count();
            let ctx = TextContext { width: w, height: h, repeat_count: repeat, opacity: None };
            let (p, ev) = classify_text(l, &ctx);
            if p < self.min_score {
                continue;
            }
            let bbox = BoundingBox::new(l.bbox.x / w, l.bbox.y / h, l.bbox.width / w, l.bbox.height / h);
            let mut c = WatermarkCandidate::new(
                wm_common::new_id(),
                WatermarkType::Text,
                p * l.score.clamp(0.5, 1.0),
                bbox,
                DetectorSource::TextOcr,
            );
            c.text = Some(l.text.clone());
            c.rotation = l.rotation;
            c.evidence = ev;
            out.push(Detection::new(c));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(t: &str, x: f32, y: f32) -> OcrLine {
        OcrLine { text: t.into(), bbox: BoundingBox::new(x, y, 120.0, 20.0), rotation: 0.0, score: 0.95 }
    }
    const CTX: TextContext = TextContext { width: 1000.0, height: 800.0, repeat_count: 1, opacity: None };

    #[test]
    fn time_and_date_formats_are_recognized() {
        for t in ["09:47", "9：05", "2026/07/06", "2026-7-6", "2026.07.06", "2026年7月6日 星期一", "拍摄于 2026/07/06 09:47"] {
            assert!(is_time_or_date(t), "{t}");
        }
        for t in ["蒙G·83888", "500HP", "12345", "2026", "1:2", "v1.2.3"] {
            assert!(!is_time_or_date(t), "{t}");
        }
    }

    #[test]
    fn edge_timestamp_is_a_candidate_but_center_plate_is_not() {
        let (p, _) = classify_text(&line("2026/07/06", 380.0, 740.0), &CTX);
        assert!(p >= 0.5, "p={p}");
        let (p, _) = classify_text(&line("蒙G·83888", 450.0, 400.0), &CTX);
        assert!(p < 0.3, "p={p}");
    }

    #[test]
    fn corner_url_is_watermark() {
        let (p, _) = classify_text(&line("www.myshop.com", 860.0, 760.0), &CTX);
        assert!(p >= 0.6, "p={p}");
    }

    #[test]
    fn copyright_username_scores_high() {
        let (p, _) = classify_text(&line("© @lens_master", 20.0, 20.0), &CTX);
        assert!(p >= 0.85, "p={p}");
    }

    #[test]
    fn road_sign_in_center_is_low() {
        let (p, _) = classify_text(&line("STOP", 450.0, 380.0), &CTX);
        assert!(p < 0.3, "p={p}");
    }

    #[test]
    fn body_text_is_penalized() {
        let (p, _) =
            classify_text(&line("The committee reviewed the quarterly results and approved the budget for next year.", 50.0, 400.0), &CTX);
        assert!(p < 0.1, "p={p}");
    }

    #[test]
    fn repeated_rotated_semi_transparent_text_is_high() {
        let mut l = line("CONFIDENTIAL", 400.0, 380.0);
        l.rotation = 45.0;
        let ctx = TextContext { repeat_count: 6, opacity: Some(0.2), ..CTX };
        let (p, _) = classify_text(&l, &ctx);
        assert!(p >= 0.55, "p={p}");
    }
}
