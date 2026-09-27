//! 文字检测 + 识别 Adapter（PaddleOCR PP-OCR 系列 ONNX 契约）。
//!
//! - 检测（DB）：输入 `x [1,3,H,W]`（BGR，ImageNet 均值方差归一化，H/W 为 32 的倍数），
//!   输出 `[1,1,H,W]` 文字概率图；二值化 → 连通域 → 按框内平均概率过滤 → 按 DB 公式外扩。
//! - 识别（CTC）：输入 `x [N,3,48,W]`（BGR，归一化到 -1..1，右侧补 0），
//!   输出 `[N,T,C]` 各时间步的类别概率；类别 0 为空白，1..=字库长度 为字库字符，其后一类为空格。
//!
//! 框为轴对齐矩形：水印文字大多水平；斜向平铺文字识别不出时置信度低，不会产生候选。

use crate::ModelRuntime;
use std::sync::Arc;
use wm_core::geometry::BoundingBox;
use wm_core::traits::{OcrEngine, OcrLine};
use wm_core::{msg, AppError, GrayU8, ImageBuffer, PixelRect, Result, Tensor};
use wm_image::ops;

/// 检测参数（与 PaddleOCR 默认值一致）。
const DET_THRESH: f32 = 0.3;
const DET_BOX_THRESH: f32 = 0.6;
const DET_UNCLIP: f32 = 1.5;
const DET_MIN_SIZE: f32 = 3.0;
/// 识别：每批行数、单行最大输入宽度、最低平均置信度。
const REC_BATCH: usize = 6;
const REC_MAX_W: u32 = 1600;
const REC_MIN_SCORE: f32 = 0.5;

pub struct OnnxOcr {
    pub det: Arc<dyn ModelRuntime>,
    pub rec: Arc<dyn ModelRuntime>,
    /// 字库（不含空白类；最后一类空格由模型输出维度推断）。
    pub dict: Vec<String>,
    /// 检测输入最长边。
    pub det_limit: u32,
    /// 识别输入高度与最小宽度。
    pub rec_h: u32,
    pub rec_min_w: u32,
}

impl OnnxOcr {
    /// 读取字库：每行一个字符。
    pub fn load_dict(text: &str) -> Vec<String> {
        text.lines().map(|l| l.trim_end_matches('\r').to_string()).collect()
    }

    fn input_name(rt: &Arc<dyn ModelRuntime>) -> String {
        rt.input_names().first().cloned().unwrap_or_else(|| "x".into())
    }

    /// 检测：返回输入图坐标下的文字框。
    fn detect_boxes(&self, img: &ImageBuffer) -> Result<Vec<PixelRect>> {
        let (w, h) = (img.width, img.height);
        let s = (self.det_limit as f32 / w.max(h) as f32).min(1.0);
        let rw = (((w as f32 * s) / 32.0).round() as u32 * 32).max(32);
        let rh = (((h as f32 * s) / 32.0).round() as u32 * 32).max(32);
        let small = ops::resize(img, rw, rh);
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
        let plane = (rw * rh) as usize;
        let mut t = Tensor::zeros(vec![1, 3, rh as usize, rw as usize]);
        for i in 0..plane {
            for c in 0..3 {
                // BGR 通道顺序
                let v = small.data[i * 4 + (2 - c)] as f32 / 255.0;
                t.data[c * plane + i] = (v - MEAN[c]) / STD[c];
            }
        }
        let out = self.det.run(vec![(Self::input_name(&self.det), t)])?;
        let prob =
            out.first().ok_or_else(|| AppError::inference(msg!("文字检测模型没有输出", "The text detection model returned no output")))?;
        if prob.data.len() != plane {
            return Err(AppError::inference(msg!(
                "文字检测模型输出不符合 [1,1,H,W] 契约",
                "Text detection output does not match the [1,1,H,W] contract"
            )));
        }
        let boxes = db_boxes(&prob.data, rw, rh);
        let (sx, sy) = (w as f32 / rw as f32, h as f32 / rh as f32);
        Ok(boxes
            .into_iter()
            .filter_map(|(x0, y0, x1, y1)| {
                let r = BoundingBox::from_corners(x0 * sx, y0 * sy, x1 * sx, y1 * sy).clamp_to(w as f32, h as f32);
                (r.width >= DET_MIN_SIZE + 2.0 && r.height >= DET_MIN_SIZE + 2.0).then(|| r.to_pixel_rect(w, h))
            })
            .collect())
    }

