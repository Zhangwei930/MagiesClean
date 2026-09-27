//! 批次学习（规格 §6）：抽样策略与 BatchWatermarkProfile。

use crate::candidate::WatermarkType;
use crate::geometry::BoundingBox;
use serde::{Deserialize, Serialize};

/// 水印在图片中的锚定方式：跨尺寸匹配时用于位置修正。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// 根据归一化中心点推断锚点（三分法）。
    pub fn from_center(cx: f32, cy: f32) -> Anchor {
        let col = if cx < 1.0 / 3.0 {
            0
        } else if cx > 2.0 / 3.0 {
            2
        } else {
            1
        };
        let row = if cy < 1.0 / 3.0 {
            0
        } else if cy > 2.0 / 3.0 {
            2
        } else {
            1
        };
        match (row, col) {
            (0, 0) => Anchor::TopLeft,
            (0, 1) => Anchor::Top,
            (0, _) => Anchor::TopRight,
            (1, 0) => Anchor::Left,
            (1, 1) => Anchor::Center,
            (1, _) => Anchor::Right,
            (_, 0) => Anchor::BottomLeft,
            (_, 1) => Anchor::Bottom,
            _ => Anchor::BottomRight,
        }
    }
}

/// 模板的缩放方式：水印随图片等比缩放，或以固定像素大小锚定在角落。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScaleMode {
    /// 位置与大小都相对图片（normalized_bbox 有效）。
    Relative,
    /// 固定像素大小：`offset` 为模板左上角相对锚点的偏移（参考长边 `ref_long_side` 下的像素）。
    Anchored { ref_long_side: u32, offset_x: f32, offset_y: f32 },
}

/// 学习到的水印模板（在学习画布坐标下）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileTemplate {
    pub width: u32,
    pub height: u32,
    /// 学习画布的长边像素数（模板尺寸相对它定义）。
    pub canvas_long_side: u32,
    /// 水印梯度的中值估计（Sobel x / y）。
    pub grad_x: Vec<f32>,
    pub grad_y: Vec<f32>,
    /// 模板范围内的水印像素概率（0..1），用于生成 Mask。
    pub support: Vec<f32>,
    /// 估计的 alpha（0..1）；无法估计时为空。
    pub alpha: Vec<f32>,
    /// 估计的水印颜色（RGB 0..255）。
    pub color: [f32; 3],
    /// alpha 估计质量（0..1）。
    pub alpha_quality: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchWatermarkProfile {
    pub id: String,
    /// 在学习分组中的归一化位置。
    pub normalized_bbox: BoundingBox,
    pub template_hash: String,
    /// 紧凑特征向量（梯度方向直方图），用于快速筛选。
    pub feature_embedding: Vec<f32>,
    pub confidence: f32,
    pub sample_count: usize,
    pub group_key: String,
    pub anchor: Anchor,
    pub scale_mode: ScaleMode,
    pub watermark_type: WatermarkType,
    pub template: ProfileTemplate,
}

/// 默认抽样数（规格 §6 表；边界已统一：101–500 抽 20，501 及以上抽 30）。
pub fn default_sample_count(n: usize) -> usize {
    match n {
        0 => 0,
        1..=10 => n,
        11..=100 => 10,
        101..=500 => 20,
        _ => 30,
    }
}

/// 在 `n` 个元素中均匀抽取 `count` 个下标，覆盖前部、中部与后部（不得只取最前面的 N 张）。
pub fn spread_indices(n: usize, count: usize) -> Vec<usize> {
    if n == 0 || count == 0 {
        return Vec::new();
    }
    if count >= n {
        return (0..n).collect();
    }
    if count == 1 {
        return vec![n / 2];
    }
    let mut out: Vec<usize> = (0..count).map(|i| ((i as f64) * (n - 1) as f64 / (count - 1) as f64).round() as usize).collect();
    out.dedup();
    out
}

