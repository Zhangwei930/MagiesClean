//! Engine：组合 Pipeline 与 Adapter，管理运行配置、工作区、后台任务与应用生命周期。

use crate::image_pipeline as ip;
use crate::model::*;
use crate::pdf_pipeline as pp;
use crate::PipelineCtx;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wm_ai::ModelManager;
use wm_batch::{BatchControl, ItemEvent, SchedulerConfig};
use wm_core::batch::{group_and_sample, BatchWatermarkProfile};
use wm_core::job::{FileKind, JobState, ProcessingStage, ReviewStatus};
use wm_core::msg;
use wm_core::settings::{AppSettings, Preset};
use wm_core::traits::WatermarkSegmenter;
use wm_core::{AppError, CandidateDecision, ErrorKind, ErrorView, MaskOp, Result, UserAction, WatermarkMask};
use wm_detection::batch::{BatchLearner, LearnSample};
use wm_detection::DetectorSet;
use wm_storage::{HistoryRecord, JobItemRecord, JobRecord, Storage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Scan,
    Process,
    Similar,
}

#[derive(Clone)]
struct ActiveTask {
    id: String,
    kind: TaskKind,
    control: BatchControl,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    pub id: String,
    pub kind: TaskKind,
    pub paused: bool,
}

#[derive(Default)]
struct Workspace {
    order: Vec<String>,
    files: HashMap<String, FileEntry>,
}

/// AI 修复区域结果缓存上限。
const AI_PATCH_CACHE_BYTES: usize = 256 * 1024 * 1024;

pub struct Engine {
    storage: Arc<Storage>,
    models: Arc<ModelManager>,
    settings: RwLock<AppSettings>,
    ws: RwLock<Workspace>,
    profiles: RwLock<Vec<BatchWatermarkProfile>>,
    sink: Arc<dyn EventSink>,
    active: Mutex<Option<ActiveTask>>,
    classic_segmenter: Arc<dyn WatermarkSegmenter>,
    ai_patches: Arc<wm_removal::PatchCache>,
    /// 后台预计算任务：(文件 id, Mask 版本, 取消令牌)。同一时间只为一个文件预计算。
    warm: Mutex<Option<(String, u32, wm_core::CancellationToken)>>,
    /// 正在进行的前台预览 / 导出数：后台预计算在它们结束前暂停。
    foreground: std::sync::atomic::AtomicUsize,
}

/// 进度事件节流。
struct Throttle {
    last: Mutex<Instant>,
    every: Duration,
}

impl Throttle {
    fn new(ms: u64) -> Self {
        Self { last: Mutex::new(Instant::now() - Duration::from_secs(1)), every: Duration::from_millis(ms) }
    }
    fn ready(&self) -> bool {
        let mut l = self.last.lock();
        if l.elapsed() >= self.every {
            *l = Instant::now();
            true
        } else {
            false
        }
    }
}

