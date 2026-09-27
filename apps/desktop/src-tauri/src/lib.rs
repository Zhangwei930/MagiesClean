//! Tauri 2 Commands + Events（规格 §12）。
//!
//! 命令只传 ID、路径、元数据和状态；预览图通过 asset 协议从缓存目录加载，
//! 不经 IPC 搬运图片数据。耗时操作在阻塞线程池执行，不阻塞 UI 线程。
//! 错误统一映射为 `ErrorView`（错误码、用户说明、是否可重试、下一步）。

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use wm_core::msg;
use wm_core::settings::{AppSettings, Preset};
use wm_core::{AppError, ErrorView, MaskOp, UserAction};
use wm_runtime::{Engine, EngineEvent, EventSink, FileView, ImportResult, TaskStatus, WorkspaceSummary};

struct AppState {
    engine: Arc<Engine>,
    data_dir: PathBuf,
    log_dir: PathBuf,
}

struct TauriSink(AppHandle);

impl EventSink for TauriSink {
    fn emit(&self, e: EngineEvent) {
        let _ = self.0.emit(e.name(), &e);
    }
}

type CmdResult<T> = Result<T, ErrorView>;

fn map<T>(r: wm_core::Result<T>) -> CmdResult<T> {
    r.map_err(|e| {
        // 诊断细节只进日志，不回传界面
        tracing::warn!(code = e.code(), detail = ?e.detail, "command failed");
        ErrorView::from(&e)
    })
}

/// 在阻塞线程池中执行引擎操作。
async fn blocking<T: Send + 'static>(engine: Arc<Engine>, f: impl FnOnce(&Engine) -> wm_core::Result<T> + Send + 'static) -> CmdResult<T> {
    map(tauri::async_runtime::spawn_blocking(move || f(&engine))
        .await
        .map_err(|e| AppError::internal(msg!("后台任务异常", "Background task failed")).with_detail(e))
        .and_then(|r| r))
}

/// 允许 WebView 通过 asset 协议读取指定文件（仅限用户导入的 PDF 原件，用于 PDF 预览）。
fn allow_assets(app: &AppHandle, files: &[FileView]) {
    let scope = app.asset_protocol_scope();
    for f in files {
        if f.kind == wm_core::job::FileKind::Pdf {
            let _ = scope.allow_file(&f.path);
        }
    }
}

// ───────────────────────── 导入与扫描 ─────────────────────────

#[tauri::command]
async fn import_files(app: AppHandle, state: State<'_, AppState>, paths: Vec<PathBuf>) -> CmdResult<ImportResult> {
    let r = blocking(state.engine.clone(), move |e| e.import(paths)).await?;
    allow_assets(&app, &r.added);
    Ok(r)
}

#[tauri::command]
async fn import_folder(app: AppHandle, state: State<'_, AppState>, path: PathBuf) -> CmdResult<ImportResult> {
    let r = blocking(state.engine.clone(), move |e| e.import(vec![path])).await?;
    allow_assets(&app, &r.added);
    Ok(r)
}

#[tauri::command]
fn scan_files(state: State<'_, AppState>, ids: Option<Vec<String>>) -> CmdResult<String> {
    map(state.engine.scan(ids))
}

#[tauri::command]
fn scan_file(state: State<'_, AppState>, id: String) -> CmdResult<String> {
    map(state.engine.scan(Some(vec![id])))
}

// ───────────────────────── 查询与预览 ─────────────────────────

#[tauri::command]
fn list_files(state: State<'_, AppState>) -> Vec<FileView> {
    state.engine.files()
}

#[tauri::command]
fn get_file_info(state: State<'_, AppState>, id: String) -> CmdResult<FileView> {
    map(state.engine.file(&id))
}

#[tauri::command]
fn get_detection_result(state: State<'_, AppState>, id: String) -> CmdResult<FileView> {
    map(state.engine.file(&id))
}

#[tauri::command]
fn workspace_summary(state: State<'_, AppState>) -> WorkspaceSummary {
    state.engine.summary()
}

#[tauri::command]
async fn generate_preview(state: State<'_, AppState>, id: String) -> CmdResult<FileView> {
    blocking(state.engine.clone(), move |e| e.generate_preview(&id)).await
}

/// 后台预计算选中图片的 AI 修复结果（立即返回）。
#[tauri::command]
fn warm_preview(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    map(state.engine.warm_preview(&id))
}

#[tauri::command]
async fn update_mask(state: State<'_, AppState>, id: String, ops: Vec<MaskOp>) -> CmdResult<FileView> {
    blocking(state.engine.clone(), move |e| e.update_mask(&id, ops)).await
}

#[tauri::command]
async fn undo_mask(state: State<'_, AppState>, id: String, redo: bool) -> CmdResult<FileView> {
    blocking(state.engine.clone(), move |e| e.undo_mask(&id, redo)).await
}

