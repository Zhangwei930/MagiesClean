//! 图像处理基础运算：滤波、梯度、形态学、连通域、阈值、缩放。
//!
//! 这些运算以纯 Rust 实现并统一在本模块封装（规格 §3.2 中 OpenCV Adapter 的职责），
//! 上层算法只依赖这里的函数签名，后续可替换为 OpenCV 实现。

use image::imageops::FilterType;
use wm_core::{GrayF32, GrayU8, ImageBuffer, PixelRect};

// ───────────────────────────── 缩放 ─────────────────────────────

/// 缩放到长边不超过 `max_side`；返回（缩略图, 缩放比例 = 缩略图 / 原图）。
pub fn thumbnail(img: &ImageBuffer, max_side: u32) -> (ImageBuffer, f32) {
    let long = img.width.max(img.height);
    if long <= max_side {
        return (img.clone(), 1.0);
    }
    let scale = max_side as f32 / long as f32;
    let w = ((img.width as f32 * scale).round() as u32).max(1);
    let h = ((img.height as f32 * scale).round() as u32).max(1);
    (resize(img, w, h), scale)
}

pub fn resize(img: &ImageBuffer, w: u32, h: u32) -> ImageBuffer {
    if img.width == w && img.height == h {
        return img.clone();
    }
    let src = image::RgbaImage::from_raw(img.width, img.height, img.data.clone()).expect("valid buffer");
    let out = if w < img.width && h < img.height {
        // 缩小：面积平均，避免锯齿
        image::imageops::thumbnail(&src, w, h)
    } else {
        image::imageops::resize(&src, w, h, FilterType::Triangle)
    };
    ImageBuffer { width: w, height: h, data: out.into_raw() }
}

/// 双线性缩放单通道图。
pub fn resize_gray(g: &GrayF32, w: u32, h: u32) -> GrayF32 {
    if g.width == w && g.height == h {
        return g.clone();
    }
    let mut out = GrayF32::new(w, h);
    let sx = g.width as f32 / w as f32;
    let sy = g.height as f32 / h as f32;
    if sx > 1.5 || sy > 1.5 {
        // 大幅缩小：面积平均
        for y in 0..h {
            let y0 = (y as f32 * sy) as u32;
            let y1 = (((y + 1) as f32 * sy).ceil() as u32).clamp(y0 + 1, g.height);
            for x in 0..w {
                let x0 = (x as f32 * sx) as u32;
                let x1 = (((x + 1) as f32 * sx).ceil() as u32).clamp(x0 + 1, g.width);
                let mut s = 0.0;
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        s += g.get(xx, yy);
                    }
                }
                out.set(x, y, s / ((y1 - y0) * (x1 - x0)) as f32);
            }
        }
        return out;
    }
    for y in 0..h {
        let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
        let y0 = (fy.floor() as u32).min(g.height - 1);
        let y1 = (y0 + 1).min(g.height - 1);
        let ty = fy - y0 as f32;
        for x in 0..w {
            let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
            let x0 = (fx.floor() as u32).min(g.width - 1);
            let x1 = (x0 + 1).min(g.width - 1);
            let tx = fx - x0 as f32;
            let a = g.get(x0, y0) * (1.0 - tx) + g.get(x1, y0) * tx;
            let b = g.get(x0, y1) * (1.0 - tx) + g.get(x1, y1) * tx;
            out.set(x, y, a * (1.0 - ty) + b * ty);
        }
    }
    out
}

/// 最近邻缩放 u8 Mask。
pub fn resize_mask_nearest(m: &GrayU8, w: u32, h: u32) -> GrayU8 {
    let mut out = GrayU8::new(w, h);
    for y in 0..h {
        let sy = ((y as f32 + 0.5) * m.height as f32 / h as f32) as u32;
        for x in 0..w {
            let sx = ((x as f32 + 0.5) * m.width as f32 / w as f32) as u32;
            out.set(x, y, m.get(sx.min(m.width - 1), sy.min(m.height - 1)));
        }
    }
    out
}

/// 双线性缩放 u8 软 Mask。
pub fn resize_mask_bilinear(m: &GrayU8, w: u32, h: u32) -> GrayU8 {
    let g = GrayF32 { width: m.width, height: m.height, data: m.data.iter().map(|&v| v as f32).collect() };
    let r = resize_gray(&g, w, h);
    GrayU8 { width: w, height: h, data: r.data.iter().map(|&v| v.round().clamp(0.0, 255.0) as u8).collect() }
}

