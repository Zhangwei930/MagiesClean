//! Mask 编辑的栅格化（MaskEditor 的矢量操作 → 像素）与 Mask 预览输出。

use crate::ops;
use wm_core::geometry::Point;
use wm_core::{GrayU8, MaskOp, MaskRegion, PixelRect, WatermarkMask};

/// 手动编辑区域的 candidate_id 标记。
pub const MANUAL_REGION: &str = "manual";

/// 把编辑操作应用到 Mask。
///
/// 实现：把所有受影响的范围合成为一块稠密“手动区域”，画笔/矩形/多边形写入 255，
/// 橡皮擦同时擦除所有区域中的对应像素。坐标为归一化坐标。
pub fn apply_mask_ops(mask: &mut WatermarkMask, ops_list: &[MaskOp]) {
    let (w, h) = (mask.width, mask.height);
    let long = w.max(h) as f32;
    for op in ops_list {
        match op {
            MaskOp::Clear => {
                mask.regions.clear();
            }
            MaskOp::Brush { points, radius, erase } => {
                let r = (radius * long).max(0.5);
                let pts: Vec<(f32, f32)> = points.iter().map(|p| (p.x * w as f32, p.y * h as f32)).collect();
                if pts.is_empty() {
                    continue;
                }
                let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for &(x, y) in &pts {
                    x0 = x0.min(x - r);
                    y0 = y0.min(y - r);
                    x1 = x1.max(x + r);
                    y1 = y1.max(y + r);
                }
                let rect = clip(x0, y0, x1, y1, w, h);
                if rect.is_empty() {
                    continue;
                }
                paint(mask, rect, *erase, |x, y| {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    if pts.len() == 1 {
                        return dist2(px, py, pts[0]) <= r * r;
                    }
                    pts.windows(2).any(|s| seg_dist2(px, py, s[0], s[1]) <= r * r)
                });
            }
            MaskOp::Rect { x, y, width, height, erase } => {
                let rect = clip(x * w as f32, y * h as f32, (x + width) * w as f32, (y + height) * h as f32, w, h);
                if !rect.is_empty() {
                    paint(mask, rect, *erase, |_, _| true);
                }
            }
            MaskOp::Polygon { points, erase } => {
                if points.len() < 3 {
                    continue;
                }
                let poly: Vec<(f32, f32)> = points.iter().map(|p: &Point| (p.x * w as f32, p.y * h as f32)).collect();
                let x0 = poly.iter().map(|p| p.0).fold(f32::MAX, f32::min);
                let y0 = poly.iter().map(|p| p.1).fold(f32::MAX, f32::min);
                let x1 = poly.iter().map(|p| p.0).fold(f32::MIN, f32::max);
                let y1 = poly.iter().map(|p| p.1).fold(f32::MIN, f32::max);
                let rect = clip(x0, y0, x1, y1, w, h);
                if !rect.is_empty() {
                    paint(mask, rect, *erase, |x, y| point_in_polygon(x as f32 + 0.5, y as f32 + 0.5, &poly));
                }
            }
        }
    }
    mask.regions.retain(|r| r.coverage() > 0);
    mask.version += 1;
}

fn clip(x0: f32, y0: f32, x1: f32, y1: f32, w: u32, h: u32) -> PixelRect {
    let a = x0.floor().clamp(0.0, w as f32) as u32;
    let b = y0.floor().clamp(0.0, h as f32) as u32;
    let c = x1.ceil().clamp(0.0, w as f32) as u32;
    let d = y1.ceil().clamp(0.0, h as f32) as u32;
    PixelRect::new(a, b, c.saturating_sub(a), d.saturating_sub(b))
}

fn paint(mask: &mut WatermarkMask, rect: PixelRect, erase: bool, inside: impl Fn(u32, u32) -> bool) {
    if erase {
        for r in &mut mask.regions {
            let ix0 = rect.x.max(r.rect.x);
            let iy0 = rect.y.max(r.rect.y);
            let ix1 = rect.right().min(r.rect.right());
            let iy1 = rect.bottom().min(r.rect.bottom());
            for y in iy0..iy1 {
                for x in ix0..ix1 {
                    if inside(x, y) {
                        r.data.set(x - r.rect.x, y - r.rect.y, 0);
                    }
                }
            }
        }
        return;
    }
    // 合并到手动区域：新区域 = 旧手动区域 ∪ rect
    let old = mask.regions.iter().position(|r| r.candidate_id.as_deref() == Some(MANUAL_REGION));
    let union = old.map(|i| mask.regions[i].rect.union(&rect)).unwrap_or(rect);
    let mut data = GrayU8::new(union.width, union.height);
    if let Some(i) = old {
        let o = &mask.regions[i];
        for y in 0..o.rect.height {
            for x in 0..o.rect.width {
                data.set(o.rect.x - union.x + x, o.rect.y - union.y + y, o.data.get(x, y));
            }
        }
    }
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            if inside(x, y) {
                data.set(x - union.x, y - union.y, 255);
            }
        }
    }
    let region = MaskRegion::new(union, data, Some(MANUAL_REGION.into()));
    match old {
        Some(i) => mask.regions[i] = region,
        None => mask.regions.push(region),
    }
}

