//! # wm-detection
//!
//! 检测器与候选融合（规格 §4、§6）。允许并列注册多个检测器：
//! `BatchPersistenceDetector`、`RepeatedPatternDetector`、`OverlayTextDetector`、`InfoStampDetector`、
//! `TextOcrDetector`（需要 OCR 模型）、模型检测器（wm-ai）、PDF 检测器（wm-pdf）。
//! 不把自动识别简化为单一模型。

pub mod batch;
pub mod fusion;
pub mod overlay;
pub mod repeated;
pub mod stamp;
pub mod text;

use std::sync::Arc;
use wm_core::settings::QualityMode;
use wm_core::traits::{Detection, DetectionInput, WatermarkDetector};
use wm_core::{DetectorSource, Result};

/// 已注册检测器集合。
#[derive(Clone, Default)]
pub struct DetectorSet {
    detectors: Vec<Arc<dyn WatermarkDetector>>,
    pub fusion: fusion::FusionConfig,
}

impl DetectorSet {
    /// 内置经典检测器（无需模型）。
    pub fn classic() -> Self {
        let mut s = Self::default();
        s.register(Arc::new(batch::BatchPersistenceDetector::default()));
        s.register(Arc::new(repeated::RepeatedPatternDetector::default()));
        s.register(Arc::new(overlay::OverlayTextDetector::default()));
        s.register(Arc::new(stamp::InfoStampDetector::default()));
        s
    }

    pub fn register(&mut self, d: Arc<dyn WatermarkDetector>) {
        self.detectors.push(d);
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.detectors.iter().map(|d| d.name()).collect()
    }

    /// 运行全部检测器并融合。
    ///
    /// 批次加速路径（规格 §6.1）：批次模板匹配可靠时，快速/均衡模式下跳过单图启发式检测，
    /// 提高速度与一致性；不匹配时回到完整检测。
    pub fn detect(&self, input: &DetectionInput) -> Result<Vec<Detection>> {
        let mut all = Vec::new();
        let mut strong_batch = false;
        for d in self.detectors.iter().filter(|d| d.source() == DetectorSource::BatchPersistence) {
            input.cancel.check()?;
            let found = d.detect(input)?;
            strong_batch |= found.iter().any(|x| x.candidate.batch_score >= 0.5 && x.candidate.confidence >= 0.85);
            all.extend(found);
        }
        for d in self.detectors.iter().filter(|d| d.source() != DetectorSource::BatchPersistence) {
            input.cancel.check()?;
            if strong_batch && input.quality != QualityMode::Best && d.source() == DetectorSource::OverlayHeuristic {
                continue;
            }
            match d.detect(input) {
                Ok(found) => all.extend(found),
                // 单个检测器失败不影响其它检测器；取消需要向上传播
                Err(e) if e.kind == wm_core::ErrorKind::Cancelled => return Err(e),
                Err(e) => tracing::warn!(detector = d.name(), code = e.code(), "detector failed"),
            }
        }
        Ok(fusion::fuse(all, &self.fusion))
    }
}