#[tauri::command]
fn set_candidate_action(state: State<'_, AppState>, id: String, candidate_id: String, action: UserAction) -> CmdResult<FileView> {
    map(state.engine.set_candidate_action(&id, &candidate_id, action))
}

#[tauri::command]
fn resolve_pending(state: State<'_, AppState>, id: String, action: UserAction) -> CmdResult<FileView> {
    map(state.engine.resolve_pending(&id, action))
}

#[tauri::command]
fn apply_to_similar(state: State<'_, AppState>, id: String, candidate_id: String) -> CmdResult<String> {
    map(state.engine.apply_to_similar(&id, &candidate_id))
}

#[tauri::command]
fn set_pdf_password(state: State<'_, AppState>, id: String, password: String) -> CmdResult<String> {
    map(state.engine.set_pdf_password(&id, password))
}

#[tauri::command]
fn confirm_signature(state: State<'_, AppState>, id: String) -> CmdResult<FileView> {
    map(state.engine.confirm_signature(&id))
}

#[tauri::command]
fn remove_files(state: State<'_, AppState>, ids: Vec<String>) -> CmdResult<()> {
    map(state.engine.remove_files(&ids))
}

#[tauri::command]
fn clear_workspace(state: State<'_, AppState>) -> CmdResult<()> {
    map(state.engine.clear_workspace())
}

// ───────────────────────── 批次控制 ─────────────────────────

#[tauri::command]
fn start_batch(state: State<'_, AppState>, ids: Option<Vec<String>>, include_pending: Option<bool>) -> CmdResult<String> {
    map(state.engine.start_batch(ids, include_pending.unwrap_or(false)))
}

#[tauri::command]
fn pause_batch(state: State<'_, AppState>) -> CmdResult<()> {
    map(state.engine.pause())
}

#[tauri::command]
fn resume_batch(state: State<'_, AppState>) -> CmdResult<()> {
    map(state.engine.resume())
}

#[tauri::command]
fn cancel_batch(state: State<'_, AppState>) -> CmdResult<()> {
    map(state.engine.cancel())
}

#[tauri::command]
fn active_task(state: State<'_, AppState>) -> Option<TaskStatus> {
    state.engine.active_task()
}

// ───────────────────────── 导出与设置 ─────────────────────────

#[tauri::command]
async fn export_result(state: State<'_, AppState>, id: String) -> CmdResult<FileView> {
    blocking(state.engine.clone(), move |e| e.export_file(&id)).await
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> AppSettings {
    state.engine.settings()
}

#[tauri::command]
fn update_settings(state: State<'_, AppState>, settings: AppSettings) -> CmdResult<AppSettings> {
    map(state.engine.update_settings(settings))
}

#[tauri::command]
fn get_presets(state: State<'_, AppState>) -> CmdResult<Vec<Preset>> {
    map(state.engine.presets())
}

#[tauri::command]
fn save_preset(state: State<'_, AppState>, preset: Preset) -> CmdResult<Vec<Preset>> {
    map(state.engine.save_preset(preset))
}

#[tauri::command]
fn delete_preset(state: State<'_, AppState>, id: String) -> CmdResult<Vec<Preset>> {
    map(state.engine.delete_preset(&id))
}

#[tauri::command]
fn apply_preset(state: State<'_, AppState>, id: String) -> CmdResult<AppSettings> {
    map(state.engine.apply_preset(&id))
}

// ───────────────────────── 模型 / 历史 / 恢复 / 缓存 ─────────────────────────

#[tauri::command]
fn get_model_status(state: State<'_, AppState>) -> Vec<wm_ai::ModelStatus> {
    state.engine.model_statuses()
}

#[tauri::command]
async fn reload_models(state: State<'_, AppState>) -> CmdResult<Vec<wm_ai::ModelStatus>> {
    blocking(state.engine.clone(), |e| Ok(e.reload_models())).await
}

#[tauri::command]
fn get_history(state: State<'_, AppState>, limit: Option<usize>, offset: Option<usize>) -> CmdResult<Vec<wm_storage::HistoryRecord>> {
    map(state.engine.history(limit.unwrap_or(200), offset.unwrap_or(0)))
}

#[tauri::command]
fn clear_history(state: State<'_, AppState>) -> CmdResult<()> {
    map(state.engine.clear_history())
}

#[tauri::command]
fn recent_jobs(state: State<'_, AppState>) -> CmdResult<Vec<wm_storage::JobRecord>> {
    map(state.engine.recent_jobs(50))
}

#[tauri::command]
fn recoverable_jobs(state: State<'_, AppState>) -> CmdResult<Vec<wm_storage::JobRecord>> {
    map(state.engine.recoverable_jobs())
}

#[tauri::command]
async fn resume_job(app: AppHandle, state: State<'_, AppState>, id: String) -> CmdResult<ImportResult> {
    let r = blocking(state.engine.clone(), move |e| e.resume_job(&id)).await?;
    allow_assets(&app, &r.added);
    Ok(r)
}

#[tauri::command]
fn discard_job(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    map(state.engine.discard_job(&id))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    data_dir: String,
    cache_dir: String,
    models_dir: String,
    log_dir: String,
    cache_bytes: u64,
    platform: String,
    arch: String,
}

#[tauri::command]
async fn app_info(state: State<'_, AppState>) -> CmdResult<AppInfo> {
    let data_dir = state.data_dir.clone();
    let log_dir = state.log_dir.clone();
    blocking(state.engine.clone(), move |e| {
        Ok(AppInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            data_dir: data_dir.to_string_lossy().to_string(),
            cache_dir: e.cache_dir().to_string_lossy().to_string(),
            models_dir: e.models_dir().to_string_lossy().to_string(),
            log_dir: log_dir.to_string_lossy().to_string(),
            cache_bytes: e.cache_size(),
            platform: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        })
    })
    .await
}

