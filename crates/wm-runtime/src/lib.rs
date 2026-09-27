//! # wm-runtime
//!
//! 组合 Pipeline 与 Adapter，管理运行配置、工作区、后台任务与应用生命周期（规格 §3）。
//! UI（Tauri）与 CLI 都只通过 [`Engine`] 使用核心能力。

pub mod dataset;
pub mod engine;
pub mod image_pipeline;
pub mod model;
pub mod pdf_pipeline;

pub use engine::{Engine, TaskKind, TaskStatus};
pub use model::*;

use std::sync::Arc;
use wm_core::batch::BatchWatermarkProfile;
use wm_core::settings::AppSettings;
use wm_core::traits::{AiInpaintBackend, WatermarkSegmenter};
use wm_detection::DetectorSet;
use wm_storage::Cache;

/// 一次处理所需的运行上下文快照（设置、检测器、分割器、修复后端、缓存、批次模板）。
pub struct PipelineCtx {
    pub settings: AppSettings,
    pub detectors: DetectorSet,
    pub segmenter: Arc<dyn WatermarkSegmenter>,
    pub ai_segmenter: Option<Arc<dyn WatermarkSegmenter>>,
    pub ai_inpaint: Option<Arc<dyn AiInpaintBackend>>,
    /// AI 修复的区域结果缓存（引擎级，跨预览、导出与后台预计算共享）。
    pub ai_patches: Arc<wm_removal::PatchCache>,
    pub cache: Cache,
    pub profiles: Vec<BatchWatermarkProfile>,
    pub memory_budget: u64,
}

/// 初始化结构化日志。Release 默认 INFO；不记录图片、PDF 内容、OCR 全文或密码。
pub fn init_logging(log_dir: Option<&std::path::Path>) {
    use tracing_subscriber::{fmt, EnvFilter};
    // ONNX Runtime 在 info 级别逐条打印图优化细节（每次加载上百行），只保留 warn 以上
    let filter = EnvFilter::try_from_env("MAGIES_LOG")
        .unwrap_or_else(|_| EnvFilter::new(if cfg!(debug_assertions) { "debug,ort=warn" } else { "info,ort=warn" }));
    let builder = fmt().with_env_filter(filter).with_target(false);
    match log_dir.and_then(|d| {
        std::fs::create_dir_all(d).ok()?;
        std::fs::OpenOptions::new().create(true).append(true).open(d.join("magies.log")).ok()
    }) {
        Some(file) => {
            let _ = builder.with_ansi(false).with_writer(std::sync::Mutex::new(file)).try_init();
        }
        None => {
            // 日志写 stderr：CLI 的 stdout 留给结构化输出（--json）
            let _ = builder.with_writer(std::io::stderr).try_init();
        }
    }
}
