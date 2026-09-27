//! 工作区文件条目与发送给前端的视图（typed IPC 契约）。
//!
//! 前端只拿到 ID、路径、元数据和状态；像素数据通过缓存文件路径（asset URL）加载，
//! 不经 IPC 搬运。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use wm_core::decision::{summarize, DecisionSummary, RouteDecision};
use wm_core::job::{FileKind, FileStatus, JobState, ProcessingStage, ReviewStatus};
use wm_core::quality::QualityReport;
use wm_core::traits::MaskHint;
use wm_core::{ErrorView, WatermarkCandidate, WatermarkMask};

/// 图片 / PDF 的元数据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MediaInfo {
    #[serde(rename_all = "camelCase")]
    Image { width: u32, height: u32, format: String, has_alpha: bool },
    #[serde(rename_all = "camelCase")]
    Pdf { page_count: usize, pdf_kind: wm_core::Msg, encrypted: bool, signed: bool, pages: Vec<(f32, f32)> },
}

/// PDF 嵌入图片的处理数据。
#[derive(Debug, Clone)]
pub struct PdfImageWork {
    pub object: (u32, u16),
    pub page: u32,
    pub mask: WatermarkMask,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub id: String,
    pub path: PathBuf,
    pub import_root: Option<PathBuf>,
    pub kind: FileKind,
    pub name: String,
    pub size: u64,
    pub fingerprint: String,
    pub info: Option<MediaInfo>,
    pub state: JobState,
    pub review: ReviewStatus,
    pub stage: ProcessingStage,
    pub progress: f32,
    pub candidates: Vec<WatermarkCandidate>,
    pub hints: HashMap<String, MaskHint>,
    /// 全分辨率（已定向）Mask；PDF 为 None。
    pub mask: Option<WatermarkMask>,
    pub pdf_images: Vec<PdfImageWork>,
    pub thumb: Option<PathBuf>,
    pub preview: Option<PathBuf>,
    pub preview_size: Option<(u32, u32)>,
    pub mask_overlay: Option<PathBuf>,
    pub result_preview: Option<PathBuf>,
    /// 处理结果（图片为无损 PNG，PDF 为输出 PDF），位于缓存目录。
    pub result_file: Option<PathBuf>,
    /// 生成结果时的 Mask 版本；Mask 变化后结果失效。
    pub result_mask_version: Option<u32>,
    pub route: Option<RouteDecision>,
    pub quality: Option<QualityReport>,
    pub output: Option<PathBuf>,
    pub error: Option<ErrorView>,
    pub notes: Vec<wm_core::Msg>,
    /// PDF 打开密码：只在内存中保存。
    pub pdf_password: Option<String>,
    pub needs_password: bool,
    pub signature_confirmed: bool,
    pub group_key: Option<String>,
    pub batch_matched: bool,
    /// Mask 编辑历史（撤销 / 重做）。
    pub mask_undo: Vec<WatermarkMask>,
    pub mask_redo: Vec<WatermarkMask>,
}

pub const MASK_HISTORY_LIMIT: usize = 30;

