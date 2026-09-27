//! 像素级 Mask（规格 §5）。
//!
//! Mask 以 **区域** 形式保存（每个区域是一块局部软 Mask），避免对 20K×20K 的大图分配整幅 Mask。
//! 每个 Mask 带有尺寸、坐标空间和版本，并绑定到对应输入文件与候选。

use crate::buffer::GrayU8;
use crate::geometry::{CoordSpace, PixelRect, Point};
use serde::{Deserialize, Serialize};

/// 一块局部软 Mask，`data` 取值 0..255（255 = 完全属于水印）。
#[derive(Debug, Clone, PartialEq)]
pub struct MaskRegion {
    pub rect: PixelRect,
    pub data: GrayU8,
    pub candidate_id: Option<String>,
}

impl MaskRegion {
    pub fn new(rect: PixelRect, data: GrayU8, candidate_id: Option<String>) -> Self {
        debug_assert_eq!((rect.width, rect.height), (data.width, data.height));
        Self { rect, data, candidate_id }
    }
    pub fn coverage(&self) -> usize {
        self.data.count_nonzero()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WatermarkMask {
    pub id: String,
    /// 所属输入文件 id。
    pub file_id: String,
    /// 对应图片（已定向）尺寸。
    pub width: u32,
    pub height: u32,
    pub space: CoordSpace,
    /// 每次编辑自增；缓存与结果以 (mask_id, version) 标识。
    pub version: u32,
    pub regions: Vec<MaskRegion>,
}

impl WatermarkMask {
    pub fn new(id: String, file_id: String, width: u32, height: u32) -> Self {
        Self { id, file_id, width, height, space: CoordSpace::OrientedImage { width, height }, version: 1, regions: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.iter().all(|r| r.coverage() == 0)
    }

    /// 所有非零像素的外接矩形。
    pub fn bounds(&self) -> Option<PixelRect> {
        let mut acc: Option<PixelRect> = None;
        for r in &self.regions {
            if let Some(b) = r.data.nonzero_bounds() {
                let b = PixelRect::new(r.rect.x + b.x, r.rect.y + b.y, b.width, b.height);
                acc = Some(match acc {
                    Some(a) => a.union(&b),
                    None => b,
                });
            }
        }
        acc
    }

    /// 覆盖像素数（区域重叠时可能重复计数，用于路由阈值估计足够）。
    pub fn coverage(&self) -> usize {
        self.regions.iter().map(|r| r.coverage()).sum()
    }

    pub fn area_ratio(&self) -> f32 {
        let total = self.width as f64 * self.height as f64;
        if total <= 0.0 {
            0.0
        } else {
            (self.coverage() as f64 / total) as f32
        }
    }

    /// 把指定矩形范围内的 Mask 合成为一块稠密 Mask（取各区域最大值）。
    pub fn rasterize(&self, rect: &PixelRect) -> GrayU8 {
        let mut out = GrayU8::new(rect.width, rect.height);
        for r in &self.regions {
            let x0 = r.rect.x.max(rect.x);
            let y0 = r.rect.y.max(rect.y);
            let x1 = r.rect.right().min(rect.right());
            let y1 = r.rect.bottom().min(rect.bottom());
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            for y in y0..y1 {
                for x in x0..x1 {
                    let v = r.data.get(x - r.rect.x, y - r.rect.y);
                    let i = out.idx(x - rect.x, y - rect.y);
                    if v > out.data[i] {
                        out.data[i] = v;
                    }
                }
            }
        }
        out
    }
}

/// 用户在 MaskEditor 中的编辑操作。坐标为归一化坐标（相对已定向原图），
/// 这样前端只需传输矢量操作，不在 IPC 中搬运像素。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MaskOp {
    /// 画笔 / 橡皮擦：沿折线以半径 `radius`（相对图像较长边）绘制。
    Brush {
        points: Vec<Point>,
        radius: f32,
        erase: bool,
    },
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        erase: bool,
    },
    Polygon {
        points: Vec<Point>,
        erase: bool,
    },
    /// 清空全部 Mask。
    Clear,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterize_merges_regions_with_max() {
        let mut m = WatermarkMask::new("m".into(), "f".into(), 20, 20);
        let mut a = GrayU8::new(4, 4);
        a.data.fill(100);
        let mut b = GrayU8::new(4, 4);
        b.data.fill(200);
        m.regions.push(MaskRegion::new(PixelRect::new(2, 2, 4, 4), a, None));
        m.regions.push(MaskRegion::new(PixelRect::new(4, 4, 4, 4), b, None));
        let r = m.rasterize(&PixelRect::new(0, 0, 10, 10));
        assert_eq!(r.get(2, 2), 100);
        assert_eq!(r.get(5, 5), 200);
        assert_eq!(r.get(9, 9), 0);
        assert_eq!(m.bounds(), Some(PixelRect::new(2, 2, 6, 6)));
    }
}