/// 按长宽比分组的键：异质批次需分组学习。
pub fn aspect_group_key(width: u32, height: u32) -> String {
    let r = (width.max(1) as f64 / height.max(1) as f64).log2();
    // 以 0.25（约 ±19%）为桶宽
    let bucket = (r / 0.25).round() as i32;
    format!("ar{bucket:+}")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleGroup {
    pub key: String,
    /// 组内全部成员（输入下标）。
    pub members: Vec<usize>,
    /// 抽样得到的成员下标。
    pub samples: Vec<usize>,
}

/// 对一批 `(index, width, height)` 分组并抽样。每组抽样数按组大小与总预算分配，
/// 同时在组内按尺寸排序后均匀抽取，以覆盖不同尺寸。
pub fn group_and_sample(items: &[(usize, u32, u32)], sample_override: Option<usize>) -> Vec<SampleGroup> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<(usize, u32, u32)>> = BTreeMap::new();
    for &(i, w, h) in items {
        groups.entry(aspect_group_key(w, h)).or_default().push((i, w, h));
    }
    let total = items.len();
    let budget = sample_override.unwrap_or_else(|| default_sample_count(total)).max(1);
    groups
        .into_iter()
        .map(|(key, mut members)| {
            // 保持导入顺序以覆盖前/中/后部，再在抽样时按尺寸分层
            let share = ((budget as f64) * members.len() as f64 / total.max(1) as f64).ceil() as usize;
            let count = share.clamp(members.len().min(3), members.len());
            let order: Vec<usize> = members.iter().map(|m| m.0).collect();
            let picks_by_order = spread_indices(order.len(), count.div_ceil(2));
            members.sort_by_key(|m| m.1 as u64 * m.2 as u64);
            let picks_by_size = spread_indices(members.len(), count / 2);
            let mut samples: Vec<usize> =
                picks_by_order.into_iter().map(|k| order[k]).chain(picks_by_size.into_iter().map(|k| members[k].0)).collect();
            samples.sort_unstable();
            samples.dedup();
            // 去重后不足时从顺序中补齐
            let mut k = 0;
            while samples.len() < count && k < order.len() {
                if !samples.contains(&order[k]) {
                    samples.push(order[k]);
                }
                k += 1;
            }
            samples.sort_unstable();
            SampleGroup { key, members: order, samples }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_table_matches_spec() {
        assert_eq!(default_sample_count(1), 1);
        assert_eq!(default_sample_count(10), 10);
        assert_eq!(default_sample_count(11), 10);
        assert_eq!(default_sample_count(100), 10);
        assert_eq!(default_sample_count(101), 20);
        assert_eq!(default_sample_count(500), 20);
        assert_eq!(default_sample_count(501), 30);
        assert_eq!(default_sample_count(5000), 30);
    }

    #[test]
    fn spread_covers_front_middle_back() {
        let s = spread_indices(100, 10);
        assert_eq!(s.len(), 10);
        assert_eq!(s[0], 0);
        assert_eq!(*s.last().unwrap(), 99);
        assert!(s.iter().any(|&i| (40..60).contains(&i)));
    }

    #[test]
    fn heterogeneous_batch_is_grouped() {
        let mut items = Vec::new();
        for i in 0..60 {
            items.push((i, 1200, 800));
        }
        for i in 60..100 {
            items.push((i, 800, 1200));
        }
        let groups = group_and_sample(&items, None);
        assert_eq!(groups.len(), 2);
        let total_samples: usize = groups.iter().map(|g| g.samples.len()).sum();
        assert!(total_samples >= 10);
        for g in &groups {
            assert!(g.samples.len() >= 3);
            assert!(g.samples.iter().all(|s| g.members.contains(s)));
        }
    }

    #[test]
    fn anchor_from_center() {
        assert_eq!(Anchor::from_center(0.9, 0.95), Anchor::BottomRight);
        assert_eq!(Anchor::from_center(0.5, 0.5), Anchor::Center);
        assert_eq!(Anchor::from_center(0.1, 0.1), Anchor::TopLeft);
    }
}