// ───────────────────────────── 滤波 ─────────────────────────────

/// 积分图（(w+1)×(h+1)，f64 避免大图累积误差）。
pub struct Integral {
    w: usize,
    data: Vec<f64>,
}

impl Integral {
    pub fn new(g: &GrayF32) -> Self {
        let (w, h) = (g.width as usize, g.height as usize);
        let mut data = vec![0.0f64; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0.0;
            for x in 0..w {
                row += g.data[y * w + x] as f64;
                data[(y + 1) * (w + 1) + x + 1] = data[y * (w + 1) + x + 1] + row;
            }
        }
        Self { w, data }
    }
    /// 矩形 [x0,x1) × [y0,y1) 内的和。
    #[inline]
    pub fn sum(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> f64 {
        let s = self.w + 1;
        self.data[y1 * s + x1] - self.data[y0 * s + x1] - self.data[y1 * s + x0] + self.data[y0 * s + x0]
    }
}

/// 方框均值滤波，半径 `r`（窗口 2r+1），边界按有效像素求平均。
pub fn box_blur(g: &GrayF32, r: u32) -> GrayF32 {
    if r == 0 {
        return g.clone();
    }
    let ii = Integral::new(g);
    let (w, h) = (g.width as usize, g.height as usize);
    let r = r as usize;
    let mut out = GrayF32::new(g.width, g.height);
    for y in 0..h {
        let y0 = y.saturating_sub(r);
        let y1 = (y + r + 1).min(h);
        for x in 0..w {
            let x0 = x.saturating_sub(r);
            let x1 = (x + r + 1).min(w);
            let n = ((y1 - y0) * (x1 - x0)) as f64;
            out.data[y * w + x] = (ii.sum(x0, y0, x1, y1) / n) as f32;
        }
    }
    out
}

/// 可分离高斯模糊。
pub fn gaussian_blur(g: &GrayF32, sigma: f32) -> GrayF32 {
    if sigma <= 0.05 {
        return g.clone();
    }
    let radius = (sigma * 3.0).ceil() as i64;
    let kernel: Vec<f32> = (-radius..=radius).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let ksum: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / ksum).collect();
    let (w, h) = (g.width as i64, g.height as i64);
    let mut tmp = GrayF32::new(g.width, g.height);
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (k, kv) in kernel.iter().enumerate() {
                s += kv * g.get_clamped(x + k as i64 - radius, y);
            }
            tmp.data[(y * w + x) as usize] = s;
        }
    }
    let mut out = GrayF32::new(g.width, g.height);
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (k, kv) in kernel.iter().enumerate() {
                s += kv * tmp.get_clamped(x, y + k as i64 - radius);
            }
            out.data[(y * w + x) as usize] = s;
        }
    }
    out
}

/// Sobel 梯度（gx, gy）。
pub fn sobel(g: &GrayF32) -> (GrayF32, GrayF32) {
    let (w, h) = (g.width as i64, g.height as i64);
    let mut gx = GrayF32::new(g.width, g.height);
    let mut gy = GrayF32::new(g.width, g.height);
    for y in 0..h {
        for x in 0..w {
            let p = |dx: i64, dy: i64| g.get_clamped(x + dx, y + dy);
            let vx = (p(1, -1) + 2.0 * p(1, 0) + p(1, 1)) - (p(-1, -1) + 2.0 * p(-1, 0) + p(-1, 1));
            let vy = (p(-1, 1) + 2.0 * p(0, 1) + p(1, 1)) - (p(-1, -1) + 2.0 * p(0, -1) + p(1, -1));
            let i = (y * w + x) as usize;
            gx.data[i] = vx / 8.0;
            gy.data[i] = vy / 8.0;
        }
    }
    (gx, gy)
}

pub fn magnitude(gx: &GrayF32, gy: &GrayF32) -> GrayF32 {
    GrayF32 { width: gx.width, height: gx.height, data: gx.data.iter().zip(&gy.data).map(|(a, b)| (a * a + b * b).sqrt()).collect() }
}

