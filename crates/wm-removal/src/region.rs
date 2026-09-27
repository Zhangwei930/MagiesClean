//! 区域化处理：把 Mask 聚成若干局部块，只裁剪 Mask 及必要 padding 进行修复（规格 §8.2），
//! 修复结果只按 Mask 合成回原图，保证 Mask 外像素不变。

use wm_core::{GrayU8, ImageBuffer, PixelRect, WatermarkMask};

/// 把 Mask 区域按 `pad` 扩展后合并重叠块。
pub fn clusters(mask: &WatermarkMask, pad: u32) -> Vec<PixelRect> {
    let mut rects: Vec<PixelRect> = mask
        .regions
        .iter()
        .filter_map(|r| {
            r.data
                .nonzero_bounds()
                .map(|b| PixelRect::new(r.rect.x + b.x, r.rect.y + b.y, b.width, b.height).pad(pad, mask.width, mask.height))
        })
        .collect();
    // 反复合并直到无重叠
    loop {
        let mut merged = false;
        'outer: for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                if overlaps(&rects[i], &rects[j]) {
                    let u = rects[i].union(&rects[j]);
                    rects[i] = u;
                    rects.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            break;
        }
    }
    rects
}

fn overlaps(a: &PixelRect, b: &PixelRect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

/// 按软 Mask 把 `patch`（修复结果）合成回 `image` 的 `rect` 位置：
/// `out = orig·(1-m) + patch·m`。Mask 为 0 的像素保持原样。
pub fn composite(image: &mut ImageBuffer, rect: &PixelRect, patch: &ImageBuffer, mask: &GrayU8) {
    for y in 0..rect.height {
        for x in 0..rect.width {
            let m = mask.get(x, y) as u32;
            if m == 0 {
                continue;
            }
            let i = image.idx(rect.x + x, rect.y + y);
            let j = patch.idx(x, y);
            for c in 0..3 {
                let o = image.data[i + c] as u32;
                let p = patch.data[j + c] as u32;
                image.data[i + c] = ((o * (255 - m) + p * m + 127) / 255) as u8;
            }
            // Alpha 通道保持原值（PNG 透明边缘不被修改）
        }
    }
}

/// 软 Mask → 修复用的硬 Mask（阈值以上视为待修复）。
pub fn hard(mask: &GrayU8, min: u8) -> Vec<bool> {
    mask.data.iter().map(|&v| v >= min).collect()
}

/// 修复所用的 padding：随 Mask 尺寸与方法变化。
pub fn padding_for(mask_rect: &PixelRect, min_pad: u32, factor: f32) -> u32 {
    let short = mask_rect.width.min(mask_rect.height) as f32;
    (short * factor).max(min_pad as f32) as u32
}