#[tauri::command]
async fn clear_cache(state: State<'_, AppState>) -> CmdResult<u64> {
    blocking(state.engine.clone(), |e| e.clear_cache()).await
}

/// 只允许打开 / 定位工作区中的输入、输出文件、导出目录与应用数据目录，防止任意路径访问。
fn path_allowed(state: &AppState, p: &Path) -> bool {
    if p.starts_with(&state.data_dir) {
        return true;
    }
    let settings = state.engine.settings();
    if settings.output.output_dir.as_ref().is_some_and(|d| p.starts_with(d)) {
        return true;
    }
    state
        .engine
        .files()
        .iter()
        .any(|f| Path::new(&f.path) == p || f.output.as_deref().map(Path::new) == Some(p) || Path::new(&f.path).parent() == Some(p))
}

#[tauri::command]
fn reveal_path(app: AppHandle, state: State<'_, AppState>, path: PathBuf) -> CmdResult<()> {
    if !path_allowed(&state, &path) {
        return Err(ErrorView::from(&AppError::permission(msg!("不允许访问该路径", "Access to this path is not allowed"))));
    }
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| ErrorView::from(&AppError::io(msg!("无法在文件管理器中显示", "Could not reveal in file manager")).with_detail(e)))
}

#[tauri::command]
fn open_path(app: AppHandle, state: State<'_, AppState>, path: PathBuf) -> CmdResult<()> {
    if !path_allowed(&state, &path) {
        return Err(ErrorView::from(&AppError::permission(msg!("不允许访问该路径", "Access to this path is not allowed"))));
    }
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| ErrorView::from(&AppError::io(msg!("无法打开文件", "Could not open the file")).with_detail(e)))
}

/// 打开系统隐私设置（macOS：隐私与安全性）。只打开固定的系统设置页面，不接受任意 URL。
#[tauri::command]
fn open_privacy_settings(app: AppHandle) -> CmdResult<()> {
    #[cfg(target_os = "macos")]
    let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
    #[cfg(target_os = "windows")]
    let url = "ms-settings:privacy";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let url = "";
    if url.is_empty() {
        return Ok(());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| ErrorView::from(&AppError::io(msg!("无法打开系统设置", "Could not open System Settings")).with_detail(e)))
}

/// 模型目录：打包后位于资源目录；开发时使用仓库中的 models/。
fn models_dir(app: &AppHandle) -> PathBuf {
    if let Ok(r) = app.path().resource_dir() {
        let p = r.join("models");
        if p.join("manifest.json").exists() {
            return p;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../models")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = app.path().app_data_dir()?;
            let log_dir = app.path().app_log_dir().unwrap_or_else(|_| data_dir.join("logs"));
            wm_runtime::init_logging(Some(&log_dir));
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "Magies Clean starting");
            let engine = Engine::new(&data_dir, &models_dir(&handle), Arc::new(TauriSink(handle.clone())))
                .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string()))?;
            // WebView 只能读取缓存目录（预览图、Mask 叠加层、结果 PDF）
            let _ = app.asset_protocol_scope().allow_directory(engine.cache_dir(), true);
            app.manage(AppState { engine, data_dir, log_dir });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            import_files,
            import_folder,
            scan_files,
            scan_file,
            list_files,
            get_file_info,
            get_detection_result,
            workspace_summary,
            generate_preview,
            warm_preview,
            update_mask,
            undo_mask,
            set_candidate_action,
            resolve_pending,
            apply_to_similar,
            set_pdf_password,
            confirm_signature,
            remove_files,
            clear_workspace,
            start_batch,
            pause_batch,
            resume_batch,
            cancel_batch,
            active_task,
            export_result,
            get_settings,
            update_settings,
            get_presets,
            save_preset,
            delete_preset,
            apply_preset,
            get_model_status,
            reload_models,
            get_history,
            clear_history,
            recent_jobs,
            recoverable_jobs,
            resume_job,
            discard_job,
            app_info,
            clear_cache,
            reveal_path,
            open_path,
            open_privacy_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Magies Clean");
}