    /// 识别一批裁剪好的行图（已按需旋转为横排）。
    fn recognize_batch(&self, crops: &[ImageBuffer]) -> Result<Vec<(String, f32)>> {
        let h = self.rec_h;
        let ratio = crops.iter().map(|c| c.width as f32 / c.height.max(1) as f32).fold(self.rec_min_w as f32 / h as f32, f32::max);
        let bw = ((h as f32 * ratio).ceil() as u32).min(REC_MAX_W);
        let n = crops.len();
        let plane = (bw * h) as usize;
        let mut t = Tensor::zeros(vec![n, 3, h as usize, bw as usize]);
        for (k, c) in crops.iter().enumerate() {
            let rw = ((h as f32 * c.width as f32 / c.height.max(1) as f32).ceil() as u32).clamp(1, bw);
            let r = ops::resize(c, rw, h);
            for y in 0..h {
                for x in 0..rw {
                    let i = r.idx(x, y);
                    for ch in 0..3 {
                        let v = r.data[i + (2 - ch)] as f32 / 255.0;
                        t.data[k * 3 * plane + ch * plane + (y * bw + x) as usize] = (v - 0.5) / 0.5;
                    }
                }
            }
        }
        let out = self.rec.run(vec![(Self::input_name(&self.rec), t)])?;
        let p = out
            .first()
            .ok_or_else(|| AppError::inference(msg!("文字识别模型没有输出", "The text recognition model returned no output")))?;
        if p.shape.len() != 3 || p.shape[0] != n {
            return Err(AppError::inference(msg!(
                "文字识别模型输出不符合 [N,T,C] 契约",
                "Text recognition output does not match the [N,T,C] contract"
            )));
        }
        let (steps, classes) = (p.shape[1], p.shape[2]);
        Ok((0..n).map(|k| ctc_decode(&p.data[k * steps * classes..(k + 1) * steps * classes], steps, classes, &self.dict)).collect())
    }
}