impl Engine {
    pub fn new(data_dir: &Path, models_dir: &Path, sink: Arc<dyn EventSink>) -> Result<Arc<Self>> {
        let storage = Arc::new(Storage::open(data_dir)?);
        let settings = storage.load_settings();
        wm_core::i18n::set_language(settings.language);
        sink.emit(EngineEvent::ModelLoading);
        let models = Arc::new(ModelManager::open(models_dir));
        for s in models.statuses() {
            let _ = storage.record_model(&s.id, &s.version, s.label());
        }
        sink.emit(EngineEvent::ModelReady);
        Ok(Arc::new(Self {
            storage,
            models,
            settings: RwLock::new(settings),
            ws: RwLock::new(Workspace::default()),
            profiles: RwLock::new(Vec::new()),
            sink,
            active: Mutex::new(None),
            classic_segmenter: Arc::new(wm_segmentation::ClassicSegmenter),
            ai_patches: Arc::new(wm_removal::PatchCache::new(AI_PATCH_CACHE_BYTES)),
            warm: Mutex::new(None),
            foreground: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    fn ctx(&self) -> PipelineCtx {
        let settings = self.settings.read().clone();
        let mut detectors = DetectorSet::classic();
        if let Some(d) = self.models.detector() {
            detectors.register(d);
        }
        if let Some(ocr) = self.models.ocr() {
            detectors.register(Arc::new(wm_detection::text::TextOcrDetector { engine: ocr, min_score: 0.5 }));
        }
        let budget = settings.performance.memory_budget_mb.map(|m| m * 1024 * 1024).unwrap_or(3 * 1024 * 1024 * 1024);
        PipelineCtx {
            detectors,
            segmenter: self.classic_segmenter.clone(),
            ai_segmenter: self.models.segmenter(),
            ai_inpaint: self.models.inpaint_backend(),
            ai_patches: self.ai_patches.clone(),
            cache: self.storage.cache.clone(),
            profiles: self.profiles.read().clone(),
            memory_budget: budget,
            settings,
        }
    }

    fn scheduler(&self) -> SchedulerConfig {
        let s = self.settings.read();
        SchedulerConfig::auto(s.performance.cpu_workers, s.performance.memory_budget_mb)
    }

    fn emit_file(&self, id: &str) {
        if let Some(v) = self.ws.read().files.get(id).map(|f| f.view()) {
            self.sink.emit(EngineEvent::FileUpdated { file: v });
        }
    }

    fn with_file<R>(&self, id: &str, f: impl FnOnce(&mut FileEntry) -> R) -> Result<R> {
        let mut ws = self.ws.write();
        let e = ws.files.get_mut(id).ok_or_else(|| AppError::internal(msg!("文件不在工作区中", "The file is not in the workspace")))?;
        Ok(f(e))
    }

    fn snapshot(&self, id: &str) -> Result<FileEntry> {
        self.ws
            .read()
            .files
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::internal(msg!("文件不在工作区中", "The file is not in the workspace")))
    }

    // ───────────────────────── 设置 / 预设 / 模型 ─────────────────────────

    pub fn settings(&self) -> AppSettings {
        self.settings.read().clone()
    }

    /// 更新设置。自动模式或阈值变化时重新计算所有候选的决策（用户显式操作保持不变）。
    pub fn update_settings(&self, s: AppSettings) -> Result<AppSettings> {
        let s = s.sanitized();
        self.storage.save_settings(&s)?;
        wm_core::i18n::set_language(s.language);
        let changed = {
            let old = self.settings.read();
            old.auto_mode != s.auto_mode || old.thresholds != s.thresholds
        };
        *self.settings.write() = s.clone();
        if changed {
            let t = s.thresholds();
            let ids: Vec<String> = {
                let mut ws = self.ws.write();
                for f in ws.files.values_mut() {
                    for c in &mut f.candidates {
                        let had_mask = c.mask_id.is_some() || c.pdf_object_ref.is_some();
                        c.decision = wm_core::decision::decide(c.confidence, &t);
                        if !had_mask && c.decision == CandidateDecision::Auto {
                            c.decision = CandidateDecision::Review;
                        }
                    }
                    f.refresh_review();
                }
                ws.order.clone()
            };
            for id in ids {
                self.emit_file(&id);
            }
        }
        Ok(s)
    }

    pub fn presets(&self) -> Result<Vec<Preset>> {
        self.storage.presets()
    }
    pub fn save_preset(&self, p: Preset) -> Result<Vec<Preset>> {
        self.storage.save_preset(&p)?;
        self.storage.presets()
    }
    pub fn delete_preset(&self, id: &str) -> Result<Vec<Preset>> {
        self.storage.delete_preset(id)?;
        self.storage.presets()
    }
    /// 应用预设：写入检测模式、门槛、修复质量与输出格式。
    pub fn apply_preset(&self, id: &str) -> Result<AppSettings> {
        let p = self
            .storage
            .presets()?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::internal(msg!("预设不存在", "Preset not found")))?;
        let mut s = self.settings();
        s.auto_mode = p.detection_mode;
        match p.detection_mode {
            wm_core::settings::AutoMode::Conservative => s.thresholds.conservative.auto = p.confidence_threshold,
            wm_core::settings::AutoMode::Standard => s.thresholds.standard.auto = p.confidence_threshold,
            wm_core::settings::AutoMode::Aggressive => s.thresholds.aggressive.auto = p.confidence_threshold,
        }
        s.quality_mode = p.removal_quality;
        s.output.format = p.output_format;
        self.update_settings(s)
    }

    pub fn model_statuses(&self) -> Vec<wm_ai::ModelStatus> {
        self.models.statuses()
    }
    pub fn reload_models(&self) -> Vec<wm_ai::ModelStatus> {
        self.sink.emit(EngineEvent::ModelLoading);
        self.models.refresh();
        self.sink.emit(EngineEvent::ModelReady);
        self.models.statuses()
    }
    pub fn models_dir(&self) -> PathBuf {
        self.models.dir().to_path_buf()
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.storage.cache.root().to_path_buf()
    }
    pub fn cache_size(&self) -> u64 {
        self.storage.cache.size_bytes()
    }
    /// 清理缓存。工作区中仍在使用的文件的缓存受保护。
    pub fn clear_cache(&self) -> Result<u64> {
        let protected: Vec<String> = self.ws.read().order.clone();
        self.storage.cache.clear(&protected)
    }

    // ───────────────────────── 工作区 ─────────────────────────

    /// 导入文件或文件夹（递归）。只接受支持的格式；重复路径忽略。
    pub fn import(&self, paths: Vec<PathBuf>) -> Result<ImportResult> {
        let mut found: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
        let mut unsupported = Vec::new();
        for p in paths {
            if p.is_dir() {
                for e in walkdir::WalkDir::new(&p).follow_links(false).into_iter().filter_map(|e| e.ok()) {
                    let fp = e.path();
                    if !e.file_type().is_file() || e.file_name().to_string_lossy().starts_with('.') {
                        continue;
                    }
                    if wm_common::is_supported_image(fp) || wm_common::is_pdf(fp) {
                        found.push((fp.to_path_buf(), Some(p.clone())));
                    } else if is_media_like(fp) {
                        unsupported.push(fp.to_string_lossy().to_string());
                    }
                }
            } else if p.is_file() {
                if wm_common::is_supported_image(&p) || wm_common::is_pdf(&p) {
                    found.push((p, None));
                } else {
                    unsupported.push(p.to_string_lossy().to_string());
                }
            }
        }
        found.sort_by(|a, b| natural_cmp(&a.0.to_string_lossy(), &b.0.to_string_lossy()));
        let mut added = Vec::new();
        let mut blocked = Vec::new();
        let mut dups = 0;
        let mut ws = self.ws.write();
        let existing: std::collections::HashSet<PathBuf> = ws.files.values().map(|f| f.path.clone()).collect();
        for (path, root) in found {
            if existing.contains(&path) {
                dups += 1;
                continue;
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let kind = if wm_common::is_pdf(&path) { FileKind::Pdf } else { FileKind::Image };
            // 导入时就确认能否读取：macOS 会拦截其它应用受保护目录（例如微信缓存）中的文件
            let readable = check_readable(&path);
            let fp = if readable.is_ok() { wm_common::fast_fingerprint(&path).map(|f| f.0).unwrap_or_default() } else { String::new() };
            let mut e = FileEntry::new(wm_common::new_id(), path, root, kind, size, fp);
            if let Err(err) = readable {
                tracing::warn!(file = %e.id, code = err.code(), detail = ?err.detail, "file not readable at import");
                e.state = JobState::Failed;
                e.error = Some(ErrorView::from(&err));
                blocked.push(e.path.to_string_lossy().to_string());
            }
            added.push(e.view());
            ws.order.push(e.id.clone());
            ws.files.insert(e.id.clone(), e);
        }
        Ok(ImportResult { added, duplicates: dups, unsupported, changed: Vec::new(), blocked })
    }

    pub fn files(&self) -> Vec<FileView> {
        let ws = self.ws.read();
        ws.order.iter().filter_map(|id| ws.files.get(id)).map(|f| f.view()).collect()
    }

    pub fn file(&self, id: &str) -> Result<FileView> {
        Ok(self.snapshot(id)?.view())
    }

    /// 原图路径（供 UI 允许 asset 访问，例如 PDF 预览）。
    pub fn file_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.snapshot(id)?.path)
    }

    /// 将被去除的 Mask（benchmark / 测试用）。
    pub fn debug_mask(&self, id: &str) -> Option<WatermarkMask> {
        self.snapshot(id).ok().and_then(|f| ip::removal_mask(&f).or(f.mask))
    }

    pub fn remove_files(&self, ids: &[String]) -> Result<()> {
        self.ensure_idle()?;
        let mut ws = self.ws.write();
        for id in ids {
            ws.files.remove(id);
        }
        ws.order.retain(|i| !ids.contains(i));
        Ok(())
    }

    pub fn clear_workspace(&self) -> Result<()> {
        self.ensure_idle()?;
        *self.ws.write() = Workspace::default();
        self.profiles.write().clear();
        Ok(())
    }

    pub fn summary(&self) -> WorkspaceSummary {
        let ws = self.ws.read();
        let mut s = WorkspaceSummary { total: ws.files.len(), ..Default::default() };
        for f in ws.files.values() {
            let d = f.summary();
            if !f.candidates.is_empty() {
                s.with_watermark += 1;
            }
            match f.status() {
                wm_core::job::FileStatus::NeedsReview => s.needs_review += 1,
                wm_core::job::FileStatus::Completed => s.completed += 1,
                wm_core::job::FileStatus::Failed => s.failed += 1,
                _ => {}
            }
            if is_processable(f) {
                s.ready_to_process += 1;
                s.candidates_to_remove += d.to_remove;
            }
            s.candidates_pending += d.needs_review;
        }
        s
    }

    fn ensure_idle(&self) -> Result<()> {
        if self.active.lock().is_some() {
            Err(AppError::new(
                ErrorKind::Internal,
                msg!("有任务正在运行，请等待完成或取消后再操作", "A task is running. Wait for it to finish or cancel it first"),
            ))
        } else {
            Ok(())
        }
    }

    pub fn active_task(&self) -> Option<TaskStatus> {
        self.active.lock().as_ref().map(|t| TaskStatus { id: t.id.clone(), kind: t.kind, paused: t.control.pause.is_paused() })
    }

    fn begin(&self, kind: TaskKind) -> Result<ActiveTask> {
        let mut a = self.active.lock();
        if a.is_some() {
            return Err(AppError::new(
                ErrorKind::Internal,
                msg!("有任务正在运行，请等待完成或取消后再操作", "A task is running. Wait for it to finish or cancel it first"),
            ));
        }
        let t = ActiveTask { id: wm_common::new_id(), kind, control: BatchControl::new() };
        *a = Some(t.clone());
        Ok(t)
    }

    fn end(&self, id: &str) {
        let mut a = self.active.lock();
        if a.as_ref().is_some_and(|t| t.id == id) {
            *a = None;
        }
    }

    pub fn pause(&self) -> Result<()> {
        let a = self.active.lock();
        let t = a.as_ref().ok_or_else(|| AppError::internal(msg!("没有正在运行的任务", "No task is running")))?;
        t.control.pause.pause();
        Ok(())
    }
    pub fn resume(&self) -> Result<()> {
        let a = self.active.lock();
        let t = a.as_ref().ok_or_else(|| AppError::internal(msg!("没有正在运行的任务", "No task is running")))?;
        t.control.pause.resume();
        Ok(())
    }
    pub fn cancel(&self) -> Result<()> {
        let a = self.active.lock();
        let t = a.as_ref().ok_or_else(|| AppError::internal(msg!("没有正在运行的任务", "No task is running")))?;
        t.control.pause.resume();
        t.control.cancel.cancel();
        Ok(())
    }

    // ───────────────────────── 扫描 ─────────────────────────

    /// 后台扫描。返回任务 ID；进度通过事件推送。
    pub fn scan(self: &Arc<Self>, ids: Option<Vec<String>>) -> Result<String> {
        let task = self.begin(TaskKind::Scan)?;
        let me = self.clone();
        let tid = task.id.clone();
        std::thread::Builder::new()
            .name("magies-scan".into())
            .spawn(move || {
                let _ = me.run_scan(&task, ids);
                me.end(&task.id);
            })
            .map_err(|e| AppError::internal(msg!("无法启动扫描任务", "Could not start scanning")).with_detail(e))?;
        Ok(tid)
    }

    /// 同步扫描（CLI / 测试）。
    pub fn scan_blocking(&self, ids: Option<Vec<String>>) -> Result<()> {
        let task = self.begin(TaskKind::Scan)?;
        let r = self.run_scan(&task, ids);
        self.end(&task.id);
        r
    }

    fn run_scan(&self, task: &ActiveTask, ids: Option<Vec<String>>) -> Result<()> {
        let targets: Vec<String> = {
            let ws = self.ws.read();
            let all = ids.unwrap_or_else(|| ws.order.clone());
            all.into_iter().filter(|id| ws.files.get(id).is_some_and(|f| f.state != JobState::Processing)).collect()
        };
        let total = targets.len();
        for id in &targets {
            let _ = self.with_file(id, |f| {
                f.state = JobState::Queued;
                f.stage = ProcessingStage::Queued;
                f.error = None;
            });
        }
        let settings = self.settings();

        // 1) 批次学习
        if settings.batch.enabled {
            self.sink.emit(EngineEvent::ScanProgress { task_id: task.id.clone(), done: 0, total, phase: "learn".into() });
            match self.learn_profiles(&targets, &settings, &task.control) {
                Ok(p) => {
                    let _ = self.storage.save_profiles(&p);
                    *self.profiles.write() = p;
                }
                Err(e) if e.kind == ErrorKind::Cancelled => {}
                Err(e) => tracing::warn!(code = e.code(), "batch learning failed"),
            }
        }

        // 2) 逐文件检测
        let ctx = self.ctx();
        let throttle = Throttle::new(120);
        let done = std::sync::atomic::AtomicUsize::new(0);
        let estimates: HashMap<String, u64> = targets.iter().map(|id| (id.clone(), self.estimate(id))).collect();
        let results = wm_batch::run(
            &targets,
            &self.scheduler(),
            &task.control,
            |id| estimates.get(id).copied().unwrap_or(64 << 20),
            |_, id, control| {
                control.checkpoint()?;
                let entry = self.with_file(id, |f| {
                    f.state = JobState::Scanning;
                    f.stage = ProcessingStage::Detect;
                    f.clone()
                })?;
                self.emit_file(id);
                self.scan_one(&ctx, &entry, control)
            },
            |ev| {
                if let ItemEvent::Finished { index, result } = ev {
                    let id = &targets[index];
                    let n = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if let Err(e) = result {
                        if e.kind != ErrorKind::Cancelled {
                            tracing::warn!(file = %id, code = e.code(), detail = ?e.detail, "scan failed");
                        }
                        let _ = self.with_file(id, |f| {
                            if e.kind == ErrorKind::Cancelled {
                                f.state = JobState::Queued;
                            } else {
                                f.state = JobState::Failed;
                                f.error = Some(ErrorView::from(&e));
                            }
                            f.stage = ProcessingStage::Queued;
                        });
                    }
                    self.emit_file(id);
                    if throttle.ready() || n == total {
                        self.sink.emit(EngineEvent::ScanProgress { task_id: task.id.clone(), done: n, total, phase: "detect".into() });
                    }
                }
            },
        );
        let cancelled = task.control.cancel.is_cancelled();
        let _ = results;
        let ws = self.ws.read();
        let with_wm = targets.iter().filter(|id| ws.files.get(*id).is_some_and(|f| !f.candidates.is_empty())).count();
        let review = targets.iter().filter(|id| ws.files.get(*id).is_some_and(|f| f.review == ReviewStatus::NeedsReview)).count();
        drop(ws);
        self.sink.emit(EngineEvent::ScanCompleted {
            task_id: task.id.clone(),
            total,
            with_watermark: with_wm,
            needs_review: review,
            profiles: self.profiles.read().len(),
            cancelled,
        });
        Ok(())
    }

    fn estimate(&self, id: &str) -> u64 {
        let Ok(e) = self.snapshot(id) else { return 64 << 20 };
        match e.kind {
            FileKind::Image => wm_image::probe(&e.path).map(|i| i.decoded_bytes() * 3).unwrap_or(64 << 20),
            FileKind::Pdf => (e.size * 4).max(64 << 20),
        }
    }

    /// 批次学习：分组、抽样、学习模板。
    fn learn_profiles(&self, targets: &[String], settings: &AppSettings, control: &BatchControl) -> Result<Vec<BatchWatermarkProfile>> {
        use rayon::prelude::*;
        let paths: Vec<(usize, PathBuf)> = {
            let ws = self.ws.read();
            targets
                .iter()
                .enumerate()
                .filter_map(|(i, id)| ws.files.get(id).filter(|f| f.kind == FileKind::Image).map(|f| (i, f.path.clone())))
                .collect()
        };
        if paths.len() < 3 {
            return Ok(Vec::new());
        }
        let dims: Vec<(usize, u32, u32)> =
            paths.par_iter().filter_map(|(i, p)| wm_image::probe(p).ok().map(|inf| (*i, inf.width, inf.height))).collect();
        let groups = group_and_sample(&dims, settings.batch.sample_override);
        let learner = BatchLearner::default();
        let mut out = Vec::new();
        let path_of: HashMap<usize, PathBuf> = paths.into_iter().collect();
        for g in groups.iter().filter(|g| g.members.len() >= 3) {
            control.checkpoint()?;
            // 解码样本到 1024 缩略图（有界：最多 30 张）
            let samples: Vec<(wm_core::ImageBuffer, u32, u32)> = g
                .samples
                .par_iter()
                .filter_map(|i| {
                    let d = wm_image::decode(path_of.get(i)?, None).ok()?;
                    let (w, h) = (d.buffer.width, d.buffer.height);
                    let (t, _) = wm_image::ops::thumbnail(&d.buffer, 1024);
                    Some((t, w, h))
                })
                .collect();
            let ls: Vec<LearnSample> =
                samples.iter().map(|(t, w, h)| LearnSample { image: t, original_width: *w, original_height: *h }).collect();
            out.extend(learner.learn(&g.key, &ls, &control.cancel)?);
        }
        Ok(out)
    }

    fn scan_one(&self, ctx: &PipelineCtx, entry: &FileEntry, control: &BatchControl) -> Result<()> {
        check_readable(&entry.path)?;
        match entry.kind {
            FileKind::Image => {
                let out = ip::scan_image(ctx, entry, &control.cancel)?;
                self.with_file(&entry.id, |f| {
                    f.info = Some(out.info);
                    f.candidates = out.candidates;
                    f.hints = out.hints;
                    f.mask = Some(out.mask);
                    f.thumb = Some(out.thumb);
                    f.preview = Some(out.preview);
                    f.preview_size = Some(out.preview_size);
                    f.mask_overlay = out.overlay;
                    f.batch_matched = out.batch_matched;
                    f.notes = out.notes;
                    reset_results(f);
                    f.state = JobState::Ready;
                    f.stage = ProcessingStage::Queued;
                    f.review = ReviewStatus::None;
                    f.refresh_review();
                })?;
            }
            FileKind::Pdf => match pp::scan_pdf(ctx, entry, &control.cancel) {
                Ok(out) => {
                    self.with_file(&entry.id, |f| {
                        f.info = Some(out.info);
                        f.candidates = out.candidates;
                        f.pdf_images = out.images;
                        f.notes = out.notes;
                        f.needs_password = false;
                        reset_results(f);
                        f.state = JobState::Ready;
                        f.stage = ProcessingStage::Queued;
                        f.review = ReviewStatus::None;
                        f.refresh_review();
                    })?;
                }
                Err(e) if pp::is_password_error(&e) => {
                    self.with_file(&entry.id, |f| {
                        f.needs_password = true;
                        f.state = JobState::Ready;
                        f.error = Some(ErrorView::from(&e));
                        f.notes = vec![if f.pdf_password.is_some() {
                            msg!("密码不正确，请重新输入", "Incorrect password. Please try again")
                        } else {
                            msg!("该 PDF 需要打开密码", "This PDF requires a password")
                        }];
                        f.refresh_review();
                    })?;
                }
                Err(e) => return Err(e),
            },
        }
        Ok(())
    }

    // ───────────────────────── 复核 / 编辑 ─────────────────────────

    pub fn set_candidate_action(&self, file_id: &str, candidate_id: &str, action: UserAction) -> Result<FileView> {
        self.ensure_not_processing(file_id)?;
        self.with_file(file_id, |f| {
            if let Some(c) = f.candidates.iter_mut().find(|c| c.id == candidate_id) {
                c.user_action = action;
            }
            f.refresh_review();
        })?;
        self.emit_file(file_id);
        self.file(file_id)
    }

    /// 对一个文件的全部待复核候选执行相同操作。
    pub fn resolve_pending(&self, file_id: &str, action: UserAction) -> Result<FileView> {
        self.ensure_not_processing(file_id)?;
        self.with_file(file_id, |f| {
            for c in f.candidates.iter_mut().filter(|c| c.awaiting_review()) {
                c.user_action = action;
            }
            f.refresh_review();
        })?;
        self.emit_file(file_id);
        self.file(file_id)
    }

    fn ensure_not_processing(&self, file_id: &str) -> Result<()> {
        if self.snapshot(file_id)?.state == JobState::Processing {
            return Err(AppError::internal(msg!("文件正在处理中", "The file is being processed")));
        }
        Ok(())
    }

    /// MaskEditor 的编辑操作（归一化坐标矢量）。修改 Mask 后需要重新生成结果，不直接改写原图。
    pub fn update_mask(&self, file_id: &str, ops: Vec<MaskOp>) -> Result<FileView> {
        self.ensure_not_processing(file_id)?;
        let ctx_cache = self.storage.cache.clone();
        let (mask, preview_size) = self.with_file(file_id, |f| -> Result<(WatermarkMask, (u32, u32))> {
            if f.kind != FileKind::Image {
                return Err(AppError::unsupported(msg!("PDF 暂不支持手动绘制 Mask", "Manual mask editing is not available for PDFs yet")));
            }
            let (w, h) = match &f.info {
                Some(MediaInfo::Image { width, height, .. }) => (*width, *height),
                _ => return Err(AppError::internal(msg!("请先扫描该文件", "Scan this file first"))),
            };
            let fid = f.id.clone();
            let before = f.mask.clone().unwrap_or_else(|| WatermarkMask::new(wm_common::new_id(), fid.clone(), w, h));
            let m = f.mask.get_or_insert_with(|| WatermarkMask::new(wm_common::new_id(), fid, w, h));
            wm_image::maskops::apply_mask_ops(m, &ops);
            let mask = m.clone();
            f.mask_undo.push(before);
            if f.mask_undo.len() > MASK_HISTORY_LIMIT {
                f.mask_undo.remove(0);
            }
            f.mask_redo.clear();
            // 被橡皮擦完全擦除的候选不再参与去除
            let covered: std::collections::HashSet<String> =
                mask.regions.iter().filter(|r| r.coverage() > 0).filter_map(|r| r.candidate_id.clone()).collect();
            for c in &mut f.candidates {
                if c.mask_id.is_some() && !covered.contains(&c.id) && c.user_action != UserAction::Ignore {
                    c.user_action = UserAction::Ignore;
                }
            }
            f.refresh_review();
            Ok((mask, f.preview_size.unwrap_or((w, h))))
        })??;
        self.refresh_overlay(&ctx_cache, file_id, &mask, preview_size)
    }

    fn refresh_overlay(
        &self,
        cache: &wm_storage::Cache,
        file_id: &str,
        mask: &WatermarkMask,
        preview_size: (u32, u32),
    ) -> Result<FileView> {
        let ov = if mask.is_empty() {
            None
        } else {
            let o = wm_image::maskops::mask_overlay_rgba(mask, preview_size.0, preview_size.1, ip::MASK_COLOR);
            let p = cache.path(wm_storage::CacheKind::Masks, &format!("{file_id}-v{}", mask.version), "png");
            wm_image::write_png(&o, &p)?;
            Some(p)
        };
        self.with_file(file_id, |f| f.mask_overlay = ov)?;
        self.emit_file(file_id);
        self.file(file_id)
    }

    /// 撤销 / 重做 Mask 编辑。版本号单调递增，保证结果缓存失效。
    pub fn undo_mask(&self, file_id: &str, redo: bool) -> Result<FileView> {
        self.ensure_not_processing(file_id)?;
        let (mask, size) = self
            .with_file(file_id, |f| -> Option<(WatermarkMask, (u32, u32))> {
                let cur = f.mask.clone()?;
                let mut prev = if redo { f.mask_redo.pop()? } else { f.mask_undo.pop()? };
                if redo {
                    f.mask_undo.push(cur.clone());
                } else {
                    f.mask_redo.push(cur.clone());
                }
                prev.version = cur.version + 1;
                // 恢复到该版本时，候选的“忽略”状态按区域是否存在重新计算
                let covered: std::collections::HashSet<String> =
                    prev.regions.iter().filter(|r| r.coverage() > 0).filter_map(|r| r.candidate_id.clone()).collect();
                for c in &mut f.candidates {
                    if c.mask_id.is_some() {
                        if covered.contains(&c.id) && c.user_action == UserAction::Ignore {
                            c.user_action = UserAction::Pending;
                        } else if !covered.contains(&c.id) {
                            c.user_action = UserAction::Ignore;
                        }
                    }
                }
                f.mask = Some(prev.clone());
                f.refresh_review();
                Some((prev, f.preview_size.unwrap_or((cur.width, cur.height))))
            })?
            .ok_or_else(|| {
                AppError::internal(if redo {
                    msg!("没有可重做的编辑", "Nothing to redo")
                } else {
                    msg!("没有可撤销的编辑", "Nothing to undo")
                })
            })?;
        let cache = self.storage.cache.clone();
        self.refresh_overlay(&cache, file_id, &mask, size)
    }

    pub fn set_pdf_password(self: &Arc<Self>, file_id: &str, password: String) -> Result<String> {
        self.with_file(file_id, |f| f.pdf_password = Some(password))?;
        self.scan(Some(vec![file_id.to_string()]))
    }

    pub fn confirm_signature(&self, file_id: &str) -> Result<FileView> {
        self.with_file(file_id, |f| {
            f.signature_confirmed = true;
            f.refresh_review();
        })?;
        self.emit_file(file_id);
        self.file(file_id)
    }

    // ───────────────────────── 预览 / 处理 / 导出 ─────────────────────────

    /// 标记一次前台预览 / 导出；返回值释放时结束。后台预计算在前台任务期间暂停。
    fn foreground(&self) -> impl Drop + '_ {
        struct Guard<'a>(&'a std::sync::atomic::AtomicUsize);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        self.foreground.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Guard(&self.foreground)
    }

    /// 后台为图片预计算 AI 修复结果：未忽略的候选按置信度从高到低逐个计算并缓存。
    /// 用户确认去除时直接取用缓存，预览几乎立即完成。再次调用（例如切换文件）会取消上一次。
    pub fn warm_preview(self: &Arc<Self>, file_id: &str) -> Result<()> {
        let entry = self.snapshot(file_id)?;
        let version = entry.mask.as_ref().map_or(0, |m| m.version);
        let token = wm_core::CancellationToken::new();
        {
            let mut w = self.warm.lock();
            // 同一文件、Mask 未变：已有任务在算（或已算完）同样的区域，不打断
            if w.as_ref().is_some_and(|(id, v, t)| id == file_id && *v == version && !t.is_cancelled()) {
                return Ok(());
            }
            if let Some((_, _, old)) = w.replace((file_id.to_string(), version, token.clone())) {
                old.cancel();
            }
        }
        let ctx = self.ctx();
        let (Some(backend), Some(mask)) = (ctx.ai_inpaint.clone(), entry.mask.clone()) else { return Ok(()) };
        if entry.kind != FileKind::Image || ctx.settings.quality_mode == wm_core::settings::QualityMode::Fast {
            return Ok(());
        }
        let mut cands: Vec<_> = entry
            .candidates
            .iter()
            .filter(|c| {
                c.user_action != UserAction::Ignore && (c.decision != CandidateDecision::Ignore || c.user_action == UserAction::Remove)
            })
            .filter_map(|c| {
                mask.regions.iter().find(|r| r.candidate_id.as_deref() == Some(c.id.as_str())).map(|r| (c.confidence, r.clone()))
            })
            .collect();
        if cands.is_empty() {
            return Ok(());
        }
        cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let me = self.clone();
        let path = entry.path.clone();
        std::thread::Builder::new()
            .name("magies-warm".into())
            .spawn(move || {
                let Ok(decoded) = wm_image::decode(&path, Some(ctx.memory_budget)) else { return };
                if (decoded.buffer.width, decoded.buffer.height) != (mask.width, mask.height) {
                    return;
                }
                let unknown = Arc::new(mask.clone());
                let remover = wm_removal::AiInpaintRemover::new(backend, Some(ctx.ai_patches.clone()));
                let started = Instant::now();
                for (_, region) in cands {
                    // 前台预览 / 导出进行中：让出模型
                    while me.foreground.load(std::sync::atomic::Ordering::SeqCst) > 0 && !token.is_cancelled() {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    if token.is_cancelled() {
                        return;
                    }
                    let single = WatermarkMask { regions: vec![region], ..mask.clone() };
                    let opts =
                        wm_core::traits::RemovalOptions { cancel: token.clone(), unknown: Some(unknown.clone()), ..Default::default() };
                    if let Err(e) = remover.precompute(&decoded.buffer, &single, &opts) {
                        if e.kind != ErrorKind::Cancelled {
                            tracing::warn!(code = e.code(), "background inpaint precompute failed");
                        }
                        return;
                    }
                }
                tracing::debug!(ms = started.elapsed().as_millis() as u64, "background inpaint precompute done");
            })
            .map_err(|e| AppError::internal(msg!("无法启动后台预计算", "Could not start background precompute")).with_detail(e))?;
        Ok(())
    }

    /// 生成处理结果预览（不导出）。
    pub fn generate_preview(&self, file_id: &str) -> Result<FileView> {
        let entry = self.snapshot(file_id)?;
        if entry.result_valid() {
            return Ok(entry.view());
        }
        let _fg = self.foreground();
        let ctx = self.ctx();
        let cancel = wm_core::CancellationToken::new();
        let version = entry.decision_version();
        match entry.kind {
            FileKind::Image => {
                let r = ip::process_image(&ctx, &entry, &cancel)?;
                let (full, prev) = ip::cache_result(&ctx, &entry.id, version, &r.image)?;
                self.with_file(file_id, |f| {
                    f.result_file = Some(full);
                    f.result_preview = Some(prev);
                    f.result_mask_version = Some(version);
                    f.route = Some(r.route);
                    f.quality = Some(r.quality);
                })?;
            }
            FileKind::Pdf => {
                let r = pp::process_pdf(&ctx, &entry, &cancel)?;
                let p = pp::cache_pdf(&ctx, &entry.id, version, &r.bytes)?;
                self.with_file(file_id, |f| {
                    f.result_file = Some(p);
                    f.result_preview = None;
                    f.result_mask_version = Some(version);
                    f.route = Some(r.route);
                    f.quality = Some(r.quality);
                    for n in r.notes {
                        if !f.notes.contains(&n) {
                            f.notes.push(n);
                        }
                    }
                })?;
            }
        }
        self.emit_file(file_id);
        self.file(file_id)
    }

    /// 处理单个文件：生成结果并在质量通过（或 `force` 用户已确认）时导出。
    fn process_one(&self, ctx: &PipelineCtx, id: &str, force: bool, cancel: &wm_core::CancellationToken) -> Result<()> {
        let _fg = self.foreground();
        let entry = self.with_file(id, |f| {
            f.state = JobState::Processing;
            f.stage = ProcessingStage::Remove;
            f.progress = 0.3;
            f.error = None;
            f.clone()
        })?;
        self.emit_file(id);
        let version = entry.decision_version();
        let (quality_ok, output) = match entry.kind {
            FileKind::Image => {
                // 已生成的预览仍有效时复用，不重复推理
                let cached = ip::load_cached_result(ctx, &entry)?;
                let reused = cached.is_some();
                let r = match cached {
                    Some(r) => r,
                    None => ip::process_image(ctx, &entry, cancel)?,
                };
                cancel.check()?;
                self.with_file(id, |f| f.stage = ProcessingStage::Encode)?;
                let (full, prev) = match (reused, &entry.result_file, &entry.result_preview) {
                    (true, Some(full), Some(prev)) if prev.exists() => (full.clone(), prev.clone()),
                    _ => ip::cache_result(ctx, &entry.id, version, &r.image)?,
                };
                let ok = r.quality.passed || force;
                let out = if ok { ip::export_image(ctx, &entry, &r.image, &r.metadata, r.format)? } else { None };
                self.with_file(id, |f| {
                    f.result_file = Some(full);
                    f.result_preview = Some(prev);
                    f.result_mask_version = Some(version);
                    f.route = Some(r.route);
                    f.quality = Some(r.quality);
                })?;
                (ok, out)
            }
            FileKind::Pdf => {
                let r = pp::process_pdf(ctx, &entry, cancel)?;
                cancel.check()?;
                let p = pp::cache_pdf(ctx, &entry.id, version, &r.bytes)?;
                let ok = r.quality.passed || force;
                let out = if ok { pp::export_pdf(ctx, &entry, &r.bytes)? } else { None };
                self.with_file(id, |f| {
                    f.result_file = Some(p);
                    f.result_mask_version = Some(version);
                    f.route = Some(r.route);
                    f.quality = Some(r.quality);
                    for n in r.notes {
                        if !f.notes.contains(&n) {
                            f.notes.push(n);
                        }
                    }
                })?;
                (ok, out)
            }
        };
        self.with_file(id, |f| {
            f.stage = ProcessingStage::Done;
            f.progress = 1.0;
            if quality_ok {
                f.state = JobState::Completed;
                f.output = output.clone();
                if output.is_none() {
                    f.notes.push(msg!(
                        "导出目标已存在，已按冲突策略跳过",
                        "The output already existed and was skipped per the conflict setting"
                    ));
                }
                f.review = if f.review == ReviewStatus::NeedsReview { ReviewStatus::Approved } else { f.review };
            } else {
                // 质量检查未通过：保留原图、Mask 与结果以便比较，不导出
                f.state = JobState::Ready;
                f.review = ReviewStatus::NeedsReview;
            }
        })?;
        Ok(())
    }

    /// 用户确认后导出单个文件（包括质量检查未通过但用户接受的结果）。
    pub fn export_file(&self, file_id: &str) -> Result<FileView> {
        let ctx = self.ctx();
        let cancel = wm_core::CancellationToken::new();
        let entry = self.snapshot(file_id)?;
        if entry.summary().needs_review > 0 {
            return Err(AppError::needs_confirmation(msg!(
                "仍有待复核的候选，请先决定去除或忽略",
                "Some detections still need review. Choose Remove or Ignore first"
            )));
        }
        let r = self.process_one(&ctx, file_id, true, &cancel);
        if let Err(e) = &r {
            let _ = self.with_file(file_id, |f| {
                f.state = JobState::Failed;
                f.error = Some(ErrorView::from(e));
            });
            self.emit_file(file_id);
        }
        r?;
        self.record_history(None, file_id);
        self.emit_file(file_id);
        self.file(file_id)
    }

    fn record_history(&self, job_id: Option<&str>, file_id: &str) {
        let Ok(f) = self.snapshot(file_id) else { return };
        let _ = self.storage.add_history(&HistoryRecord {
            id: wm_common::new_id(),
            job_id: job_id.map(str::to_string),
            input: f.path.to_string_lossy().to_string(),
            output: f.output.as_ref().map(|p| p.to_string_lossy().to_string()),
            kind: match f.kind {
                FileKind::Image => "image".into(),
                FileKind::Pdf => "pdf".into(),
            },
            route: f.route.as_ref().map(|r| r.route.key().to_string()),
            quality: f.quality.as_ref().map(|q| q.score as f64),
            candidates: f.summary().to_remove as i64,
            status: match f.status() {
                wm_core::job::FileStatus::Completed => "completed".into(),
                wm_core::job::FileStatus::NeedsReview => "needs_review".into(),
                wm_core::job::FileStatus::Failed => "failed".into(),
                _ => "other".into(),
            },
            created_at: wm_common::now_millis(),
        });
    }

    /// 开始批量处理（全部去除）。只处理 **没有待复核候选** 且有需要去除内容的文件；
    /// 低置信度候选不会在后台无提示混入。
    ///
    /// `include_pending`：用户在批量处理对话框中明确选择“待复核的一起去除”时为 true——
    /// 开始前把这些文件里待复核的候选全部确认为去除。
    pub fn start_batch(self: &Arc<Self>, ids: Option<Vec<String>>, include_pending: bool) -> Result<String> {
        if include_pending {
            self.approve_pending(ids.as_deref());
        }
        let task = self.begin(TaskKind::Process)?;
        let me = self.clone();
        let tid = task.id.clone();
        std::thread::Builder::new()
            .name("magies-process".into())
            .spawn(move || {
                let _ = me.run_batch(&task, ids);
                me.end(&task.id);
            })
            .map_err(|e| AppError::internal(msg!("无法启动处理任务", "Could not start processing")).with_detail(e))?;
        Ok(tid)
    }

    /// 把指定文件（None = 全部）中待复核的候选确认为去除。
    pub fn approve_pending(&self, ids: Option<&[String]>) {
        let targets: Vec<String> = {
            let ws = self.ws.read();
            let all = ids.map(<[String]>::to_vec).unwrap_or_else(|| ws.order.clone());
            all.into_iter()
                .filter(|id| {
                    ws.files.get(id).is_some_and(|f| f.state != JobState::Processing && f.candidates.iter().any(|c| c.awaiting_review()))
                })
                .collect()
        };
        for id in targets {
            if let Err(e) = self.resolve_pending(&id, UserAction::Remove) {
                tracing::warn!(code = e.code(), "could not approve pending detections");
            }
        }
    }

    pub fn process_blocking(&self, ids: Option<Vec<String>>) -> Result<BatchSummary> {
        let task = self.begin(TaskKind::Process)?;
        let r = self.run_batch(&task, ids);
        self.end(&task.id);
        r
    }

    fn run_batch(&self, task: &ActiveTask, ids: Option<Vec<String>>) -> Result<BatchSummary> {
        let started = Instant::now();
        let settings = self.settings();
        let (targets, skipped): (Vec<String>, usize) = {
            let ws = self.ws.read();
            let all = ids.unwrap_or_else(|| ws.order.clone());
            let t: Vec<String> = all.iter().filter(|id| ws.files.get(*id).is_some_and(is_processable)).cloned().collect();
            let s = all.len() - t.len();
            (t, s)
        };
        let job_id = task.id.clone();
        self.storage.create_job(&job_id, targets.len(), &settings)?;
        for id in &targets {
            self.persist_item(&job_id, id);
        }
        let ctx = self.ctx();
        let total = targets.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        let failed = std::sync::atomic::AtomicUsize::new(0);
        let throttle = Throttle::new(120);
        let estimates: HashMap<String, u64> = targets.iter().map(|id| (id.clone(), self.estimate(id))).collect();
        wm_batch::run(
            &targets,
            &self.scheduler(),
            &task.control,
            |id| estimates.get(id).copied().unwrap_or(64 << 20),
            |_, id, control| {
                control.checkpoint()?;
                self.process_one(&ctx, id, false, &control.cancel)
            },
            |ev| {
                if let ItemEvent::Finished { index, result } = ev {
                    let id = &targets[index];
                    let n = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    match &result {
                        Ok(()) => {}
                        Err(e) if e.kind == ErrorKind::Cancelled => {
                            let _ = self.with_file(id, |f| {
                                f.state = JobState::Ready;
                                f.stage = ProcessingStage::Queued;
                            });
                        }
                        Err(e) => {
                            tracing::warn!(file = %id, code = e.code(), detail = ?e.detail, "processing failed");
                            failed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            let _ = self.with_file(id, |f| {
                                f.state = JobState::Failed;
                                f.stage = ProcessingStage::Queued;
                                f.error = Some(ErrorView::from(e));
                            });
                        }
                    }
                    self.persist_item(&job_id, id);
                    if result.is_ok() {
                        self.record_history(Some(&job_id), id);
                    }
                    if let Ok(v) = self.file(id) {
                        let ev = if matches!(&result, Err(e) if e.kind != ErrorKind::Cancelled) {
                            EngineEvent::ItemFailed { task_id: job_id.clone(), file: v }
                        } else {
                            EngineEvent::ItemCompleted { task_id: job_id.clone(), file: v }
                        };
                        self.sink.emit(ev);
                    }
                    if throttle.ready() || n == total {
                        self.sink.emit(EngineEvent::ProcessingProgress {
                            task_id: job_id.clone(),
                            done: n,
                            total,
                            failed: failed.load(std::sync::atomic::Ordering::SeqCst),
                            paused: task.control.pause.is_paused(),
                        });
                    }
                }
            },
        );
        let cancelled = task.control.cancel.is_cancelled();
        let _ = self.storage.update_job_counts(&job_id, if cancelled { "cancelled" } else { "completed" });
        let ws = self.ws.read();
        let count = |pred: &dyn Fn(&FileEntry) -> bool| targets.iter().filter(|id| ws.files.get(*id).is_some_and(pred)).count();
        let summary = BatchSummary {
            job_id: job_id.clone(),
            total,
            completed: count(&|f| f.state == JobState::Completed),
            failed: count(&|f| f.state == JobState::Failed),
            needs_review: count(&|f| f.review == ReviewStatus::NeedsReview),
            skipped,
            cancelled,
            output_dir: settings.output.output_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        drop(ws);
        self.sink.emit(EngineEvent::BatchCompleted { summary: summary.clone() });
        Ok(summary)
    }

    fn persist_item(&self, job_id: &str, id: &str) {
        let Ok(f) = self.snapshot(id) else { return };
        let _ = self.storage.upsert_item(&JobItemRecord {
            id: format!("{job_id}-{id}"),
            job_id: job_id.to_string(),
            file_id: id.to_string(),
            input: f.path.to_string_lossy().to_string(),
            import_root: f.import_root.as_ref().map(|p| p.to_string_lossy().to_string()),
            fingerprint: f.fingerprint.clone(),
            output: f.output.as_ref().map(|p| p.to_string_lossy().to_string()),
            state: f.state.as_str().to_string(),
            stage: format!("{:?}", f.stage).to_lowercase(),
            review: match f.review {
                ReviewStatus::None => "none",
                ReviewStatus::NeedsReview => "needs_review",
                ReviewStatus::Approved => "approved",
            }
            .into(),
            progress: f.progress as f64,
            error: f.error.as_ref().map(|e| e.code.clone()),
        });
    }

    // ───────────────────────── 应用到相似文件 ─────────────────────────

    /// 以已确认的候选为模板，在其它文件中做模板匹配；匹配可靠的位置标记为去除。
    /// 只复用经匹配验证的模板与 Mask 参数，不复制绝对坐标。
    pub fn apply_to_similar(self: &Arc<Self>, file_id: &str, candidate_id: &str) -> Result<String> {
        let entry = self.snapshot(file_id)?;
        if entry.kind != FileKind::Image {
            return Err(AppError::unsupported(msg!(
                "“应用到相似文件”目前仅支持图片",
                "\"Apply to similar files\" currently supports images only"
            )));
        }
        let cand = entry
            .candidates
            .iter()
            .find(|c| c.id == candidate_id)
            .cloned()
            .ok_or_else(|| AppError::internal(msg!("候选不存在", "Detection not found")))?;
        // 标记当前候选为去除
        self.set_candidate_action(file_id, candidate_id, UserAction::Remove)?;
        let task = self.begin(TaskKind::Similar)?;
        let me = self.clone();
        let tid = task.id.clone();
        let file_id = file_id.to_string();
        std::thread::spawn(move || {
            let _ = me.run_similar(&task, &file_id, cand);
            me.end(&task.id);
        });
        Ok(tid)
    }

    fn run_similar(&self, task: &ActiveTask, file_id: &str, cand: wm_core::WatermarkCandidate) -> Result<()> {
        let entry = self.snapshot(file_id)?;
        let profile = match cand.batch_profile_id.as_ref().and_then(|id| self.profiles.read().iter().find(|p| &p.id == id).cloned()) {
            Some(p) => p,
            None => {
                let img = wm_image::decode(&entry.path, None)?.buffer;
                let region =
                    entry.mask.as_ref().and_then(|m| m.regions.iter().find(|r| r.candidate_id.as_deref() == Some(cand.id.as_str())));
                ip::profile_from_candidate(&img, &cand, region).ok_or_else(|| {
                    AppError::internal(msg!("候选区域太小，无法建立模板", "The detection is too small to build a template"))
                })?
            }
        };
        let targets: Vec<String> = {
            let ws = self.ws.read();
            ws.order
                .iter()
                .filter(|id| {
                    id.as_str() != file_id && ws.files.get(*id).is_some_and(|f| f.kind == FileKind::Image && f.state == JobState::Ready)
                })
                .cloned()
                .collect()
        };
        let ctx = self.ctx();
        let total = targets.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        wm_batch::run(
            &targets,
            &self.scheduler(),
            &task.control,
            |id| self.estimate(id),
            |_, id, control| -> Result<()> {
                control.checkpoint()?;
                let e = self.snapshot(id)?;
                let d = wm_image::decode(&e.path, Some(ctx.memory_budget))?;
                let img = d.buffer;
                let (det, _) = wm_image::ops::thumbnail(&img, wm_image::DETECTION_MAX_SIDE);
                let Some(m) = wm_detection::batch::match_profile(&det, (img.width, img.height), &profile, 0.45) else { return Ok(()) };
                // 已有重叠候选：直接标记去除
                let updated = self.with_file(id, |f| {
                    if let Some(c) = f.candidates.iter_mut().find(|c| c.bbox.iou(&m.bbox) > 0.3) {
                        c.user_action = UserAction::Remove;
                        f.refresh_review();
                        true
                    } else {
                        false
                    }
                })?;
                if !updated {
                    let mut c = wm_core::WatermarkCandidate::new(
                        wm_common::new_id(),
                        cand.watermark_type,
                        cand.confidence.min(0.95) * (0.7 + 0.3 * m.score),
                        m.bbox,
                        wm_core::DetectorSource::BatchPersistence,
                    );
                    c.batch_score = m.score;
                    c.user_action = UserAction::Remove;
                    c.decision = CandidateDecision::Review;
                    let hint = wm_core::traits::MaskHint {
                        bbox: m.bbox,
                        data: wm_core::GrayU8 {
                            width: profile.template.width,
                            height: profile.template.height,
                            data: profile.template.support.iter().map(|&v| (v * 255.0) as u8).collect(),
                        },
                    };
                    let mut cs = vec![c];
                    let hints = HashMap::from([(cs[0].id.clone(), hint.clone())]);
                    let (mask, _) = ip::segment_candidates(&ctx, &img, id, &mut cs, &hints, &control.cancel)?;
                    let c = cs.remove(0);
                    let preview_size = e.preview_size.unwrap_or((img.width, img.height));
                    let new_mask = self.with_file(id, |f| {
                        let fid = f.id.clone();
                        let m = f.mask.get_or_insert_with(|| WatermarkMask::new(wm_common::new_id(), fid, img.width, img.height));
                        m.regions.extend(mask.regions);
                        m.version += 1;
                        let snapshot = m.clone();
                        f.hints.insert(c.id.clone(), hint);
                        f.candidates.push(c);
                        f.refresh_review();
                        snapshot
                    })?;
                    let ov = ip::write_overlay(&ctx, id, &new_mask, preview_size)?;
                    self.with_file(id, |f| f.mask_overlay = ov)?;
                }
                Ok(())
            },
            |ev| {
                if let ItemEvent::Finished { index, .. } = ev {
                    let n = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    self.emit_file(&targets[index]);
                    self.sink.emit(EngineEvent::ScanProgress { task_id: task.id.clone(), done: n, total, phase: "similar".into() });
                }
            },
        );
        self.sink.emit(EngineEvent::ScanCompleted {
            task_id: task.id.clone(),
            total,
            with_watermark: 0,
            needs_review: 0,
            profiles: 1,
            cancelled: task.control.cancel.is_cancelled(),
        });
        Ok(())
    }

    // ───────────────────────── 历史 / 恢复 ─────────────────────────

    pub fn history(&self, limit: usize, offset: usize) -> Result<Vec<HistoryRecord>> {
        self.storage.history(limit, offset)
    }
    pub fn clear_history(&self) -> Result<()> {
        self.storage.clear_history()
    }
    pub fn recent_jobs(&self, limit: usize) -> Result<Vec<JobRecord>> {
        self.storage.recent_jobs(limit)
    }

    /// 启动时发现的未完成任务。
    pub fn recoverable_jobs(&self) -> Result<Vec<JobRecord>> {
        self.storage.unfinished_jobs()
    }

    pub fn discard_job(&self, job_id: &str) -> Result<()> {
        self.storage.discard_job(job_id)
    }

    /// 恢复未完成任务：校验输入是否改变、输出是否已提交；已完成的不重复生成。
    /// 未完成的文件重新导入工作区（需重新扫描后处理）。
    pub fn resume_job(&self, job_id: &str) -> Result<ImportResult> {
        let items = self.storage.job_items(job_id)?;
        let mut paths = Vec::new();
        let mut changed = Vec::new();
        for it in items {
            if it.state == "completed" && it.output.as_ref().is_some_and(|o| Path::new(o).exists()) {
                continue;
            }
            let p = PathBuf::from(&it.input);
            let fp = wm_common::fast_fingerprint(&p).map(|f| f.0).unwrap_or_default();
            if fp != it.fingerprint {
                changed.push(it.input.clone());
                continue;
            }
            paths.push(p);
        }
        if let Some(s) = self.storage.job_settings(job_id) {
            let mut cur = self.settings();
            cur.output = s.output;
            *self.settings.write() = cur;
        }
        let mut r = self.import(paths)?;
        r.changed = changed;
        self.storage.discard_job(job_id)?;
        Ok(r)
    }
}

fn reset_results(f: &mut FileEntry) {
    f.result_file = None;
    f.result_preview = None;
    f.result_mask_version = None;
    f.route = None;
    f.quality = None;
    f.output = None;
    f.error = None;
}

/// 可被“全部去除”处理：已扫描、没有待复核内容、有需要去除的区域。
fn is_processable(f: &FileEntry) -> bool {
    let has_manual =
        f.mask.as_ref().is_some_and(|m| m.regions.iter().any(|r| r.candidate_id.as_deref() == Some(wm_image::maskops::MANUAL_REGION)));
    matches!(f.state, JobState::Ready)
        && f.review != ReviewStatus::NeedsReview
        && (f.summary().to_remove > 0 || has_manual)
        && !f.needs_password
}

/// 检查文件能否读取；被 macOS 隐私保护拦截时给出可执行的说明。
fn check_readable(path: &Path) -> Result<()> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| readable_error(path, e))?;
    let mut b = [0u8; 1];
    f.read(&mut b).map_err(|e| readable_error(path, e))?;
    Ok(())
}

