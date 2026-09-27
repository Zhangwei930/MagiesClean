//! RepeatedPatternDetector（规格 §6.2）：重复文字或 45° 倾斜平铺水印。
//!
//! 1. 高通响应的二维自相关（FFT）→ 周期峰值 → 平铺晶格 (v1, v2)；
//! 2. 晶格一致性：对每个像素取 p + a·v1 + b·v2 处高通响应的 **中值**——平铺水印的笔画在所有
//!    平移位置一致出现而被保留，图像内容随平移变化而被抑制；
//! 3. 规则纹理本身不等于水印：结合稀疏度、低饱和度、极性一致性与重复次数等证据给出置信度。

use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;
use wm_core::traits::{Detection, DetectionInput, MaskHint, WatermarkDetector};
use wm_core::{DetectorSource, Evidence, GrayF32, GrayU8, Result, WatermarkCandidate, WatermarkType};
use wm_image::ops;

pub struct RepeatedPatternDetector {
    pub canvas: u32,
}

impl Default for RepeatedPatternDetector {
    fn default() -> Self {
        Self { canvas: 512 }
    }
}

#[derive(Debug, Clone, Copy)]
struct Peak {
    dx: i32,
    dy: i32,
    rho: f32,
}

impl WatermarkDetector for RepeatedPatternDetector {
    fn name(&self) -> &'static str {
        "repeated-pattern"
    }
    fn source(&self) -> DetectorSource {
        DetectorSource::RepeatedPattern
    }

    fn detect(&self, input: &DetectionInput) -> Result<Vec<Detection>> {
        let (img, _) = ops::thumbnail(input.image, self.canvas);
        let (w, h) = (img.width as usize, img.height as usize);
        if w < 96 || h < 96 {
            return Ok(Vec::new());
        }
        let luma = img.to_luma_f32();
        // 带符号高通：像素相对局部均值的偏差
        let blur = ops::box_blur(&luma, 6);
        let hp: Vec<f32> = luma.data.iter().zip(&blur.data).map(|(a, b)| a - b).collect();
        let energy: Vec<f32> = hp.iter().map(|v| v.abs()).collect();

        input.cancel.check()?;
        let ac = autocorr(&energy, w, h);
        let min_period = 14i32;
        let peaks = find_peaks(&ac, w, h, min_period);
        let Some(v1) = peaks.first().copied() else {
            return Ok(Vec::new());
        };
        if v1.rho < 0.18 {
            return Ok(Vec::new());
        }
        let v2 = peaks
            .iter()
            .skip(1)
            .find(|p| {
                let cross = (v1.dx * p.dy - v1.dy * p.dx) as f32;
                let norm = ((v1.dx.pow(2) + v1.dy.pow(2)) as f32).sqrt() * ((p.dx.pow(2) + p.dy.pow(2)) as f32).sqrt();
                cross.abs() > 0.35 * norm && p.rho > 0.12
            })
            .copied();

        // 晶格平移集合
        let mut shifts: Vec<(i32, i32)> = Vec::new();
        for a in -2i32..=2 {
            let range_b = if v2.is_some() { -2i32..=2 } else { 0..=0 };
            for b in range_b {
                let (ox, oy) = (a * v1.dx + b * v2.map_or(0, |v| v.dx), a * v1.dy + b * v2.map_or(0, |v| v.dy));
                shifts.push((ox, oy));
            }
        }
        // 每像素：各平移位置高通值的中值
        let mut cons = GrayF32::new(w as u32, h as u32);
        let mut vals = Vec::with_capacity(shifts.len());
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                vals.clear();
                for &(ox, oy) in &shifts {
                    let (sx, sy) = (x + ox, y + oy);
                    if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                        vals.push(hp[sy as usize * w + sx as usize]);
                    }
                }
                if vals.len() >= 4 {
                    cons.data[y as usize * w + x as usize] = ops::median(&mut vals);
                }
            }
        }
        // 极性：正/负一致响应的能量
        let pos: f32 = cons.data.iter().filter(|v| **v > 0.0).map(|v| v * v).sum();
        let neg: f32 = cons.data.iter().filter(|v| **v < 0.0).map(|v| v * v).sum();
        let bright = pos >= neg;
        let polarity_ratio = pos.max(neg) / (pos + neg).max(1e-6);
        let signed: Vec<f32> = cons.data.iter().map(|&v| if bright { v } else { -v }).collect();
        let positive: Vec<f32> = signed.iter().copied().filter(|v| *v > 0.5).collect();
        if positive.len() < 50 {
            return Ok(Vec::new());
        }
        let t = ops::otsu(&positive).max(5.0);
        let mut mask = GrayU8::new(w as u32, h as u32);
        for (i, &v) in signed.iter().enumerate() {
            if v > t {
                mask.data[i] = 255;
            }
        }
        let mask = ops::remove_small_components(&mask, 4);
        let coverage = mask.count_nonzero() as f32 / (w * h) as f32;
        if coverage < 0.004 {
            return Ok(Vec::new());
        }

        // 证据：白 / 灰 / 黑色半透明叠加会让像素相对本地背景 **更去饱和**（向无彩色靠拢）。
        // 直接看像素饱和度不可靠：半透明白字叠在彩色背景上，混合后的像素仍然偏饱和。
        let sat = ops::saturation(&img);
        let planes = ops::split_rgb(&img);
        let bg: Vec<GrayF32> = planes.iter().map(|p| ops::box_blur(p, 8)).collect();
        let (mut desat, mut n) = (0.0f32, 0.0f32);
        for (i, &m) in mask.data.iter().enumerate() {
            if m > 0 {
                let (r, g, b) = (bg[0].data[i], bg[1].data[i], bg[2].data[i]);
                let mx = r.max(g).max(b);
                let bg_sat = if mx <= 0.0 { 0.0 } else { (mx - r.min(g).min(b)) / mx };
                if sat.data[i] < bg_sat - 0.02 || bg_sat < 0.08 {
                    desat += 1.0;
                }
                n += 1.0;
            }
        }
        let desat_frac = if n > 0.0 { desat / n } else { 0.0 };
        let period = ((v1.dx.pow(2) + v1.dy.pow(2)) as f32).sqrt();
        let repeats = (w.max(h) as f32 / period).min(20.0);

        // 空间铺展：平铺水印覆盖画面大部分区域；招牌、织物纹样等局部重复只集中在一处
        let mut cells = [false; 16];
        for (i, &m) in mask.data.iter().enumerate() {
            if m > 0 {
                let (x, y) = (i % w, i / w);
                cells[(y * 4 / h) * 4 + x * 4 / w] = true;
            }
        }
        let spread = cells.iter().filter(|&&c| c).count() as f32 / 16.0;
        let e_spread = ((spread - 0.35) / 0.4).clamp(0.0, 1.0);

        let e_period = ((v1.rho - 0.18) / 0.4).clamp(0.0, 1.0);
        let e_lattice = if v2.is_some() { 1.0 } else { 0.4 };
        // 平铺水印通常较稀疏；覆盖过大更像规则纹理（砖墙、织物）
        let e_sparse = if coverage <= 0.25 { 1.0 } else { (1.0 - (coverage - 0.25) / 0.25).clamp(0.0, 1.0) };
        let e_sat = ((desat_frac - 0.4) / 0.4).clamp(0.0, 1.0);
        let e_polarity = ((polarity_ratio - 0.55) / 0.35).clamp(0.0, 1.0);
        let e_repeats = ((repeats - 2.0) / 3.0).clamp(0.0, 1.0);
        let combined =
            0.2 * e_period + 0.12 * e_lattice + 0.14 * e_sparse + 0.14 * e_sat + 0.14 * e_polarity + 0.08 * e_repeats + 0.18 * e_spread;
        let mut conf = (0.3 + 0.62 * combined).clamp(0.0, 0.92);
        // 没有铺展到全图或只有一维重复时，不足以自动判定
        if spread < 0.5 || v2.is_none() {
            conf = conf.min(0.72);
        }
        if conf < 0.45 {
            return Ok(Vec::new());
        }

        let bounds = mask.nonzero_bounds().unwrap();
        let bbox = bounds.to_bbox().to_normalized(w as u32, h as u32);
        let rotation = (v1.dy as f32).atan2(v1.dx as f32).to_degrees();
        let mut c = WatermarkCandidate::new(wm_common::new_id(), WatermarkType::Repeated, conf, bbox, DetectorSource::RepeatedPattern);
        c.repeat_score = v1.rho;
        c.rotation = rotation;
        c.evidence = vec![
            Evidence::new(DetectorSource::RepeatedPattern, "periodicity", e_period),
            Evidence::new(DetectorSource::RepeatedPattern, "lattice_2d", e_lattice),
            Evidence::new(DetectorSource::RepeatedPattern, "sparsity", e_sparse),
            Evidence::new(DetectorSource::RepeatedPattern, "achromatic_overlay", e_sat),
            Evidence::new(DetectorSource::RepeatedPattern, "polarity_consistency", e_polarity),
            Evidence::new(DetectorSource::RepeatedPattern, "repeat_count", e_repeats),
            Evidence::new(DetectorSource::RepeatedPattern, "spatial_spread", e_spread),
        ];
        let hint_data = mask.crop(&bounds);
        Ok(vec![Detection { candidate: c, hint: Some(MaskHint { bbox, data: hint_data }) }])
    }
}

