//! 合成数据工具（规格 §10.3 Synthetic Dataset Generator 的绘制部分）。
//!
//! 在无水印图上叠加文字 / 图形水印：随机 Opacity、Rotation、Scale、Blur、重复平铺，
//! 输出 watermarked image、ground truth mask 与 alpha。内置 5×7 点阵字体，
//! 使测试与数据生成不依赖系统字体。

use crate::ops;
use wm_core::{GrayF32, GrayU8, ImageBuffer};

/// SplitMix64：确定性随机数。
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f32()
    }
    pub fn int(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            lo
        } else {
            lo + (self.next_u64() % (hi - lo + 1) as u64) as i64
        }
    }
}

fn glyph(c: char) -> [u8; 7] {
    let rows: [&str; 7] = match c.to_ascii_uppercase() {
        'A' => ["01110", "10001", "10001", "11111", "10001", "10001", "10001"],
        'B' => ["11110", "10001", "10001", "11110", "10001", "10001", "11110"],
        'C' => ["01110", "10001", "10000", "10000", "10000", "10001", "01110"],
        'D' => ["11100", "10010", "10001", "10001", "10001", "10010", "11100"],
        'E' => ["11111", "10000", "10000", "11110", "10000", "10000", "11111"],
        'F' => ["11111", "10000", "10000", "11110", "10000", "10000", "10000"],
        'G' => ["01110", "10001", "10000", "10111", "10001", "10001", "01111"],
        'H' => ["10001", "10001", "10001", "11111", "10001", "10001", "10001"],
        'I' => ["01110", "00100", "00100", "00100", "00100", "00100", "01110"],
        'J' => ["00111", "00010", "00010", "00010", "00010", "10010", "01100"],
        'K' => ["10001", "10010", "10100", "11000", "10100", "10010", "10001"],
        'L' => ["10000", "10000", "10000", "10000", "10000", "10000", "11111"],
        'M' => ["10001", "11011", "10101", "10101", "10001", "10001", "10001"],
        'N' => ["10001", "10001", "11001", "10101", "10011", "10001", "10001"],
        'O' => ["01110", "10001", "10001", "10001", "10001", "10001", "01110"],
        'P' => ["11110", "10001", "10001", "11110", "10000", "10000", "10000"],
        'Q' => ["01110", "10001", "10001", "10001", "10101", "10010", "01101"],
        'R' => ["11110", "10001", "10001", "11110", "10100", "10010", "10001"],
        'S' => ["01111", "10000", "10000", "01110", "00001", "00001", "11110"],
        'T' => ["11111", "00100", "00100", "00100", "00100", "00100", "00100"],
        'U' => ["10001", "10001", "10001", "10001", "10001", "10001", "01110"],
        'V' => ["10001", "10001", "10001", "10001", "10001", "01010", "00100"],
        'W' => ["10001", "10001", "10001", "10101", "10101", "10101", "01010"],
        'X' => ["10001", "10001", "01010", "00100", "01010", "10001", "10001"],
        'Y' => ["10001", "10001", "01010", "00100", "00100", "00100", "00100"],
        'Z' => ["11111", "00001", "00010", "00100", "01000", "10000", "11111"],
        '0' => ["01110", "10001", "10011", "10101", "11001", "10001", "01110"],
        '1' => ["00100", "01100", "00100", "00100", "00100", "00100", "01110"],
        '2' => ["01110", "10001", "00001", "00010", "00100", "01000", "11111"],
        '3' => ["11111", "00010", "00100", "00010", "00001", "10001", "01110"],
        '4' => ["00010", "00110", "01010", "10010", "11111", "00010", "00010"],
        '5' => ["11111", "10000", "11110", "00001", "00001", "10001", "01110"],
        '6' => ["00110", "01000", "10000", "11110", "10001", "10001", "01110"],
        '7' => ["11111", "00001", "00010", "00100", "01000", "01000", "01000"],
        '8' => ["01110", "10001", "10001", "01110", "10001", "10001", "01110"],
        '9' => ["01110", "10001", "10001", "01111", "00001", "00010", "01100"],
        '.' => ["00000", "00000", "00000", "00000", "00000", "01100", "01100"],
        '@' => ["01110", "10001", "10111", "10101", "10111", "10000", "01111"],
        '/' => ["00001", "00010", "00010", "00100", "01000", "01000", "10000"],
        '-' => ["00000", "00000", "00000", "11111", "00000", "00000", "00000"],
        '_' => ["00000", "00000", "00000", "00000", "00000", "00000", "11111"],
        ':' => ["00000", "01100", "01100", "00000", "01100", "01100", "00000"],
        _ => ["00000", "00000", "00000", "00000", "00000", "00000", "00000"],
    };
    let mut out = [0u8; 7];
    for (i, r) in rows.iter().enumerate() {
        out[i] = u8::from_str_radix(r, 2).unwrap_or(0);
    }
    out
}

