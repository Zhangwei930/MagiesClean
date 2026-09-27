//! Pipeline 契约（规格 §4.1、§5.1、§7）。
//!
//! 具体实现位于 wm-detection / wm-segmentation / wm-removal / wm-ai，
//! 由 wm-runtime 组合注入。core 不依赖任何图像 SDK 或模型运行时。

use crate::batch::BatchWatermarkProfile;
use crate::buffer::{GrayF32, GrayU8, ImageBuffer, Tensor};
use crate::candidate::{DetectorSource, WatermarkCandidate};
use crate::decision::RemovalRoute;
use crate::error::{AppError, Result};
use crate::geometry::PixelRect;
use crate::mask::{MaskRegion, WatermarkMask};
use crate::settings::{MaskParams, QualityMode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 可在线程间共享的取消令牌。所有长任务都需要定期检查。
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    /// 已取消时返回 `Err(Cancelled)`，便于 `?` 传播。
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(AppError::cancelled())
        } else {
            Ok(())
        }
    }
}

/// 统一检测输入。`image` 为已定向的检测用缩略图（长边约 2048 px）。
pub struct DetectionInput<'a> {
    pub file_id: &'a str,
    pub image: &'a ImageBuffer,
    pub original_width: u32,
    pub original_height: u32,
    pub profiles: &'a [BatchWatermarkProfile],
    pub quality: QualityMode,
    pub cancel: &'a CancellationToken,
}

/// 检测器可以附带的像素级提示：在 `bbox`（归一化坐标）范围内的软概率图。
/// 分割器会在原图分辨率下以它为先验精修 Mask（Bounding Box 不能直接作为最终 Mask）。
#[derive(Debug, Clone, PartialEq)]
pub struct MaskHint {
    pub bbox: crate::geometry::BoundingBox,
    pub data: GrayU8,
}

/// 检测结果：候选 + 可选的 Mask 提示。
#[derive(Debug, Clone)]
pub struct Detection {
    pub candidate: WatermarkCandidate,
    pub hint: Option<MaskHint>,
}

impl Detection {
    pub fn new(candidate: WatermarkCandidate) -> Self {
        Self { candidate, hint: None }
    }
}

pub trait WatermarkDetector: Send + Sync {
    fn name(&self) -> &'static str;
    fn source(&self) -> DetectorSource;
    fn detect(&self, input: &DetectionInput) -> Result<Vec<Detection>>;
}

/// 分割上下文：`image` 为全分辨率已定向图的局部裁剪，`crop` 为其在原图中的位置。
pub struct SegmentContext<'a> {
    pub image: &'a ImageBuffer,
    pub crop: PixelRect,
    pub full_width: u32,
    pub full_height: u32,
    pub params: &'a MaskParams,
    pub hint: Option<&'a MaskHint>,
    pub quality: QualityMode,
}

/// 候选区域 → 像素 Mask。只覆盖真实水印像素，不能把整块矩形当作 Mask。
pub trait WatermarkSegmenter: Send + Sync {
    fn name(&self) -> &'static str;
    fn segment(&self, candidate: &WatermarkCandidate, ctx: &SegmentContext) -> Result<Option<MaskRegion>>;
}

/// 半透明水印的 alpha 蒙版估计（原图像素坐标）。
#[derive(Debug, Clone)]
pub struct AlphaMatte {
    pub rect: PixelRect,
    pub alpha: GrayF32,
    pub color: [f32; 3],
    pub quality: f32,
}

#[derive(Debug, Clone)]
pub struct RemovalOptions {
    pub quality: QualityMode,
    pub alpha: Vec<AlphaMatte>,
    pub cancel: CancellationToken,
    /// 不可作为修复上下文的像素：文件内全部候选与手动区域（含未选择去除的）。
    /// AI 修复据此让每个区域的结果只取决于原图，便于按区域缓存；None 时只把待去除区域视为洞。
    pub unknown: Option<std::sync::Arc<crate::WatermarkMask>>,
}

impl Default for RemovalOptions {
    fn default() -> Self {
        Self { quality: QualityMode::Balanced, alpha: Vec::new(), cancel: CancellationToken::new(), unknown: None }
    }
}

/// 去除引擎。为满足大图有界内存要求，实现 **原地** 修改 `image`，
/// 且只触碰 Mask 覆盖区域及其必要邻域（规格 §8.2）。
pub trait WatermarkRemover: Send + Sync {
    fn route(&self) -> RemovalRoute;
    fn remove(&self, image: &mut ImageBuffer, mask: &WatermarkMask, options: &RemovalOptions) -> Result<()>;
}

/// 本地 AI Inpainting 后端。具体模型不写死在 core。
pub trait AiInpaintBackend: Send + Sync {
    /// 模型期望的输入尺寸（宽、高）。
    fn input_size(&self) -> (u32, u32);
    /// image: [1,3,H,W] 0..1；mask: [1,1,H,W] 0/1；返回 [1,3,H,W] 0..1。
    fn inpaint(&self, image: &Tensor, mask: &Tensor) -> Result<Tensor>;
}

/// OCR 识别结果（像素坐标相对检测输入）。
#[derive(Debug, Clone)]
pub struct OcrLine {
    pub text: String,
    pub bbox: crate::geometry::BoundingBox,
    pub rotation: f32,
    pub score: f32,
}

pub trait OcrEngine: Send + Sync {
    fn recognize(&self, image: &ImageBuffer) -> Result<Vec<OcrLine>>;
}

/// 用于把裁剪区域的软 Mask 与图像组合的辅助：统计 Mask 覆盖。
pub fn mask_coverage(mask: &GrayU8) -> f32 {
    if mask.data.is_empty() {
        0.0
    } else {
        mask.count_nonzero() as f32 / mask.data.len() as f32
    }
}