fn dist2(px: f32, py: f32, p: (f32, f32)) -> f32 {
    (px - p.0).powi(2) + (py - p.1).powi(2)
}

fn seg_dist2(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    if l2 <= 1e-9 {
        return dist2(px, py, a);
    }
    let t = (((px - a.0) * dx + (py - a.1) * dy) / l2).clamp(0.0, 1.0);
    dist2(px, py, (a.0 + t * dx, a.1 + t * dy))
}

fn point_in_polygon(x: f32, y: f32, poly: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// 生成 Mask 叠加预览（RGBA，着色 + alpha），缩放到 `(pw, ph)`。
pub fn mask_overlay_rgba(mask: &WatermarkMask, pw: u32, ph: u32, color: [u8; 3]) -> wm_core::ImageBuffer {
    let mut out = wm_core::ImageBuffer::new(pw, ph);
    let sx = mask.width as f32 / pw as f32;
    let sy = mask.height as f32 / ph as f32;
    for r in &mask.regions {
        // 区域在预览坐标下的范围
        let px0 = (r.rect.x as f32 / sx).floor() as u32;
        let py0 = (r.rect.y as f32 / sy).floor() as u32;
        let px1 = ((r.rect.right() as f32 / sx).ceil() as u32).min(pw);
        let py1 = ((r.rect.bottom() as f32 / sy).ceil() as u32).min(ph);
        for py in py0..py1 {
            for px in px0..px1 {
                // 以预览像素覆盖的原图块取最大值，避免细笔画在缩小后消失
                let ox0 = ((px as f32 * sx) as u32).max(r.rect.x);
                let oy0 = ((py as f32 * sy) as u32).max(r.rect.y);
                let ox1 = (((px + 1) as f32 * sx).ceil() as u32).min(r.rect.right()).max(ox0 + 1);
                let oy1 = (((py + 1) as f32 * sy).ceil() as u32).min(r.rect.bottom()).max(oy0 + 1);
                let mut v = 0u8;
                for oy in oy0..oy1.min(r.rect.bottom()) {
                    for ox in ox0..ox1.min(r.rect.right()) {
                        v = v.max(r.data.get(ox - r.rect.x, oy - r.rect.y));
                    }
                }
                if v > 0 {
                    let i = out.idx(px, py);
                    let a = (v as f32 * 0.72) as u8;
                    if a > out.data[i + 3] {
                        out.data[i..i + 3].copy_from_slice(&color);
                        out.data[i + 3] = a;
                    }
                }
            }
        }
    }
    out
}

/// 手动编辑后清理：去除孤立噪点。
pub fn cleanup_manual(mask: &mut WatermarkMask) {
    for r in &mut mask.regions {
        if r.candidate_id.as_deref() == Some(MANUAL_REGION) {
            r.data = ops::remove_small_components(&r.data, 2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brush_then_erase() {
        let mut m = WatermarkMask::new("m".into(), "f".into(), 100, 100);
        apply_mask_ops(
            &mut m,
            &[MaskOp::Brush { points: vec![Point { x: 0.2, y: 0.5 }, Point { x: 0.8, y: 0.5 }], radius: 0.03, erase: false }],
        );
        let r = m.rasterize(&PixelRect::new(0, 0, 100, 100));
        assert_eq!(r.get(50, 50), 255);
        assert_eq!(r.get(50, 60), 0);
        apply_mask_ops(&mut m, &[MaskOp::Rect { x: 0.4, y: 0.4, width: 0.2, height: 0.2, erase: true }]);
        let r = m.rasterize(&PixelRect::new(0, 0, 100, 100));
        assert_eq!(r.get(50, 50), 0);
        assert_eq!(r.get(25, 50), 255);
        assert_eq!(m.version, 3);
    }

    #[test]
    fn polygon_fill() {
        let mut m = WatermarkMask::new("m".into(), "f".into(), 50, 50);
        let pts = vec![Point { x: 0.1, y: 0.1 }, Point { x: 0.9, y: 0.1 }, Point { x: 0.5, y: 0.9 }];
        apply_mask_ops(&mut m, &[MaskOp::Polygon { points: pts, erase: false }]);
        let r = m.rasterize(&PixelRect::new(0, 0, 50, 50));
        assert_eq!(r.get(25, 20), 255);
        assert_eq!(r.get(5, 45), 0);
    }
}