fn readable_error(path: &Path, e: std::io::Error) -> AppError {
    if e.kind() != std::io::ErrorKind::PermissionDenied {
        return AppError::from(e);
    }
    let s = path.to_string_lossy();
    let in_app_container = s.contains("/Library/Containers/") || s.contains("/Library/Group Containers/");
    if in_app_container {
        AppError::permission(msg!(
            "macOS 阻止了读取：该文件位于其它应用的受保护目录（例如微信的聊天缓存）。请先在该应用中“另存为”到“下载”或“桌面”再导入，或在 系统设置 → 隐私与安全性 中允许本应用访问其他 App 的数据",
            "macOS blocked access: this file is inside another app's protected folder (for example WeChat's chat cache). Save it to Downloads or Desktop from that app first, or allow this app to access data from other apps in System Settings → Privacy & Security"
        ))
        .with_detail(e)
    } else {
        AppError::permission(msg!(
            "没有读取该文件的权限。请确认文件权限，或在 系统设置 → 隐私与安全性 中允许本应用访问该位置",
            "No permission to read this file. Check its permissions, or allow this app to access the location in System Settings → Privacy & Security"
        ))
        .with_detail(e)
    }
}

fn is_media_like(p: &Path) -> bool {
    matches!(
        wm_common::lower_ext(p).as_deref(),
        Some("heic" | "heif" | "avif" | "gif" | "raw" | "cr2" | "nef" | "arw" | "dng" | "jxl" | "ico")
    )
}

/// 自然排序（img2 < img10）。
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na: String = std::iter::from_fn(|| ai.next_if(|c| c.is_ascii_digit())).collect();
                let nb: String = std::iter::from_fn(|| bi.next_if(|c| c.is_ascii_digit())).collect();
                let o = na.parse::<u128>().unwrap_or(0).cmp(&nb.parse::<u128>().unwrap_or(0));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                ai.next();
                bi.next();
            }
        }
    }
}
