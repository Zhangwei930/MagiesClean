//! 水印候选（规格 §4.2）。

use crate::geometry::{BoundingBox, CoordSpace};
use crate::i18n::Msg;
use crate::tr;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatermarkType {
    Text,
    Logo,
    Transparent,
    Repeated,
    PdfNative,
    /// 相机 / 打卡应用叠加的时间、地点信息块。
    InfoStamp,
    Unknown,
}

impl WatermarkType {
    /// 双语名称（用于拼接到其它消息中）。
    pub fn msg(&self) -> Msg {
        let (zh, en) = match self {
            WatermarkType::Text => ("文字水印", "Text watermark"),
            WatermarkType::Logo => ("Logo 水印", "Logo watermark"),
            WatermarkType::Transparent => ("半透明水印", "Semi-transparent watermark"),
            WatermarkType::Repeated => ("平铺水印", "Tiled watermark"),
            WatermarkType::PdfNative => ("PDF 水印对象", "PDF watermark object"),
            WatermarkType::InfoStamp => ("时间地点水印", "Time & location stamp"),
            WatermarkType::Unknown => ("疑似水印", "Possible watermark"),
        };
        Msg::new(zh, en)
    }

    pub fn label(&self) -> &'static str {
        match self {
            WatermarkType::Text => tr!("文字水印", "Text watermark"),
            WatermarkType::Logo => tr!("Logo 水印", "Logo watermark"),
            WatermarkType::Transparent => tr!("半透明水印", "Semi-transparent watermark"),
            WatermarkType::Repeated => tr!("平铺水印", "Tiled watermark"),
            WatermarkType::PdfNative => tr!("PDF 水印对象", "PDF watermark object"),
            WatermarkType::InfoStamp => tr!("时间地点水印", "Time & location stamp"),
            WatermarkType::Unknown => tr!("疑似水印", "Possible watermark"),
        }
    }
}

/// 产生候选的检测器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectorSource {
    /// 模型检测器（ONNX）。
    AiDetector,
    /// OCR + 水印文字分类器。
    TextOcr,
    /// 经典图像分析：半透明叠加文字/Logo 结构。
    OverlayHeuristic,
    /// 经典图像分析：边缘带内的时间、地点信息块。
    InfoStamp,
    /// 平铺 / 重复图案。
    RepeatedPattern,
    /// 批次持续特征（跨文件一致）。
    BatchPersistence,
    /// PDF 原生对象分析。
    PdfNative,
    /// 用户手动绘制。
    Manual,
}

impl DetectorSource {
    pub fn label(&self) -> &'static str {
        match self {
            DetectorSource::AiDetector => tr!("模型检测", "Model"),
            DetectorSource::TextOcr => tr!("文字识别", "Text recognition"),
            DetectorSource::OverlayHeuristic => tr!("叠加层分析", "Overlay analysis"),
            DetectorSource::InfoStamp => tr!("信息块分析", "Info block analysis"),
            DetectorSource::RepeatedPattern => tr!("重复图案", "Repeated pattern"),
            DetectorSource::BatchPersistence => tr!("批次特征", "Batch pattern"),
            DetectorSource::PdfNative => tr!("PDF 结构", "PDF structure"),
            DetectorSource::Manual => tr!("手动标注", "Manual"),
        }
    }
}

/// 检测证据：用于解释置信度的来源，便于复核与调试。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub source: DetectorSource,
    /// 证据名称，例如 `corner_position`、`low_saturation`、`page_frequency`。
    pub name: String,
    /// 0..1 的证据强度。
    pub value: f32,
}

impl Evidence {
    pub fn new(source: DetectorSource, name: impl Into<String>, value: f32) -> Self {
        Self { source, name: name.into(), value: value.clamp(0.0, 1.0) }
    }
}

/// 自动决策结果：由自动模式 + 置信度决定（规格 §2.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDecision {
    /// 可进入自动去除流程。
    Auto,
    /// 需要人工复核后才能处理。
    Review,
    /// 置信度过低，默认不处理（仍在列表中可见，用户可手动纳入）。
    Ignore,
}

/// 用户对候选的显式操作。优先级高于自动决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UserAction {
    #[default]
    Pending,
    Remove,
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatermarkCandidate {
    pub id: String,
    pub watermark_type: WatermarkType,
    pub confidence: f32,
    /// 位置；图片为归一化坐标，PDF 原生对象为页面坐标（见 `space`）。
    pub bbox: BoundingBox,
    pub space: CoordSpace,
    /// 旋转角度（度）。
    pub rotation: f32,
    pub opacity: Option<f32>,
    /// OCR 文本或 PDF 文字内容。仅用于界面展示，不写入日志。
    pub text: Option<String>,
    pub mask_id: Option<String>,
    pub detector_sources: Vec<DetectorSource>,
    pub evidence: Vec<Evidence>,
    pub repeat_score: f32,
    pub batch_score: f32,
    /// PDF 原生对象引用（见 wm-pdf 的 `PdfObjectRef` 序列化格式）。
    pub pdf_object_ref: Option<String>,
    /// PDF 页码（从 0 开始）；图片为 None。
    pub page: Option<u32>,
    /// 匹配的批次模板 id。
    pub batch_profile_id: Option<String>,
    pub decision: CandidateDecision,
    pub user_action: UserAction,
}

impl WatermarkCandidate {
    pub fn new(id: String, watermark_type: WatermarkType, confidence: f32, bbox: BoundingBox, source: DetectorSource) -> Self {
        Self {
            id,
            watermark_type,
            confidence: confidence.clamp(0.0, 1.0),
            bbox,
            space: CoordSpace::Normalized,
            rotation: 0.0,
            opacity: None,
            text: None,
            mask_id: None,
            detector_sources: vec![source],
            evidence: Vec::new(),
            repeat_score: 0.0,
            batch_score: 0.0,
            pdf_object_ref: None,
            page: None,
            batch_profile_id: None,
            decision: CandidateDecision::Review,
            user_action: UserAction::Pending,
        }
    }

    /// 最终是否要去除：用户操作优先，其次是自动决策。
    pub fn should_remove(&self) -> bool {
        match self.user_action {
            UserAction::Remove => true,
            UserAction::Ignore => false,
            UserAction::Pending => self.decision == CandidateDecision::Auto,
        }
    }

    /// 是否仍在等待人工决定。
    pub fn awaiting_review(&self) -> bool {
        self.user_action == UserAction::Pending && self.decision == CandidateDecision::Review
    }

    pub fn primary_source(&self) -> DetectorSource {
        self.detector_sources.first().copied().unwrap_or(DetectorSource::OverlayHeuristic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_action_overrides_decision() {
        let mut c = WatermarkCandidate::new("a".into(), WatermarkType::Text, 0.99, BoundingBox::default(), DetectorSource::TextOcr);
        c.decision = CandidateDecision::Auto;
        assert!(c.should_remove());
        c.user_action = UserAction::Ignore;
        assert!(!c.should_remove());
        c.decision = CandidateDecision::Ignore;
        c.user_action = UserAction::Remove;
        assert!(c.should_remove());
    }
}
