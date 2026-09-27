//! 与具体图像 SDK 无关的像素缓冲区与张量类型。
//!
//! 所有处理链统一使用 RGBA8（已定向、sRGB 语义），由 `wm-image` 负责与编解码器互转。

use crate::geometry::PixelRect;

/// RGBA8 像素缓冲区（行主序，无填充）。
#[derive(Clone, PartialEq)]
pub struct ImageBuffer {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for ImageBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageBuffer({}x{})", self.width, self.height)
    }
}

impl ImageBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize * 4] }
    }

    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..(width as usize * height as usize) {
            data.extend_from_slice(&rgba);
        }
        Self { width, height, data }
    }

    pub fn from_raw(width: u32, height: u32, data: Vec<u8>) -> Option<Self> {
        (data.len() == width as usize * height as usize * 4).then_some(Self { width, height, data })
    }

    #[inline]
    pub fn idx(&self, x: u32, y: u32) -> usize {
        (y as usize * self.width as usize + x as usize) * 4
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let i = self.idx(x, y);
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    #[inline]
    pub fn put(&mut self, x: u32, y: u32, px: [u8; 4]) {
        let i = self.idx(x, y);
        self.data[i..i + 4].copy_from_slice(&px);
    }

    pub fn byte_size(&self) -> usize {
        self.data.len()
    }

    pub fn full_rect(&self) -> PixelRect {
        PixelRect::new(0, 0, self.width, self.height)
    }

    /// 复制出一个区域。
    pub fn crop(&self, r: &PixelRect) -> ImageBuffer {
        let r = clamp_rect(r, self.width, self.height);
        let mut out = ImageBuffer::new(r.width, r.height);
        let row = r.width as usize * 4;
        for y in 0..r.height {
            let src = self.idx(r.x, r.y + y);
            let dst = y as usize * row;
            out.data[dst..dst + row].copy_from_slice(&self.data[src..src + row]);
        }
        out
    }

    /// 把 `patch` 写回到 `(x, y)` 位置。
    pub fn paste(&mut self, patch: &ImageBuffer, x: u32, y: u32) {
        let w = patch.width.min(self.width.saturating_sub(x));
        let h = patch.height.min(self.height.saturating_sub(y));
        let row = w as usize * 4;
        for yy in 0..h {
            let dst = self.idx(x, y + yy);
            let src = patch.idx(0, yy);
            self.data[dst..dst + row].copy_from_slice(&patch.data[src..src + row]);
        }
    }

    /// 转灰度（BT.601 亮度，0..255 浮点）。
    pub fn to_luma_f32(&self) -> GrayF32 {
        let mut out = GrayF32::new(self.width, self.height);
        for (i, px) in self.data.chunks_exact(4).enumerate() {
            out.data[i] = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
        }
        out
    }

    pub fn has_transparency(&self) -> bool {
        self.data.chunks_exact(4).any(|p| p[3] != 255)
    }
}

pub fn clamp_rect(r: &PixelRect, width: u32, height: u32) -> PixelRect {
    let x = r.x.min(width);
    let y = r.y.min(height);
    PixelRect::new(x, y, r.width.min(width - x), r.height.min(height - y))
}

/// 单通道浮点图（灰度、梯度、概率图等）。
#[derive(Clone, PartialEq)]
pub struct GrayF32 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

impl std::fmt::Debug for GrayF32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GrayF32({}x{})", self.width, self.height)
    }
}

impl GrayF32 {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0.0; width as usize * height as usize] }
    }
    #[inline]
    pub fn idx(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> f32 {
        self.data[self.idx(x, y)]
    }
    #[inline]
    pub fn set(&mut self, x: u32, y: u32, v: f32) {
        let i = self.idx(x, y);
        self.data[i] = v;
    }
    /// 边界钳制读取。
    #[inline]
    pub fn get_clamped(&self, x: i64, y: i64) -> f32 {
        let xx = x.clamp(0, self.width as i64 - 1) as u32;
        let yy = y.clamp(0, self.height as i64 - 1) as u32;
        self.get(xx, yy)
    }
    pub fn crop(&self, r: &PixelRect) -> GrayF32 {
        let r = clamp_rect(r, self.width, self.height);
        let mut out = GrayF32::new(r.width, r.height);
        for y in 0..r.height {
            let s = self.idx(r.x, r.y + y);
            let d = out.idx(0, y);
            out.data[d..d + r.width as usize].copy_from_slice(&self.data[s..s + r.width as usize]);
        }
        out
    }
    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(f32::MIN, f32::max)
    }
    pub fn mean(&self) -> f32 {
        if self.data.is_empty() {
            0.0
        } else {
            self.data.iter().sum::<f32>() / self.data.len() as f32
        }
    }
}

/// 单通道 u8 图（二值或软 Mask）。
#[derive(Clone, PartialEq)]
pub struct GrayU8 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for GrayU8 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GrayU8({}x{})", self.width, self.height)
    }
}

impl GrayU8 {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize] }
    }
    #[inline]
    pub fn idx(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> u8 {
        self.data[self.idx(x, y)]
    }
    #[inline]
    pub fn set(&mut self, x: u32, y: u32, v: u8) {
        let i = self.idx(x, y);
        self.data[i] = v;
    }
    pub fn count_nonzero(&self) -> usize {
        self.data.iter().filter(|&&v| v > 0).count()
    }
    pub fn crop(&self, r: &PixelRect) -> GrayU8 {
        let r = clamp_rect(r, self.width, self.height);
        let mut out = GrayU8::new(r.width, r.height);
        for y in 0..r.height {
            let s = self.idx(r.x, r.y + y);
            let d = out.idx(0, y);
            out.data[d..d + r.width as usize].copy_from_slice(&self.data[s..s + r.width as usize]);
        }
        out
    }
    /// 非零像素的最小外接矩形。
    pub fn nonzero_bounds(&self) -> Option<PixelRect> {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for y in 0..self.height {
            for x in 0..self.width {
                if self.get(x, y) > 0 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        (x0 != u32::MAX).then(|| PixelRect::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1))
    }
}

/// 模型输入输出张量（NCHW，f32）。
#[derive(Debug, Clone, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl Tensor {
    pub fn new(shape: Vec<usize>, data: Vec<f32>) -> Option<Self> {
        (shape.iter().product::<usize>() == data.len()).then_some(Self { shape, data })
    }
    pub fn zeros(shape: Vec<usize>) -> Self {
        let n = shape.iter().product();
        Self { shape, data: vec![0.0; n] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_and_paste_roundtrip() {
        let mut img = ImageBuffer::filled(8, 6, [10, 20, 30, 255]);
        img.put(3, 2, [1, 2, 3, 4]);
        let c = img.crop(&PixelRect::new(2, 1, 3, 3));
        assert_eq!(c.get(1, 1), [1, 2, 3, 4]);
        let mut dst = ImageBuffer::new(8, 6);
        dst.paste(&c, 2, 1);
        assert_eq!(dst.get(3, 2), [1, 2, 3, 4]);
    }

    #[test]
    fn nonzero_bounds() {
        let mut m = GrayU8::new(10, 10);
        m.set(2, 3, 255);
        m.set(7, 5, 10);
        assert_eq!(m.nonzero_bounds(), Some(PixelRect::new(2, 3, 6, 3)));
    }
}