/// 归一化线性自相关 ρ(dx, dy)，输出以 (0,0) 为中心的 (2w-1)×(2h-1) 网格，按行主序；索引 (dx+w-1, dy+h-1)。
fn autocorr(v: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let (pw, ph) = ((2 * w).next_power_of_two(), (2 * h).next_power_of_two());
    let mut buf = vec![Complex32::new(0.0, 0.0); pw * ph];
    for y in 0..h {
        for x in 0..w {
            buf[y * pw + x].re = v[y * w + x] - mean;
        }
    }
    let mut planner = FftPlanner::<f32>::new();
    let fr = planner.plan_fft_forward(pw);
    let fc = planner.plan_fft_forward(ph);
    let ir = planner.plan_fft_inverse(pw);
    let ic = planner.plan_fft_inverse(ph);
    fft2(&mut buf, pw, ph, &*fr, &*fc);
    for c in buf.iter_mut() {
        *c = Complex32::new(c.norm_sqr(), 0.0);
    }
    fft2(&mut buf, pw, ph, &*ir, &*ic);
    let zero = buf[0].re.max(1e-6);
    let (ow, oh) = (2 * w - 1, 2 * h - 1);
    let mut out = vec![0.0f32; ow * oh];
    let full = (w * h) as f32;
    for dy in -(h as i64 - 1)..=(h as i64 - 1) {
        for dx in -(w as i64 - 1)..=(w as i64 - 1) {
            let sx = ((dx + pw as i64) % pw as i64) as usize;
            let sy = ((dy + ph as i64) % ph as i64) as usize;
            let overlap = ((w as i64 - dx.abs()) * (h as i64 - dy.abs())) as f32;
            if overlap < full * 0.25 {
                continue;
            }
            // 按重叠面积做无偏归一化
            out[(dy + h as i64 - 1) as usize * ow + (dx + w as i64 - 1) as usize] = buf[sy * pw + sx].re / zero * full / overlap;
        }
    }
    out
}

