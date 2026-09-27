//! Alpha Restore（规格 §7.1）。
//!
//! 透明叠加模型：`Observed = Background·(1−α) + Watermark·α`，
//! 反推：`Background = (Observed − Watermark·α) / (1−α)`。
//!
//! 只有 α 与水印颜色估计可信、背景信息尚存时才有效。α 接近 1、色值裁剪或重压缩
//! 会放大误差，因此逐像素做稳定性与颜色范围检查，不稳定像素交给 Inpaint 兜底。

use wm_core::traits::AlphaMatte;
use wm_core::{GrayU8, ImageBuffer, PixelRect};

/// α 超过该值时背景信息几乎丢失，不做反推。
pub const ALPHA_MAX: f32 = 0.88;
/// 反推值超出 [−TOL, 255+TOL] 视为不稳定。
const RANGE_TOL: f32 = 18.0;

pub struct AlphaOutcome {
    /// 需要进一步 Inpaint 的不稳定像素（相对 `rect`）。
    pub unstable: GrayU8,
    pub rect: PixelRect,
    pub restored: usize,
}

/// 原地反推 `matte.rect` 内的背景。返回不稳定像素 Mask。
pub fn restore(image: &mut ImageBuffer, matte: &AlphaMatte) -> AlphaOutcome {
    let r = wm_core::buffer::clamp_rect(&matte.rect, image.width, image.height);
    let mut unstable = GrayU8::new(r.width, r.height);
    let mut restored = 0;
    for y in 0..r.height {
        for x in 0..r.width {
            if x >= matte.alpha.width || y >= matte.alpha.height {
                continue;
            }
            let a = matte.alpha.get(x, y);
            if a <= 0.01 {
                continue;
            }
            let i = image.idx(r.x + x, r.y + y);
            if a >= ALPHA_MAX {
                unstable.set(x, y, 255);
                continue;
            }
            let mut out = [0.0f32; 3];
            let mut ok = true;
            for c in 0..3 {
                let o = image.data[i + c] as f32;
                // 观测值已饱和（被裁剪）时反推不可靠
                if (o >= 254.5 && matte.color[c] > o) || (o <= 0.5 && matte.color[c] < o) {
                    ok = false;
                }
                let b = (o - matte.color[c] * a) / (1.0 - a);
                if !(-RANGE_TOL..=255.0 + RANGE_TOL).contains(&b) {
                    ok = false;
                }
                out[c] = b;
            }
            if ok {
                for c in 0..3 {
                    image.data[i + c] = out[c].round().clamp(0.0, 255.0) as u8;
                }
                restored += 1;
            } else {
                unstable.set(x, y, 255);
            }
        }
    }
    AlphaOutcome { unstable, rect: r, restored }
}

/// 在已知背景（例如同一批次中的多张图）下估计 α：
/// 对每个像素求 `α = (O − B) / (C − B)` 的稳健中值。
pub fn estimate_alpha_from_pairs(observed: &[f32], background: &[f32], color: f32) -> Option<f32> {
    let mut v: Vec<f32> = observed
        .iter()
        .zip(background)
        .filter(|(_, &b)| (color - b).abs() > 24.0)
        .map(|(&o, &b)| ((o - b) / (color - b)).clamp(0.0, 1.0))
        .collect();
    if v.len() < 3 {
        return None;
    }
    let k = v.len() / 2;
    v.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap());
    Some(v[k])
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_core::GrayF32;

    #[test]
    fn exact_inverse_for_known_alpha() {
        let mut img = ImageBuffer::filled(10, 10, [100, 150, 50, 255]);
        let bg = img.clone();
        let a = 0.4f32;
        let c = [255.0, 255.0, 255.0];
        for y in 2..8 {
            for x in 2..8 {
                let i = img.idx(x, y);
                for ch in 0..3 {
                    let o = bg.data[i + ch] as f32 * (1.0 - a) + c[ch] * a;
                    img.data[i + ch] = o.round() as u8;
                }
            }
        }
        let mut alpha = GrayF32::new(10, 10);
        for y in 2..8 {
            for x in 2..8 {
                alpha.set(x, y, a);
            }
        }
        let matte = AlphaMatte { rect: PixelRect::new(0, 0, 10, 10), alpha, color: c, quality: 1.0 };
        let out = restore(&mut img, &matte);
        assert_eq!(out.unstable.count_nonzero(), 0);
        for (p, q) in img.data.iter().zip(&bg.data) {
            assert!((*p as i32 - *q as i32).abs() <= 2);
        }
    }

    #[test]
    fn opaque_pixels_are_flagged_unstable() {
        let mut img = ImageBuffer::filled(4, 4, [255, 255, 255, 255]);
        let mut alpha = GrayF32::new(4, 4);
        alpha.data.fill(0.95);
        let matte = AlphaMatte { rect: PixelRect::new(0, 0, 4, 4), alpha, color: [255.0; 3], quality: 1.0 };
        let out = restore(&mut img, &matte);
        assert_eq!(out.unstable.count_nonzero(), 16);
    }

    #[test]
    fn alpha_estimate_is_robust_median() {
        let bg = [10.0, 50.0, 100.0, 200.0, 30.0];
        let a = 0.3;
        let obs: Vec<f32> = bg.iter().map(|b| b * (1.0 - a) + 255.0 * a).collect();
        let est = estimate_alpha_from_pairs(&obs, &bg, 255.0).unwrap();
        assert!((est - 0.3).abs() < 0.01);
    }
}
