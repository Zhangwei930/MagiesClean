//! 几何与坐标空间（规格 §5 实现契约、§8.1）。
//!
//! 检测在缩略图上进行，结果以 **归一化坐标**（相对已按 EXIF 定向的原图，0..1）保存，
//! 再映射回原图像素坐标。PDF 页面坐标、缩略图坐标与模型裁剪坐标不可混用，
//! 因此所有 Mask 与候选都显式带上 [`CoordSpace`]。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// 轴对齐包围盒。单位由所在的 [`CoordSpace`] 决定。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl BoundingBox {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width: width.max(0.0), height: height.max(0.0) }
    }

    pub fn from_corners(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self::new(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs())
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }
    pub fn area(&self) -> f32 {
        self.width * self.height
    }
    pub fn center(&self) -> Point {
        Point { x: self.x + self.width / 2.0, y: self.y + self.height / 2.0 }
    }
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    pub fn intersection(&self, o: &BoundingBox) -> Option<BoundingBox> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        (x1 > x0 && y1 > y0).then(|| BoundingBox::from_corners(x0, y0, x1, y1))
    }

    pub fn union(&self, o: &BoundingBox) -> BoundingBox {
        BoundingBox::from_corners(self.x.min(o.x), self.y.min(o.y), self.right().max(o.right()), self.bottom().max(o.bottom()))
    }

    pub fn iou(&self, o: &BoundingBox) -> f32 {
        let inter = self.intersection(o).map(|b| b.area()).unwrap_or(0.0);
        let uni = self.area() + o.area() - inter;
        if uni <= 0.0 {
            0.0
        } else {
            inter / uni
        }
    }

    /// 交集占较小框面积的比例，用于判断包含关系。
    pub fn containment(&self, o: &BoundingBox) -> f32 {
        let inter = self.intersection(o).map(|b| b.area()).unwrap_or(0.0);
        let m = self.area().min(o.area());
        if m <= 0.0 {
            0.0
        } else {
            inter / m
        }
    }

    pub fn expand(&self, pad_x: f32, pad_y: f32) -> BoundingBox {
        BoundingBox::from_corners(self.x - pad_x, self.y - pad_y, self.right() + pad_x, self.bottom() + pad_y)
    }

    pub fn clamp_to(&self, width: f32, height: f32) -> BoundingBox {
        let x0 = self.x.clamp(0.0, width);
        let y0 = self.y.clamp(0.0, height);
        let x1 = self.right().clamp(0.0, width);
        let y1 = self.bottom().clamp(0.0, height);
        BoundingBox::from_corners(x0, y0, x1, y1)
    }

    /// 归一化坐标 → 像素坐标。
    pub fn to_pixels(&self, width: u32, height: u32) -> BoundingBox {
        BoundingBox::new(self.x * width as f32, self.y * height as f32, self.width * width as f32, self.height * height as f32)
    }

    /// 像素坐标 → 归一化坐标。
    pub fn to_normalized(&self, width: u32, height: u32) -> BoundingBox {
        let (w, h) = (width.max(1) as f32, height.max(1) as f32);
        BoundingBox::new(self.x / w, self.y / h, self.width / w, self.height / h)
    }

    /// 取整后的像素矩形（向外取整并裁剪到图像范围内）。
    pub fn to_pixel_rect(&self, width: u32, height: u32) -> PixelRect {
        let x0 = self.x.floor().max(0.0) as u32;
        let y0 = self.y.floor().max(0.0) as u32;
        let x1 = (self.right().ceil().max(0.0) as u32).min(width);
        let y1 = (self.bottom().ceil().max(0.0) as u32).min(height);
        PixelRect { x: x0.min(width), y: y0.min(height), width: x1.saturating_sub(x0), height: y1.saturating_sub(y0) }
    }
}

/// 整数像素矩形。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
    pub fn right(&self) -> u32 {
        self.x + self.width
    }
    pub fn bottom(&self) -> u32 {
        self.y + self.height
    }
    pub fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub fn union(&self, o: &PixelRect) -> PixelRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = self.right().max(o.right());
        let y1 = self.bottom().max(o.bottom());
        PixelRect::new(x0, y0, x1 - x0, y1 - y0)
    }
    /// 向四周扩展 `pad` 像素并裁剪到 `(width, height)`。
    pub fn pad(&self, pad: u32, width: u32, height: u32) -> PixelRect {
        let x0 = self.x.saturating_sub(pad);
        let y0 = self.y.saturating_sub(pad);
        let x1 = (self.right() + pad).min(width);
        let y1 = (self.bottom() + pad).min(height);
        PixelRect::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }
    pub fn to_bbox(&self) -> BoundingBox {
        BoundingBox::new(self.x as f32, self.y as f32, self.width as f32, self.height as f32)
    }
}

/// 坐标空间标签。Mask / 候选在跨阶段传递时必须携带。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoordSpace {
    /// 相对已定向原图的 0..1 坐标。
    Normalized,
    /// 已按 EXIF Orientation 旋转后的原图像素坐标。
    OrientedImage { width: u32, height: u32 },
    /// 缩略图像素坐标（仅检测阶段内部使用）。
    Thumbnail { width: u32, height: u32 },
    /// PDF 页面坐标（point，原点左下）。
    PdfPage { page: u32, width_pt: f32, height_pt: f32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_and_containment() {
        let a = BoundingBox::new(0.0, 0.0, 10.0, 10.0);
        let b = BoundingBox::new(5.0, 5.0, 10.0, 10.0);
        assert!((a.iou(&b) - 25.0 / 175.0).abs() < 1e-6);
        let c = BoundingBox::new(2.0, 2.0, 3.0, 3.0);
        assert!((a.containment(&c) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalized_roundtrip() {
        let px = BoundingBox::new(100.0, 50.0, 200.0, 25.0);
        let n = px.to_normalized(1000, 500);
        let back = n.to_pixels(1000, 500);
        assert!((back.x - 100.0).abs() < 1e-3 && (back.height - 25.0).abs() < 1e-3);
    }

    #[test]
    fn pixel_rect_clamps() {
        let r = BoundingBox::new(-5.0, 90.0, 20.0, 20.0).to_pixel_rect(100, 100);
        assert_eq!(r, PixelRect::new(0, 90, 15, 10));
        let p = PixelRect::new(95, 95, 5, 5).pad(10, 100, 100);
        assert_eq!(p, PixelRect::new(85, 85, 15, 15));
    }
}