impl OcrEngine for OnnxOcr {
    fn recognize(&self, image: &ImageBuffer) -> Result<Vec<OcrLine>> {
        let boxes = self.detect_boxes(image)?;
        // 竖长框旋转为横排；按宽高比排序后分批，批内补齐宽度更少
        let mut items: Vec<(PixelRect, bool, ImageBuffer)> = boxes
            .into_iter()
            .map(|r| {
                let crop = image.crop(&r);
                let vertical = r.height as f32 >= r.width as f32 * 1.5;
                let crop = if vertical { rotate_ccw(&crop) } else { crop };
                (r, vertical, crop)
            })
            .collect();
        items.sort_by(|a, b| {
            let ra = a.2.width as f32 / a.2.height.max(1) as f32;
            let rb = b.2.width as f32 / b.2.height.max(1) as f32;
            ra.partial_cmp(&rb).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut lines = Vec::new();
        for chunk in items.chunks(REC_BATCH) {
            let crops: Vec<ImageBuffer> = chunk.iter().map(|x| x.2.clone()).collect();
            for ((r, vertical, _), (text, score)) in chunk.iter().zip(self.recognize_batch(&crops)?) {
                if text.trim().is_empty() || score < REC_MIN_SCORE {
                    continue;
                }
                lines.push(OcrLine {
                    text,
                    bbox: BoundingBox::new(r.x as f32, r.y as f32, r.width as f32, r.height as f32),
                    rotation: if *vertical { 90.0 } else { 0.0 },
                    score,
                });
            }
        }
        // 按阅读顺序输出
        lines.sort_by(|a, b| (a.bbox.y, a.bbox.x).partial_cmp(&(b.bbox.y, b.bbox.x)).unwrap_or(std::cmp::Ordering::Equal));
        Ok(lines)
    }
}

/// DB 后处理：概率图 → 文字框（概率图坐标，x0,y0,x1,y1）。
pub fn db_boxes(prob: &[f32], w: u32, h: u32) -> Vec<(f32, f32, f32, f32)> {
    let bitmap = GrayU8 { width: w, height: h, data: prob.iter().map(|&p| if p > DET_THRESH { 255 } else { 0 }).collect() };
    let (labels, comps) = ops::connected_components(&bitmap);
    let mut sum = vec![0.0f32; comps.len()];
    for (i, &l) in labels.iter().enumerate() {
        if l > 0 {
            sum[l as usize - 1] += prob[i];
        }
    }
    let mut out = Vec::new();
    for (k, c) in comps.iter().enumerate() {
        let (bw, bh) = (c.rect.width as f32, c.rect.height as f32);
        if bw.min(bh) < DET_MIN_SIZE || sum[k] / (c.area.max(1) as f32) < DET_BOX_THRESH {
            continue;
        }
        // DB unclip：按 面积 × 比例 / 周长 向外扩
        let d = bw * bh * DET_UNCLIP / (2.0 * (bw + bh));
        out.push((c.rect.x as f32 - d, c.rect.y as f32 - d, c.rect.right() as f32 + d, c.rect.bottom() as f32 + d));
    }
    out
}

/// CTC 贪心解码：返回（文字，所选字符的平均概率）。
pub fn ctc_decode(p: &[f32], steps: usize, classes: usize, dict: &[String]) -> (String, f32) {
    let mut text = String::new();
    let mut probs = Vec::new();
    let mut prev = 0usize;
    for t in 0..steps {
        let row = &p[t * classes..(t + 1) * classes];
        let (idx, &pr) =
            row.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or((0, &0.0));
        if idx != 0 && idx != prev {
            match dict.get(idx - 1) {
                Some(ch) => text.push_str(ch),
                // 字库之后的一类是空格
                None => text.push(' '),
            }
            probs.push(pr);
        }
        prev = idx;
    }
    let score = if probs.is_empty() { 0.0 } else { probs.iter().sum::<f32>() / probs.len() as f32 };
    (text, score)
}

/// 逆时针旋转 90°（竖排文字转横排）。
fn rotate_ccw(img: &ImageBuffer) -> ImageBuffer {
    let mut out = ImageBuffer::new(img.height, img.width);
    for y in 0..img.height {
        for x in 0..img.width {
            let s = img.idx(x, y);
            let d = out.idx(y, img.width - 1 - x);
            out.data[d..d + 4].copy_from_slice(&img.data[s..s + 4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctc_merges_repeats_and_drops_blanks() {
        let dict: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        // 类别：0 空白、1 a、2 b、3 空格；序列 a a 空白 a b 空格 b
        let seq = [1, 1, 0, 1, 2, 3, 2];
        let classes = 4;
        let mut p = vec![0.0f32; seq.len() * classes];
        for (t, &c) in seq.iter().enumerate() {
            p[t * classes + c] = 0.9;
        }
        let (text, score) = ctc_decode(&p, seq.len(), classes, &dict);
        assert_eq!(text, "aab b");
        assert!((score - 0.9).abs() < 1e-5);
    }

    #[test]
    fn db_boxes_are_filtered_and_unclipped() {
        let (w, h) = (100u32, 60u32);
        let mut prob = vec![0.0f32; (w * h) as usize];
        // 一块高概率文字区与一块低概率噪声
        for y in 20..30 {
            for x in 10..70 {
                prob[(y * w + x) as usize] = 0.9;
            }
        }
        for y in 45..50 {
            for x in 80..90 {
                prob[(y * w + x) as usize] = 0.35;
            }
        }
        let b = db_boxes(&prob, w, h);
        assert_eq!(b.len(), 1);
        let (x0, y0, x1, y1) = b[0];
        assert!(x0 < 10.0 && y0 < 20.0 && x1 > 70.0 && y1 > 30.0, "{:?}", b[0]);
    }

    #[test]
    fn dict_keeps_space_like_entries() {
        let d = OnnxOcr::load_dict("a\r\n'\n疗\n");
        assert_eq!(d, vec!["a", "'", "疗"]);
    }
}