/// 灰度图的 Sobel 梯度幅值。
pub fn magnitude_of(g: &GrayF32) -> GrayF32 {
    let (gx, gy) = sobel(g);
    magnitude(&gx, &gy)
}

/// RGB 各通道分离为浮点平面。
pub fn split_rgb(img: &ImageBuffer) -> [GrayF32; 3] {
    let mut out = [GrayF32::new(img.width, img.height), GrayF32::new(img.width, img.height), GrayF32::new(img.width, img.height)];
    for (i, px) in img.data.chunks_exact(4).enumerate() {
        out[0].data[i] = px[0] as f32;
        out[1].data[i] = px[1] as f32;
        out[2].data[i] = px[2] as f32;
    }
    out
}

/// 每像素饱和度（HSV S，0..1）。
pub fn saturation(img: &ImageBuffer) -> GrayF32 {
    let mut out = GrayF32::new(img.width, img.height);
    for (i, px) in img.data.chunks_exact(4).enumerate() {
        let mx = px[0].max(px[1]).max(px[2]) as f32;
        let mn = px[0].min(px[1]).min(px[2]) as f32;
        out.data[i] = if mx <= 0.0 { 0.0 } else { (mx - mn) / mx };
    }
    out
}

// ───────────────────────────── 阈值 ─────────────────────────────

/// Otsu 阈值（在 [lo, hi] 上 256 桶直方图）。
pub fn otsu(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let lo = values.iter().copied().fold(f32::MAX, f32::min);
    let hi = values.iter().copied().fold(f32::MIN, f32::max);
    if hi - lo < 1e-6 {
        return hi;
    }
    let mut hist = [0u64; 256];
    let scale = 255.0 / (hi - lo);
    for &v in values {
        hist[((v - lo) * scale).round().clamp(0.0, 255.0) as usize] += 1;
    }
    let total = values.len() as f64;
    let sum_all: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
    let (mut w0, mut sum0, mut best, mut best_t) = (0.0f64, 0.0f64, -1.0f64, 0usize);
    for (t, &c) in hist.iter().enumerate() {
        w0 += c as f64;
        if w0 == 0.0 {
            continue;
        }
        let w1 = total - w0;
        if w1 == 0.0 {
            break;
        }
        sum0 += t as f64 * c as f64;
        let m0 = sum0 / w0;
        let m1 = (sum_all - sum0) / w1;
        let between = w0 * w1 * (m0 - m1) * (m0 - m1);
        if between > best {
            best = between;
            best_t = t;
        }
    }
    lo + (best_t as f32 + 0.5) / scale
}

/// 分位数（0..1）。
pub fn percentile(values: &[f32], q: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v: Vec<f32> = values.to_vec();
    let k = ((v.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize;
    v.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[k]
}

pub fn median(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let k = values.len() / 2;
    values.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values[k]
}

// ───────────────────────────── 形态学 ─────────────────────────────

/// 近似欧氏距离变换（倒角 3-4），返回到最近非零像素的距离（像素）。
pub fn distance_to_nonzero(m: &GrayU8) -> GrayF32 {
    let (w, h) = (m.width as usize, m.height as usize);
    const INF: f32 = 1e9;
    let mut d: Vec<f32> = m.data.iter().map(|&v| if v > 0 { 0.0 } else { INF }).collect();
    let (a, b) = (1.0f32, std::f32::consts::SQRT_2);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + a);
            }
            if y > 0 {
                v = v.min(d[i - w] + a);
                if x > 0 {
                    v = v.min(d[i - w - 1] + b);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + b);
                }
            }
            d[i] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut v = d[i];
            if x + 1 < w {
                v = v.min(d[i + 1] + a);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + a);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + b);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + b);
                }
            }
            d[i] = v;
        }
    }
    GrayF32 { width: m.width, height: m.height, data: d }
}

/// 圆盘膨胀（二值：非零 → 255）。
pub fn dilate(m: &GrayU8, radius: f32) -> GrayU8 {
    if radius <= 0.0 {
        return binarize(m, 1);
    }
    let d = distance_to_nonzero(m);
    GrayU8 { width: m.width, height: m.height, data: d.data.iter().map(|&v| if v <= radius { 255 } else { 0 }).collect() }
}