/// 渲染文字为 alpha 图（0..1）。`px` 为点阵每格的像素大小，带 4× 超采样抗锯齿。
pub fn render_text(text: &str, px: f32) -> GrayF32 {
    const SS: usize = 4;
    let cols = text.chars().count().max(1) * 6 - 1;
    let cell = (px * SS as f32).max(1.0);
    let w = ((cols as f32 * cell).ceil() as usize).max(1);
    let h = ((7.0 * cell).ceil() as usize).max(1);
    let mut hi = vec![0.0f32; w * h];
    for (ci, ch) in text.chars().enumerate() {
        let g = glyph(ch);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                let x0 = ((ci * 6 + col) as f32 * cell) as usize;
                let y0 = (row as f32 * cell) as usize;
                let x1 = (((ci * 6 + col + 1) as f32 * cell) as usize).min(w);
                let y1 = (((row + 1) as f32 * cell) as usize).min(h);
                for y in y0..y1 {
                    for x in x0..x1 {
                        hi[y * w + x] = 1.0;
                    }
                }
            }
        }
    }
    let g = GrayF32 { width: w as u32, height: h as u32, data: hi };
    let (ow, oh) = ((w / SS).max(1) as u32, (h / SS).max(1) as u32);
    ops::resize_gray(&g, ow, oh)
}

/// 以中心为原点旋转 alpha 图（度，顺时针为正），输出扩展后的画布。
pub fn rotate(a: &GrayF32, degrees: f32) -> GrayF32 {
    if degrees.abs() < 0.01 {
        return a.clone();
    }
    let t = degrees.to_radians();
    let (s, c) = (t.sin(), t.cos());
    let (w, h) = (a.width as f32, a.height as f32);
    let nw = (w * c.abs() + h * s.abs()).ceil() as u32;
    let nh = (w * s.abs() + h * c.abs()).ceil() as u32;
    let mut out = GrayF32::new(nw, nh);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (ncx, ncy) = (nw as f32 / 2.0, nh as f32 / 2.0);
    for y in 0..nh {
        for x in 0..nw {
            let dx = x as f32 + 0.5 - ncx;
            let dy = y as f32 + 0.5 - ncy;
            // 逆旋转采样
            let sx = c * dx + s * dy + cx - 0.5;
            let sy = -s * dx + c * dy + cy - 0.5;
            if sx < -1.0 || sy < -1.0 || sx > w || sy > h {
                continue;
            }
            let (x0, y0) = (sx.floor() as i64, sy.floor() as i64);
            let (tx, ty) = (sx - x0 as f32, sy - y0 as f32);
            let p = |xx: i64, yy: i64| {
                if xx < 0 || yy < 0 || xx >= a.width as i64 || yy >= a.height as i64 {
                    0.0
                } else {
                    a.get(xx as u32, yy as u32)
                }
            };
            let v = (p(x0, y0) * (1.0 - tx) + p(x0 + 1, y0) * tx) * (1.0 - ty) + (p(x0, y0 + 1) * (1.0 - tx) + p(x0 + 1, y0 + 1) * tx) * ty;
            out.set(x, y, v);
        }
    }
    out
}

/// 叠加结果：真值 alpha（全图尺寸）与二值 Mask。
pub struct Overlay {
    pub alpha: GrayF32,
    pub mask: GrayU8,
}

impl Overlay {
    pub fn new(w: u32, h: u32) -> Self {
        Self { alpha: GrayF32::new(w, h), mask: GrayU8::new(w, h) }
    }
}

/// 把 alpha 图 `a` 以不透明度 `opacity`、颜色 `color` 叠加到 `(x, y)`。
pub fn overlay(img: &mut ImageBuffer, truth: &mut Overlay, a: &GrayF32, x: i64, y: i64, color: [u8; 3], opacity: f32) {
    for yy in 0..a.height as i64 {
        for xx in 0..a.width as i64 {
            let (px, py) = (x + xx, y + yy);
            if px < 0 || py < 0 || px >= img.width as i64 || py >= img.height as i64 {
                continue;
            }
            let al = (a.get(xx as u32, yy as u32) * opacity).clamp(0.0, 1.0);
            if al <= 0.0 {
                continue;
            }
            let i = img.idx(px as u32, py as u32);
            for c in 0..3 {
                let b = img.data[i + c] as f32;
                img.data[i + c] = (b * (1.0 - al) + color[c] as f32 * al).round().clamp(0.0, 255.0) as u8;
            }
            let ti = truth.alpha.idx(px as u32, py as u32);
            // 多次叠加时合成 alpha
            let prev = truth.alpha.data[ti];
            truth.alpha.data[ti] = prev + al * (1.0 - prev);
            if truth.alpha.data[ti] > 0.02 {
                truth.mask.data[ti] = 255;
            }
        }
    }
}