fn fft2(buf: &mut [Complex32], pw: usize, ph: usize, row: &dyn rustfft::Fft<f32>, col: &dyn rustfft::Fft<f32>) {
    for r in buf.chunks_exact_mut(pw) {
        row.process(r);
    }
    let mut tmp = vec![Complex32::new(0.0, 0.0); ph];
    for x in 0..pw {
        for y in 0..ph {
            tmp[y] = buf[y * pw + x];
        }
        col.process(&mut tmp);
        for y in 0..ph {
            buf[y * pw + x] = tmp[y];
        }
    }
}

/// 在自相关中寻找局部峰值（排除中心附近），按 ρ 降序。只取上半平面（对称）。
fn find_peaks(ac: &[f32], w: usize, h: usize, min_period: i32) -> Vec<Peak> {
    let (ow, oh) = (2 * w - 1, 2 * h - 1);
    let at = |dx: i32, dy: i32| -> f32 {
        let x = dx + w as i32 - 1;
        let y = dy + h as i32 - 1;
        if x < 0 || y < 0 || x >= ow as i32 || y >= oh as i32 {
            0.0
        } else {
            ac[y as usize * ow + x as usize]
        }
    };
    let mut peaks = Vec::new();
    let (mx, my) = ((w / 2) as i32, (h / 2) as i32);
    for dy in 0..=my {
        for dx in -mx..=mx {
            if dy == 0 && dx <= 0 {
                continue;
            }
            if dx * dx + dy * dy < min_period * min_period {
                continue;
            }
            let v = at(dx, dy);
            if v < 0.1 {
                continue;
            }
            let mut is_max = true;
            'n: for ny in -3..=3 {
                for nx in -3..=3 {
                    if (nx != 0 || ny != 0) && at(dx + nx, dy + ny) > v {
                        is_max = false;
                        break 'n;
                    }
                }
            }
            if is_max {
                peaks.push(Peak { dx, dy, rho: v });
            }
        }
    }
    peaks.sort_by(|a, b| b.rho.partial_cmp(&a.rho).unwrap_or(std::cmp::Ordering::Equal));
    // 基频优先：若较短向量的峰值接近最高峰，用它作为 v1（避免选到谐波）
    if let Some(&best) = peaks.first() {
        let blen = best.dx * best.dx + best.dy * best.dy;
        if let Some(pos) = peaks.iter().position(|p| p.rho > best.rho * 0.8 && p.dx * p.dx + p.dy * p.dy < blen) {
            let p = peaks.remove(pos);
            peaks.insert(0, p);
        }
    }
    peaks.truncate(24);
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_core::CancellationToken;
    use wm_image::synth;

    fn run(img: &wm_core::ImageBuffer) -> Vec<Detection> {
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
        RepeatedPatternDetector::default().detect(&input).unwrap()
    }

    #[test]
    fn detects_tiled_diagonal_watermark() {
        let (w, h) = (900u32, 700u32);
        let mut img = synth::photo_like(w, h, 3);
        let mut t = synth::Overlay::new(w, h);
        let a = synth::rotate(&synth::render_text("PREVIEW", 3.0), -30.0);
        let (sx, sy) = (220i64, 160i64);
        let mut row = 0;
        let mut y = -40i64;
        while y < h as i64 {
            let mut x = -60i64 + (row % 2) * sx / 2;
            while x < w as i64 {
                synth::overlay(&mut img, &mut t, &a, x, y, [255, 255, 255], 0.35);
                x += sx;
            }
            y += sy;
            row += 1;
        }
        let d = run(&img);
        assert_eq!(d.len(), 1, "expected detection");
        let c = &d[0].candidate;
        assert_eq!(c.watermark_type, WatermarkType::Repeated);
        assert!(c.confidence > 0.7, "conf {}", c.confidence);
        // 提示 Mask 与真值的重合度
        let hint = d[0].hint.as_ref().unwrap();
        let full = ops::resize_mask_nearest(&hint.data, (hint.bbox.width * w as f32) as u32, (hint.bbox.height * h as f32) as u32);
        let (bx, by) = ((hint.bbox.x * w as f32) as u32, (hint.bbox.y * h as f32) as u32);
        let (mut tp, mut pred) = (0usize, 0usize);
        for y in 0..full.height {
            for x in 0..full.width {
                if full.get(x, y) > 0 {
                    pred += 1;
                    let (gx, gy) = (bx + x, by + y);
                    if gx < w && gy < h && t.mask.get(gx, gy) > 0 {
                        tp += 1;
                    }
                }
            }
        }
        assert!(tp as f32 / pred as f32 > 0.6, "precision {}", tp as f32 / pred as f32);
    }

    #[test]
    fn ignores_plain_photo() {
        let img = synth::photo_like(800, 600, 21);
        let d = run(&img);
        assert!(d.iter().all(|x| x.candidate.confidence < 0.7), "{:?}", d.iter().map(|x| x.candidate.confidence).collect::<Vec<_>>());
    }

    #[test]
    fn dense_regular_texture_is_not_high_confidence() {
        // 规则砖纹：强周期但覆盖密集、双极性
        let (w, h) = (640u32, 480u32);
        let mut img = wm_core::ImageBuffer::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let row = y / 24;
                let off = if row % 2 == 0 { 0 } else { 24 };
                let mortar = y % 24 < 3 || (x + off) % 48 < 3;
                let v = if mortar { 200 } else { 120 + ((x * 7 + y * 3) % 20) as u8 };
                img.put(x, y, [v, (v as f32 * 0.6) as u8, (v as f32 * 0.4) as u8, 255]);
            }
        }
        let d = run(&img);
        assert!(d.iter().all(|x| x.candidate.confidence < 0.85), "{:?}", d.iter().map(|x| x.candidate.confidence).collect::<Vec<_>>());
    }
}