/// 圆盘腐蚀。
pub fn erode(m: &GrayU8, radius: f32) -> GrayU8 {
    if radius <= 0.0 {
        return binarize(m, 1);
    }
    let inv = GrayU8 { width: m.width, height: m.height, data: m.data.iter().map(|&v| if v > 0 { 0 } else { 255 }).collect() };
    let d = distance_to_nonzero(&inv);
    GrayU8 { width: m.width, height: m.height, data: d.data.iter().map(|&v| if v > radius { 255 } else { 0 }).collect() }
}

pub fn open(m: &GrayU8, radius: f32) -> GrayU8 {
    dilate(&erode(m, radius), radius)
}

pub fn close(m: &GrayU8, radius: f32) -> GrayU8 {
    erode(&dilate(m, radius), radius)
}

pub fn binarize(m: &GrayU8, min: u8) -> GrayU8 {
    GrayU8 { width: m.width, height: m.height, data: m.data.iter().map(|&v| if v >= min { 255 } else { 0 }).collect() }
}

/// 羽化：核心保持 255，外沿按高斯衰减，得到软 Mask。
pub fn feather(m: &GrayU8, radius: f32) -> GrayU8 {
    if radius <= 0.1 {
        return m.clone();
    }
    let g = GrayF32 { width: m.width, height: m.height, data: m.data.iter().map(|&v| v as f32).collect() };
    let b = gaussian_blur(&g, radius / 2.0);
    GrayU8 {
        width: m.width,
        height: m.height,
        data: m
            .data
            .iter()
            .zip(&b.data)
            // 只向外羽化：blur 后的边缘值乘 2 以保持过渡平滑且覆盖原边界
            .map(|(&o, &bv)| o.max((bv * 2.0).min(255.0) as u8))
            .collect(),
    }
}

// ───────────────────────────── 灰度形态学 ─────────────────────────────

/// van Herk / Gil-Werman 一维滑动窗口极值（窗口 2r+1），每像素 O(1)。
fn vhgw_1d(src: &[f32], dst: &mut [f32], r: usize, is_max: bool, g: &mut Vec<f32>, h: &mut Vec<f32>) {
    let n = src.len();
    let k = 2 * r + 1;
    let pad = if is_max { f32::MIN } else { f32::MAX };
    let op = |a: f32, b: f32| if is_max { a.max(b) } else { a.min(b) };
    // 两端各填充 r 个中性值
    let m = n + 2 * r;
    let len = m.div_ceil(k) * k;
    g.clear();
    h.clear();
    g.resize(len, pad);
    h.resize(len, pad);
    let at = |i: usize| if i < r || i >= r + n { pad } else { src[i - r] };
    for b in (0..len).step_by(k) {
        g[b] = at(b);
        for i in b + 1..b + k {
            g[i] = op(g[i - 1], at(i));
        }
        h[b + k - 1] = at(b + k - 1);
        for i in (b..b + k - 1).rev() {
            h[i] = op(h[i + 1], at(i));
        }
    }
    for (i, d) in dst.iter_mut().enumerate().take(n) {
        // 输出 i 对应填充坐标窗口 [i, i+2r]
        *d = op(h[i], g[i + 2 * r]);
    }
}

fn separable_extreme(img: &GrayF32, r: u32, is_max: bool) -> GrayF32 {
    if r == 0 {
        return img.clone();
    }
    let (w, hgt) = (img.width as usize, img.height as usize);
    let r = r as usize;
    let mut tmp = GrayF32::new(img.width, img.height);
    let (mut g, mut h) = (Vec::new(), Vec::new());
    let mut row_out = vec![0.0f32; w];
    for y in 0..hgt {
        vhgw_1d(&img.data[y * w..(y + 1) * w], &mut row_out, r, is_max, &mut g, &mut h);
        tmp.data[y * w..(y + 1) * w].copy_from_slice(&row_out);
    }
    let mut out = GrayF32::new(img.width, img.height);
    let mut col = vec![0.0f32; hgt];
    let mut col_out = vec![0.0f32; hgt];
    for x in 0..w {
        for y in 0..hgt {
            col[y] = tmp.data[y * w + x];
        }
        vhgw_1d(&col, &mut col_out, r, is_max, &mut g, &mut h);
        for y in 0..hgt {
            out.data[y * w + x] = col_out[y];
        }
    }
    out
}