/// 类照片背景：平滑渐变 + 多尺度值噪声 + 随机色块 + 细噪声。
pub fn photo_like(w: u32, h: u32, seed: u64) -> ImageBuffer {
    let mut rng = Rng::new(seed);
    let base: [f32; 3] = [rng.range(30.0, 220.0), rng.range(30.0, 220.0), rng.range(30.0, 220.0)];
    let grad: [f32; 3] = [rng.range(-80.0, 80.0), rng.range(-80.0, 80.0), rng.range(-80.0, 80.0)];
    let angle = rng.range(0.0, std::f32::consts::TAU);
    let (ga, gb) = (angle.cos(), angle.sin());
    // 值噪声格点
    let octaves: Vec<(u32, Vec<f32>, f32)> = [8u32, 24, 64]
        .iter()
        .map(|&cells| {
            let n = ((cells + 2) * (cells + 2)) as usize;
            let v = (0..n * 3).map(|_| rng.range(-1.0, 1.0)).collect();
            (cells, v, 60.0 / (cells as f32).sqrt())
        })
        .collect();
    let mut img = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let g = (u - 0.5) * ga + (v - 0.5) * gb;
            let mut px = [0.0f32; 3];
            for c in 0..3 {
                px[c] = base[c] + grad[c] * g;
            }
            for (cells, vals, amp) in &octaves {
                let fx = u * *cells as f32;
                let fy = v * *cells as f32;
                let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
                let stride = (*cells + 2) as usize;
                for c in 0..3 {
                    let at = |i: usize, j: usize| vals[((j * stride + i) * 3) + c];
                    let a = at(x0, y0) * (1.0 - sx) + at(x0 + 1, y0) * sx;
                    let b = at(x0, y0 + 1) * (1.0 - sx) + at(x0 + 1, y0 + 1) * sx;
                    px[c] += (a * (1.0 - sy) + b * sy) * amp;
                }
            }
            img.put(x, y, [px[0].clamp(0.0, 255.0) as u8, px[1].clamp(0.0, 255.0) as u8, px[2].clamp(0.0, 255.0) as u8, 255]);
        }
    }
    // 随机色块（模拟物体）
    for _ in 0..rng.int(3, 7) {
        let cx = rng.range(0.0, w as f32);
        let cy = rng.range(0.0, h as f32);
        let rx = rng.range(0.05, 0.25) * w as f32;
        let ry = rng.range(0.05, 0.25) * h as f32;
        let col = [rng.range(0.0, 255.0), rng.range(0.0, 255.0), rng.range(0.0, 255.0)];
        let x0 = (cx - rx).max(0.0) as u32;
        let x1 = ((cx + rx) as u32).min(w);
        let y0 = (cy - ry).max(0.0) as u32;
        let y1 = ((cy + ry) as u32).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((x as f32 - cx) / rx).powi(2) + ((y as f32 - cy) / ry).powi(2);
                if d <= 1.0 {
                    let k = (1.0 - d).sqrt().min(1.0) * 0.85;
                    let i = img.idx(x, y);
                    for c in 0..3 {
                        img.data[i + c] = (img.data[i + c] as f32 * (1.0 - k) + col[c] * k) as u8;
                    }
                }
            }
        }
    }
    // 细噪声（传感器噪声）
    for v in img.data.chunks_exact_mut(4) {
        let n = rng.range(-4.0, 4.0);
        for c in v.iter_mut().take(3) {
            *c = (*c as f32 + n).clamp(0.0, 255.0) as u8;
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_renders_nonempty() {
        let a = render_text("SHOP.COM", 3.0);
        assert!(a.width > 100 && a.height >= 20);
        assert!(a.data.iter().filter(|&&v| v > 0.5).count() > 100);
        let r = rotate(&a, 45.0);
        assert!(r.width > a.width / 2 && r.height > a.height);
    }

    #[test]
    fn overlay_records_ground_truth() {
        let mut img = photo_like(200, 120, 7);
        let mut t = Overlay::new(200, 120);
        let a = render_text("AB", 4.0);
        overlay(&mut img, &mut t, &a, 10, 10, [255, 255, 255], 0.5);
        assert!(t.mask.count_nonzero() > 50);
        assert!(t.alpha.max() <= 0.5 + 1e-4);
    }
}
