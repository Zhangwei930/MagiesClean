//! RemovalQualityEvaluator（规格 §7.2）。
//!
//! 输入：原图区域 + Mask + 结果区域。检查：水印残留、异常模糊、颜色不连续、边缘伪影、
//! 候选外区域意外变化。输出 quality_score 与原因；低于阈值进入 Needs Review。
//! 质量分只是复核辅助信号，不能证明被遮挡的真实细节已经恢复。

use wm_core::msg;
use wm_core::quality::{QualityIssue, QualityIssueKind, QualityReport};
use wm_core::{GrayU8, ImageBuffer};
use wm_image::ops;

pub fn evaluate(original: &ImageBuffer, result: &ImageBuffer, mask: &GrayU8, threshold: f32) -> QualityReport {
    debug_assert_eq!((original.width, original.height), (result.width, result.height));
    let mut issues = Vec::new();
    let n = mask.data.len();
    let inside: Vec<bool> = mask.data.iter().map(|&v| v >= 128).collect();
    let n_in = inside.iter().filter(|&&b| b).count();
    if n_in == 0 {
        return QualityReport::perfect();
    }

    // 1) Mask 外改动
    let mut changed_out = 0usize;
    let mut n_out = 0usize;
    for i in 0..n {
        if mask.data[i] == 0 {
            n_out += 1;
            let o = &original.data[i * 4..i * 4 + 4];
            let r = &result.data[i * 4..i * 4 + 4];
            if o.iter().zip(r).any(|(a, b)| a.abs_diff(*b) > 1) {
                changed_out += 1;
            }
        }
    }
    if changed_out > 0 {
        let frac = changed_out as f32 / n_out.max(1) as f32;
        issues.push(QualityIssue {
            kind: QualityIssueKind::OutsideChange,
            severity: (frac * 50.0).clamp(0.05, 1.0),
            message: msg!(
                format!("水印区域外有 {changed_out} 个像素被改动"),
                format!("{changed_out} pixels outside the watermark were changed")
            ),
        });
    }

    // 梯度
    let go = ops::magnitude_of(&original.to_luma_f32());
    let gr = ops::magnitude_of(&result.to_luma_f32());
    let dist = ops::distance_to_nonzero(&ops::binarize(mask, 128));
    let ring_w = ((mask.width.min(mask.height) as f32) * 0.15).clamp(3.0, 16.0);
    let (mut e_in_o, mut e_in_r, mut e_ring, mut n_ring) = (0.0f64, 0.0f64, 0.0f64, 0usize);
    let mut a_o = Vec::with_capacity(n_in);
    let mut a_r = Vec::with_capacity(n_in);
    let (mut sum_in, mut sum_ring, mut sq_ring) = ([0.0f64; 3], [0.0f64; 3], [0.0f64; 3]);
    let (mut e_edge, mut n_edge) = (0.0f64, 0usize);
    let unchanged_in =
        inside.iter().enumerate().filter(|(i, &b)| b && original.data[i * 4..i * 4 + 3] == result.data[i * 4..i * 4 + 3]).count();
    for i in 0..n {
        if inside[i] {
            e_in_o += go.data[i] as f64;
            e_in_r += gr.data[i] as f64;
            a_o.push(go.data[i]);
            a_r.push(gr.data[i]);
            for c in 0..3 {
                sum_in[c] += result.data[i * 4 + c] as f64;
            }
        } else if dist.data[i] > 1.5 && dist.data[i] <= ring_w + 1.5 {
            e_ring += gr.data[i] as f64;
            n_ring += 1;
            for c in 0..3 {
                let v = result.data[i * 4 + c] as f64;
                sum_ring[c] += v;
                sq_ring[c] += v * v;
            }
        }
        // Mask 边界（软 Mask 的外沿 1–2 像素）
        if mask.data[i] > 0 && mask.data[i] < 255 || (dist.data[i] > 0.0 && dist.data[i] <= 1.5) {
            e_edge += gr.data[i] as f64;
            n_edge += 1;
        }
    }
    let e_in_o = e_in_o / n_in as f64;
    let e_in_r = e_in_r / n_in as f64;
    let e_ring = if n_ring > 0 { e_ring / n_ring as f64 } else { e_in_r };

    // 2) 残留：结果中仍保留原水印的结构
    if unchanged_in as f32 / n_in as f32 > 0.5 {
        issues.push(QualityIssue {
            kind: QualityIssueKind::Residual,
            severity: 1.0,
            message: msg!("水印区域大部分未被修改", "Most of the watermark area was not changed"),
        });
    } else if e_in_o > e_ring + 1.0 {
        let corr = ops::ncc(&a_o, &a_r).max(0.0) as f64;
        let excess_o = (e_in_o - e_ring).max(1e-3);
        let excess_r = (e_in_r - e_ring).max(0.0);
        let energy = (excess_r / excess_o).clamp(0.0, 1.0);
        // 人眼对“弱但结构化”的轮廓残留很敏感：只要残留梯度与原水印高度相关，
        // 即使强度只有原来的 15–30% 也应判为残留。
        let sev = (((corr - 0.3) / 0.35).clamp(0.0, 1.0) * ((energy - 0.08) / 0.25).clamp(0.0, 1.0)) as f32;
        if sev > 0.05 {
            issues.push(QualityIssue {
                kind: QualityIssueKind::Residual,
                severity: sev,
                message: msg!(
                    format!("修复区域仍保留部分原水印轮廓（相关度 {:.0}%）", corr * 100.0),
                    format!("The repaired area still shows the watermark outline ({:.0}% correlation)", corr * 100.0)
                ),
            });
        }
    }

    // 3) 模糊：周围有纹理但修复区域过于平滑
    if n_ring > 0 && e_ring > 4.0 && e_in_r < 0.3 * e_ring {
        let sev = (1.0 - e_in_r / (0.3 * e_ring)).clamp(0.0, 1.0) as f32;
        issues.push(QualityIssue {
            kind: QualityIssueKind::Blur,
            severity: sev * 0.8,
            message: msg!("修复区域比周围明显更平滑", "The repaired area is noticeably smoother than its surroundings"),
        });
    }

    // 4) 颜色不连续
    if n_ring > 8 {
        let mut worst = 0.0f64;
        for c in 0..3 {
            let mi = sum_in[c] / n_in as f64;
            let mr = sum_ring[c] / n_ring as f64;
            let sd = (sq_ring[c] / n_ring as f64 - mr * mr).max(0.0).sqrt();
            worst = worst.max(((mi - mr).abs() - 2.0 * sd - 6.0) / 40.0);
        }
        if worst > 0.0 {
            issues.push(QualityIssue {
                kind: QualityIssueKind::ColorShift,
                severity: worst.clamp(0.0, 1.0) as f32,
                message: msg!("修复区域与周围颜色存在差异", "The repaired area differs in color from its surroundings"),
            });
        }
    }

    // 5) 边缘伪影
    if n_edge > 0 {
        let e_edge = e_edge / n_edge as f64;
        let excess = e_edge - 2.2 * e_ring.max(2.0);
        if excess > 0.0 {
            issues.push(QualityIssue {
                kind: QualityIssueKind::EdgeArtifact,
                severity: (excess / 25.0).clamp(0.0, 1.0) as f32,
                message: msg!("修复边缘出现明显接缝", "A visible seam appears along the repair edge"),
            });
        }
    }

    QualityReport::from_issues(issues, threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn textured(w: u32, h: u32) -> ImageBuffer {
        let mut b = ImageBuffer::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = (((x * 7 + y * 13) % 31) * 4 + 60) as u8;
                b.put(x, y, [v, v, v, 255]);
            }
        }
        b
    }

    fn with_text(b: &ImageBuffer, mask: &GrayU8) -> ImageBuffer {
        let mut o = b.clone();
        for y in 0..b.height {
            for x in 0..b.width {
                if mask.get(x, y) > 0 && (x / 2) % 2 == 0 {
                    o.put(x, y, [255, 255, 255, 255]);
                }
            }
        }
        o
    }

    fn block_mask(w: u32, h: u32) -> GrayU8 {
        let mut m = GrayU8::new(w, h);
        for y in 20..30 {
            for x in 15..45 {
                m.set(x, y, 255);
            }
        }
        m
    }

    #[test]
    fn perfect_restoration_passes() {
        let clean = textured(60, 50);
        let m = block_mask(60, 50);
        let wm = with_text(&clean, &m);
        let r = evaluate(&wm, &clean, &m, 0.6);
        assert!(r.passed, "{r:?}");
    }

    #[test]
    fn untouched_result_fails_with_residual() {
        let clean = textured(60, 50);
        let m = block_mask(60, 50);
        let wm = with_text(&clean, &m);
        let r = evaluate(&wm, &wm, &m, 0.6);
        assert!(!r.passed);
        assert!(r.issues.iter().any(|i| i.kind == QualityIssueKind::Residual));
    }

    #[test]
    fn faint_structured_residue_is_flagged() {
        let clean = textured(60, 50);
        let m = block_mask(60, 50);
        let wm = with_text(&clean, &m);
        // 结果只保留 30% 的原水印对比度（典型的轮廓残留）
        let mut res = clean.clone();
        for (i, px) in res.data.iter_mut().enumerate() {
            if i % 4 != 3 {
                *px = (clean.data[i] as f32 + 0.3 * (wm.data[i] as f32 - clean.data[i] as f32)).round() as u8;
            }
        }
        let r = evaluate(&wm, &res, &m, 0.6);
        assert!(r.issues.iter().any(|i| i.kind == QualityIssueKind::Residual), "{r:?}");
    }

    #[test]
    fn outside_change_blocks() {
        let clean = textured(60, 50);
        let m = block_mask(60, 50);
        let wm = with_text(&clean, &m);
        let mut res = clean.clone();
        res.put(0, 0, [0, 0, 0, 255]);
        let r = evaluate(&wm, &res, &m, 0.6);
        assert!(!r.passed);
        assert!(r.issues.iter().any(|i| i.kind == QualityIssueKind::OutsideChange));
    }

    #[test]
    fn flat_fill_on_texture_flags_blur() {
        let clean = textured(60, 50);
        let m = block_mask(60, 50);
        let wm = with_text(&clean, &m);
        let mut flat = clean.clone();
        for y in 20..30 {
            for x in 15..45 {
                flat.put(x, y, [120, 120, 120, 255]);
            }
        }
        let r = evaluate(&wm, &flat, &m, 0.6);
        assert!(r.issues.iter().any(|i| i.kind == QualityIssueKind::Blur), "{r:?}");
    }
}