/// 灰度腐蚀（方形窗口，半径 r）。
pub fn gray_erode(img: &GrayF32, r: u32) -> GrayF32 {
    separable_extreme(img, r, false)
}

/// 灰度膨胀（方形窗口，半径 r）。
pub fn gray_dilate(img: &GrayF32, r: u32) -> GrayF32 {
    separable_extreme(img, r, true)
}

/// 白顶帽：亮于周围、宽度小于 2r 的细结构（例如白色半透明文字）。
pub fn tophat_bright(img: &GrayF32, r: u32) -> GrayF32 {
    let opened = gray_dilate(&gray_erode(img, r), r);
    GrayF32 { width: img.width, height: img.height, data: img.data.iter().zip(&opened.data).map(|(a, b)| (a - b).max(0.0)).collect() }
}

/// 黑顶帽：暗于周围的细结构。
pub fn tophat_dark(img: &GrayF32, r: u32) -> GrayF32 {
    let closed = gray_erode(&gray_dilate(img, r), r);
    GrayF32 { width: img.width, height: img.height, data: closed.data.iter().zip(&img.data).map(|(a, b)| (a - b).max(0.0)).collect() }
}

/// 逐通道顶帽取最大值：对彩色 Logo 也敏感。返回（亮顶帽, 暗顶帽）。
pub fn tophat_rgb(img: &ImageBuffer, r: u32) -> (GrayF32, GrayF32) {
    let planes = split_rgb(img);
    let mut b = GrayF32::new(img.width, img.height);
    let mut d = GrayF32::new(img.width, img.height);
    for p in &planes {
        let tb = tophat_bright(p, r);
        let td = tophat_dark(p, r);
        for i in 0..b.data.len() {
            b.data[i] = b.data[i].max(tb.data[i]);
            d.data[i] = d.data[i].max(td.data[i]);
        }
    }
    (b, d)
}

// ───────────────────────────── 连通域 ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Component {
    pub label: u32,
    pub area: u32,
    pub rect: PixelRect,
    pub cx: f32,
    pub cy: f32,
}

/// 8 连通标记。返回（标签图，组件列表）；标签 0 为背景，组件标签从 1 开始。
pub fn connected_components(m: &GrayU8) -> (Vec<u32>, Vec<Component>) {
    let (w, h) = (m.width as usize, m.height as usize);
    let mut labels = vec![0u32; w * h];
    let mut comps = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if m.data[start] == 0 || labels[start] != 0 {
            continue;
        }
        let label = comps.len() as u32 + 1;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        let (mut area, mut sx, mut sy) = (0u32, 0f64, 0f64);
        labels[start] = label;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            area += 1;
            sx += x as f64;
            sy += y as f64;
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = x as i64 + dx;
                    let ny = y as i64 + dy;
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if m.data[j] > 0 && labels[j] == 0 {
                        labels[j] = label;
                        stack.push(j);
                    }
                }
            }
        }
        comps.push(Component {
            label,
            area,
            rect: PixelRect::new(x0 as u32, y0 as u32, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32),
            cx: (sx / area as f64) as f32,
            cy: (sy / area as f64) as f32,
        });
    }
    (labels, comps)
}

/// 移除面积小于 `min_area` 的连通域。
pub fn remove_small_components(m: &GrayU8, min_area: u32) -> GrayU8 {
    if min_area <= 1 {
        return m.clone();
    }
    let (labels, comps) = connected_components(m);
    let keep: Vec<bool> = std::iter::once(false).chain(comps.iter().map(|c| c.area >= min_area)).collect();
    GrayU8 {
        width: m.width,
        height: m.height,
        data: m.data.iter().zip(&labels).map(|(&v, &l)| if keep[l as usize] { v } else { 0 }).collect(),
    }
}

// ───────────────────────────── 统计 ─────────────────────────────