impl FileEntry {
    pub fn new(id: String, path: PathBuf, import_root: Option<PathBuf>, kind: FileKind, size: u64, fingerprint: String) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        Self {
            id,
            path,
            import_root,
            kind,
            name,
            size,
            fingerprint,
            info: None,
            state: JobState::Queued,
            review: ReviewStatus::None,
            stage: ProcessingStage::Queued,
            progress: 0.0,
            candidates: Vec::new(),
            hints: HashMap::new(),
            mask: None,
            pdf_images: Vec::new(),
            thumb: None,
            preview: None,
            preview_size: None,
            mask_overlay: None,
            result_preview: None,
            result_file: None,
            result_mask_version: None,
            route: None,
            quality: None,
            output: None,
            error: None,
            notes: Vec::new(),
            pdf_password: None,
            needs_password: false,
            signature_confirmed: false,
            group_key: None,
            batch_matched: false,
            mask_undo: Vec::new(),
            mask_redo: Vec::new(),
        }
    }

    pub fn status(&self) -> FileStatus {
        FileStatus::derive(self.state, self.review, self.candidates.len())
    }

    pub fn summary(&self) -> DecisionSummary {
        summarize(&self.candidates)
    }

    /// 根据候选的人工决定状态、质量与签名 / 密码需求重新计算复核状态。
    pub fn refresh_review(&mut self) {
        let pending = self.candidates.iter().any(|c| c.awaiting_review());
        let quality_failed = self.quality.as_ref().is_some_and(|q| !q.passed) && self.result_file.is_some() && self.output.is_none();
        let needs_signature =
            matches!(self.info, Some(MediaInfo::Pdf { signed: true, .. })) && !self.signature_confirmed && self.summary().to_remove > 0;
        self.review = if pending || quality_failed || self.needs_password || needs_signature {
            ReviewStatus::NeedsReview
        } else if self.review == ReviewStatus::NeedsReview {
            ReviewStatus::Approved
        } else {
            self.review
        };
    }

    pub fn mask_version(&self) -> u32 {
        self.mask.as_ref().map(|m| m.version).unwrap_or(0)
    }

    /// 结果是否仍对应当前 Mask / 决策。
    pub fn result_valid(&self) -> bool {
        self.result_file.as_ref().is_some_and(|p| p.exists()) && self.result_mask_version == Some(self.decision_version())
    }

    /// 决策版本：Mask 版本与候选决定共同决定结果是否需要重新生成。
    pub fn decision_version(&self) -> u32 {
        let mut h: u32 = self.mask_version().wrapping_mul(2654435761);
        for c in &self.candidates {
            if c.should_remove() {
                for b in c.id.bytes() {
                    h = h.rotate_left(5) ^ b as u32;
                }
            }
        }
        h
    }

    pub fn view(&self) -> FileView {
        FileView {
            id: self.id.clone(),
            path: self.path.to_string_lossy().to_string(),
            name: self.name.clone(),
            kind: self.kind,
            size: self.size,
            info: self.info.clone(),
            status: self.status(),
            state: self.state,
            review: self.review,
            stage: self.stage,
            progress: self.progress,
            candidates: self.candidates.iter().map(CandidateView::from).collect(),
            summary: self.summary(),
            thumb: self.thumb.as_ref().map(|p| p.to_string_lossy().to_string()),
            preview: self.preview.as_ref().map(|p| p.to_string_lossy().to_string()),
            preview_size: self.preview_size,
            mask_overlay: self.mask_overlay.as_ref().map(|p| p.to_string_lossy().to_string()),
            result_preview: self.result_preview.as_ref().map(|p| p.to_string_lossy().to_string()),
            result_file: self.result_file.as_ref().map(|p| p.to_string_lossy().to_string()),
            result_current: self.result_valid(),
            route: self.route.clone(),
            quality: self.quality.clone(),
            output: self.output.as_ref().map(|p| p.to_string_lossy().to_string()),
            error: self.error.clone(),
            notes: self.notes.clone(),
            needs_password: self.needs_password,
            signature_confirmed: self.signature_confirmed,
            mask_version: self.mask_version(),
            batch_matched: self.batch_matched,
            has_manual_mask: self
                .mask
                .as_ref()
                .is_some_and(|m| m.regions.iter().any(|r| r.candidate_id.as_deref() == Some(wm_image::maskops::MANUAL_REGION))),
            can_undo_mask: !self.mask_undo.is_empty(),
            can_redo_mask: !self.mask_redo.is_empty(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateView {
    pub id: String,
    pub watermark_type: wm_core::WatermarkType,
    pub type_label: String,
    pub confidence: f32,
    pub bbox: wm_core::BoundingBox,
    pub rotation: f32,
    pub opacity: Option<f32>,
    pub text: Option<String>,
    pub sources: Vec<String>,
    pub evidence: Vec<wm_core::Evidence>,
    pub decision: wm_core::CandidateDecision,
    pub user_action: wm_core::UserAction,
    pub will_remove: bool,
    pub awaiting_review: bool,
    pub page: Option<u32>,
    pub batch_profile_id: Option<String>,
}

impl From<&WatermarkCandidate> for CandidateView {
    fn from(c: &WatermarkCandidate) -> Self {
        Self {
            id: c.id.clone(),
            watermark_type: c.watermark_type,
            type_label: c.watermark_type.label().to_string(),
            confidence: c.confidence,
            bbox: c.bbox,
            rotation: c.rotation,
            opacity: c.opacity,
            text: c.text.clone(),
            sources: c.detector_sources.iter().map(|s| s.label().to_string()).collect(),
            evidence: c.evidence.clone(),
            decision: c.decision,
            user_action: c.user_action,
            will_remove: c.should_remove(),
            awaiting_review: c.awaiting_review(),
            page: c.page,
            batch_profile_id: c.batch_profile_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
    pub info: Option<MediaInfo>,
    pub status: FileStatus,
    pub state: JobState,
    pub review: ReviewStatus,
    pub stage: ProcessingStage,
    pub progress: f32,
    pub candidates: Vec<CandidateView>,
    pub summary: DecisionSummary,
    pub thumb: Option<String>,
    pub preview: Option<String>,
    pub preview_size: Option<(u32, u32)>,
    pub mask_overlay: Option<String>,
    pub result_preview: Option<String>,
    pub result_file: Option<String>,
    pub result_current: bool,
    pub route: Option<RouteDecision>,
    pub quality: Option<QualityReport>,
    pub output: Option<String>,
    pub error: Option<ErrorView>,
    pub notes: Vec<wm_core::Msg>,
    pub needs_password: bool,
    pub signature_confirmed: bool,
    pub mask_version: u32,
    pub batch_matched: bool,
    pub has_manual_mask: bool,
    pub can_undo_mask: bool,
    pub can_redo_mask: bool,
}

/// 工作区汇总（底部状态栏、“全部去除”确认）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub total: usize,
    pub with_watermark: usize,
    pub needs_review: usize,
    pub ready_to_process: usize,
    pub completed: usize,
    pub failed: usize,
    /// “全部去除”将处理的候选数。
    pub candidates_to_remove: usize,
    /// 仍待复核、不会被“全部去除”处理的候选数。
    pub candidates_pending: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub added: Vec<FileView>,
    pub duplicates: usize,
    pub unsupported: Vec<String>,
    /// 恢复任务时发现已改变、因此未恢复的文件。
    #[serde(default)]
    pub changed: Vec<String>,
    /// 被系统隐私保护拦截、无法读取的文件（已加入工作区并标记失败原因）。
    #[serde(default)]
    pub blocked: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchSummary {
    pub job_id: String,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub needs_review: usize,
    pub skipped: usize,
    pub cancelled: bool,
    pub output_dir: Option<String>,
    pub elapsed_ms: u64,
}

/// 后端 → 前端事件（规格 §12：事件包含 id、stage、progress 和状态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum EngineEvent {
    #[serde(rename_all = "camelCase")]
    ScanProgress {
        task_id: String,
        done: usize,
        total: usize,
        phase: String,
    },
    #[serde(rename_all = "camelCase")]
    ScanCompleted {
        task_id: String,
        total: usize,
        with_watermark: usize,
        needs_review: usize,
        profiles: usize,
        cancelled: bool,
    },
    #[serde(rename_all = "camelCase")]
    ProcessingProgress {
        task_id: String,
        done: usize,
        total: usize,
        failed: usize,
        paused: bool,
    },
    #[serde(rename_all = "camelCase")]
    ItemCompleted {
        task_id: String,
        file: FileView,
    },
    #[serde(rename_all = "camelCase")]
    ItemFailed {
        task_id: String,
        file: FileView,
    },
    #[serde(rename_all = "camelCase")]
    BatchCompleted {
        summary: BatchSummary,
    },
    #[serde(rename_all = "camelCase")]
    FileUpdated {
        file: FileView,
    },
    ModelLoading,
    ModelReady,
    #[serde(rename_all = "camelCase")]
    Warning {
        message: wm_core::Msg,
    },
}

impl EngineEvent {
    pub fn name(&self) -> &'static str {
        match self {
            EngineEvent::ScanProgress { .. } => "scan-progress",
            EngineEvent::ScanCompleted { .. } => "scan-completed",
            EngineEvent::ProcessingProgress { .. } => "processing-progress",
            EngineEvent::ItemCompleted { .. } => "item-completed",
            EngineEvent::ItemFailed { .. } => "item-failed",
            EngineEvent::BatchCompleted { .. } => "batch-completed",
            EngineEvent::FileUpdated { .. } => "file-updated",
            EngineEvent::ModelLoading => "model-loading",
            EngineEvent::ModelReady => "model-ready",
            EngineEvent::Warning { .. } => "warning",
        }
    }
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: EngineEvent);
}

/// 丢弃所有事件（CLI / 测试）。
pub struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: EngineEvent) {}
}