/// Mask 周围环带（距离 (0, ring]）的平均梯度幅值，归一化为 0..1 的场景复杂度。
pub fn ring_complexity(img: &ImageBuffer, mask: &GrayU8, ring: f32) -> f32 {
    let g = img.to_luma_f32();
    let (gx, gy) = sobel(&g);
    let mag = magnitude(&gx, &gy);
    let d = distance_to_nonzero(mask);
    let (mut s, mut n) = (0.0f64, 0u64);
    for (i, &dv) in d.data.iter().enumerate() {
        if dv > 0.0 && dv <= ring {
            s += mag.data[i] as f64;
            n += 1;
        }
    }
    if n == 0 {
        return 0.0;
    }
    // 经验归一化：均值梯度 ~25（/255 刻度，Sobel 已 /8）视为高复杂度
    ((s / n as f64) as f32 / 25.0).clamp(0.0, 1.0)
}

/// 归一化互相关（等尺寸）。
pub fn ncc(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let ma = a[..n].iter().sum::<f32>() / n as f32;
    let mb = b[..n].iter().sum::<f32>() / n as f32;
    let (mut num, mut da, mut db) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let x = (a[i] - ma) as f64;
        let y = (b[i] - mb) as f64;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 1e-12 || db <= 1e-12 {
        0.0
    } else {
        (num / (da.sqrt() * db.sqrt())) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otsu_separates_two_modes() {
        let mut v = vec![10.0f32; 100];
        v.extend(vec![200.0f32; 100]);
        let t = otsu(&v);
        assert!(t > 10.0 && t < 200.0, "t={t}");
    }

    #[test]
    fn dilate_erode_disk() {
        let mut m = GrayU8::new(21, 21);
        m.set(10, 10, 255);
        let d = dilate(&m, 3.0);
        assert_eq!(d.get(13, 10), 255);
        assert_eq!(d.get(14, 10), 0);
        let e = erode(&d, 2.0);
        assert_eq!(e.get(10, 10), 255);
        assert_eq!(e.get(13, 10), 0);
    }

    #[test]
    fn components_and_small_removal() {
        let mut m = GrayU8::new(10, 10);
        for y in 0..3 {
            for x in 0..3 {
                m.set(x, y, 255);
            }
        }
        m.set(8, 8, 255);
        let (_, comps) = connected_components(&m);
        assert_eq!(comps.len(), 2);
        let r = remove_small_components(&m, 2);
        assert_eq!(r.get(8, 8), 0);
        assert_eq!(r.get(1, 1), 255);
    }

    #[test]
    fn box_blur_preserves_constant() {
        let g = GrayF32 { width: 5, height: 5, data: vec![7.0; 25] };
        let b = box_blur(&g, 2);
        assert!(b.data.iter().all(|&v| (v - 7.0).abs() < 1e-4));
        let gb = gaussian_blur(&g, 1.5);
        assert!(gb.data.iter().all(|&v| (v - 7.0).abs() < 1e-3));
    }

    #[test]
    fn vhgw_matches_naive() {
        let data: Vec<f32> = (0..97).map(|i| ((i * 37) % 23) as f32).collect();
        let g = GrayF32 { width: 97, height: 1, data: data.clone() };
        for r in [1u32, 3, 7] {
            let e = gray_erode(&g, r);
            let d = gray_dilate(&g, r);
            for i in 0..97usize {
                let lo = i.saturating_sub(r as usize);
                let hi = (i + r as usize).min(96);
                let mn = data[lo..=hi].iter().copied().fold(f32::MAX, f32::min);
                let mx = data[lo..=hi].iter().copied().fold(f32::MIN, f32::max);
                assert_eq!(e.data[i], mn, "erode r={r} i={i}");
                assert_eq!(d.data[i], mx, "dilate r={r} i={i}");
            }
        }
    }

    #[test]
    fn tophat_finds_thin_bright_line() {
        let mut g = GrayF32 { width: 40, height: 40, data: vec![100.0; 1600] };
        for x in 0..40 {
            g.set(x, 20, 180.0);
        }
        let t = tophat_bright(&g, 3);
        assert!((t.get(10, 20) - 80.0).abs() < 1e-3);
        assert!(t.get(10, 10) < 1e-3);
    }

    #[test]
    fn ncc_identity() {
        let a = [1.0, 2.0, 3.0, 4.0];
        assert!((ncc(&a, &a) - 1.0).abs() < 1e-5);
        let b = [4.0, 3.0, 2.0, 1.0];
        assert!((ncc(&a, &b) + 1.0).abs() < 1e-5);
    }
}
