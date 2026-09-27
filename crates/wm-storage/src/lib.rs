//! # wm-storage
//!
//! SQLite、Settings、History、Preset 与可恢复任务（规格 §11.4、§12）。
//!
//! 只保存 path、metadata、status、configuration；**禁止把图片二进制放入数据库**。
//! 缓存目录中的文件使用 UUID 命名，不直接使用原文件名。

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use wm_core::batch::BatchWatermarkProfile;
use wm_core::msg;
use wm_core::settings::{builtin_presets, AppSettings, Preset};
use wm_core::{AppError, Result};

const SCHEMA_VERSION: i32 = 1;

fn db_err(e: rusqlite::Error) -> AppError {
    AppError::internal(msg!("本地数据库读写失败", "Local database error")).with_detail(e)
}

pub struct Storage {
    conn: Mutex<Connection>,
    pub cache: Cache,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRecord {
    pub id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub state: String,
    pub total: i64,
    pub completed: i64,
    pub failed: i64,
    pub needs_review: i64,
    pub output_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobItemRecord {
    pub id: String,
    pub job_id: String,
    pub file_id: String,
    pub input: String,
    pub import_root: Option<String>,
    pub fingerprint: String,
    pub output: Option<String>,
    pub state: String,
    pub stage: String,
    pub review: String,
    pub progress: f64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    pub id: String,
    pub job_id: Option<String>,
    pub input: String,
    pub output: Option<String>,
    pub kind: String,
    pub route: Option<String>,
    pub quality: Option<f64>,
    pub candidates: i64,
    pub status: String,
    pub created_at: i64,
}

impl Storage {
    /// 打开（或创建）数据目录下的数据库与缓存目录。
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let conn = Connection::open(data_dir.join("magies.db")).map_err(db_err)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;").map_err(db_err)?;
        migrate(&conn)?;
        let cache = Cache::open(&data_dir.join("cache"))?;
        Ok(Self { conn: Mutex::new(conn), cache })
    }

    pub fn open_in_memory(cache_dir: &Path) -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn), cache: Cache::open(cache_dir)? })
    }

    // ── 设置 ──
    pub fn load_settings(&self) -> AppSettings {
        let c = self.conn.lock();
        let v: Option<String> = c.query_row("SELECT value FROM settings WHERE key='app'", [], |r| r.get(0)).optional().ok().flatten();
        v.and_then(|s| serde_json::from_str::<AppSettings>(&s).ok()).unwrap_or_default().sanitized()
    }

    pub fn save_settings(&self, s: &AppSettings) -> Result<()> {
        let json = serde_json::to_string(s)
            .map_err(|e| AppError::internal(msg!("设置序列化失败", "Failed to serialize settings")).with_detail(e))?;
        self.conn
            .lock()
            .execute("INSERT INTO settings(key,value) VALUES('app',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![json])
            .map_err(db_err)?;
        Ok(())
    }

    // ── 预设 ──
    pub fn presets(&self) -> Result<Vec<Preset>> {
        let c = self.conn.lock();
        let mut stmt = c.prepare("SELECT data FROM presets ORDER BY created_at").map_err(db_err)?;
        let user: Vec<Preset> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        Ok(builtin_presets().into_iter().chain(user).collect())
    }

    pub fn save_preset(&self, p: &Preset) -> Result<()> {
        if p.builtin {
            return Err(AppError::permission(msg!("内置预设不能修改", "Built-in presets cannot be modified")));
        }
        let json = serde_json::to_string(p)
            .map_err(|e| AppError::internal(msg!("预设序列化失败", "Failed to serialize preset")).with_detail(e))?;
        self.conn
            .lock()
            .execute(
                "INSERT INTO presets(id,name,data,created_at) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET name=excluded.name, data=excluded.data",
                params![p.id, p.name, json, wm_common::now_millis()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn delete_preset(&self, id: &str) -> Result<()> {
        self.conn.lock().execute("DELETE FROM presets WHERE id=?1", params![id]).map_err(db_err)?;
        Ok(())
    }

    // ── 任务 ──
    pub fn create_job(&self, id: &str, total: usize, settings: &AppSettings) -> Result<()> {
        let now = wm_common::now_millis();
        let s = serde_json::to_string(settings).unwrap_or_default();
        let out = settings.output.output_dir.as_ref().map(|p| p.to_string_lossy().to_string());
        self.conn
            .lock()
            .execute(
                "INSERT INTO jobs(id,created_at,updated_at,state,total,completed,failed,needs_review,settings,output_dir) VALUES(?1,?2,?2,'processing',?3,0,0,0,?4,?5)",
                params![id, now, total as i64, s, out],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn upsert_item(&self, it: &JobItemRecord) -> Result<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO job_items(id,job_id,file_id,input,import_root,fingerprint,output,state,stage,review,progress,error,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                 ON CONFLICT(id) DO UPDATE SET output=excluded.output, state=excluded.state, stage=excluded.stage, review=excluded.review,
                   progress=excluded.progress, error=excluded.error, updated_at=excluded.updated_at",
                params![
                    it.id,
                    it.job_id,
                    it.file_id,
                    it.input,
                    it.import_root,
                    it.fingerprint,
                    it.output,
                    it.state,
                    it.stage,
                    it.review,
                    it.progress,
                    it.error,
                    wm_common::now_millis()
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn update_job_counts(&self, job_id: &str, state: &str) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "UPDATE jobs SET state=?2, updated_at=?3,
               completed=(SELECT COUNT(*) FROM job_items WHERE job_id=?1 AND state='completed'),
               failed=(SELECT COUNT(*) FROM job_items WHERE job_id=?1 AND state='failed'),
               needs_review=(SELECT COUNT(*) FROM job_items WHERE job_id=?1 AND review='needs_review')
             WHERE id=?1",
            params![job_id, state, wm_common::now_millis()],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 未完成（可恢复）的任务。
    pub fn unfinished_jobs(&self) -> Result<Vec<JobRecord>> {
        self.jobs_where("state IN ('processing','paused')")
    }

    pub fn recent_jobs(&self, limit: usize) -> Result<Vec<JobRecord>> {
        self.jobs_where(&format!("1=1 ORDER BY created_at DESC LIMIT {}", limit.min(500)))
    }

    fn jobs_where(&self, cond: &str) -> Result<Vec<JobRecord>> {
        let c = self.conn.lock();
        let mut stmt = c
            .prepare(&format!(
                "SELECT id,created_at,updated_at,state,total,completed,failed,needs_review,output_dir FROM jobs WHERE {cond}"
            ))
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(JobRecord {
                    id: r.get(0)?,
                    created_at: r.get(1)?,
                    updated_at: r.get(2)?,
                    state: r.get(3)?,
                    total: r.get(4)?,
                    completed: r.get(5)?,
                    failed: r.get(6)?,
                    needs_review: r.get(7)?,
                    output_dir: r.get(8)?,
                })
            })
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn job_settings(&self, job_id: &str) -> Option<AppSettings> {
        let c = self.conn.lock();
        c.query_row("SELECT settings FROM jobs WHERE id=?1", params![job_id], |r| r.get::<_, String>(0))
            .optional()
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn job_items(&self, job_id: &str) -> Result<Vec<JobItemRecord>> {
        let c = self.conn.lock();
        let mut stmt = c
            .prepare("SELECT id,job_id,file_id,input,import_root,fingerprint,output,state,stage,review,progress,error FROM job_items WHERE job_id=?1 ORDER BY rowid")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![job_id], |r| {
                Ok(JobItemRecord {
                    id: r.get(0)?,
                    job_id: r.get(1)?,
                    file_id: r.get(2)?,
                    input: r.get(3)?,
                    import_root: r.get(4)?,
                    fingerprint: r.get(5)?,
                    output: r.get(6)?,
                    state: r.get(7)?,
                    stage: r.get(8)?,
                    review: r.get(9)?,
                    progress: r.get(10)?,
                    error: r.get(11)?,
                })
            })
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// 放弃恢复：把任务标记为已取消。
    pub fn discard_job(&self, job_id: &str) -> Result<()> {
        self.conn
            .lock()
            .execute("UPDATE jobs SET state='cancelled', updated_at=?2 WHERE id=?1", params![job_id, wm_common::now_millis()])
            .map_err(db_err)?;
        Ok(())
    }

    // ── 历史 ──
    pub fn add_history(&self, h: &HistoryRecord) -> Result<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO processing_history(id,job_id,input,output,kind,route,quality,candidates,status,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![h.id, h.job_id, h.input, h.output, h.kind, h.route, h.quality, h.candidates, h.status, h.created_at],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn history(&self, limit: usize, offset: usize) -> Result<Vec<HistoryRecord>> {
        let c = self.conn.lock();
        let mut stmt = c
            .prepare("SELECT id,job_id,input,output,kind,route,quality,candidates,status,created_at FROM processing_history ORDER BY created_at DESC LIMIT ?1 OFFSET ?2")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |r| {
                Ok(HistoryRecord {
                    id: r.get(0)?,
                    job_id: r.get(1)?,
                    input: r.get(2)?,
                    output: r.get(3)?,
                    kind: r.get(4)?,
                    route: r.get(5)?,
                    quality: r.get(6)?,
                    candidates: r.get(7)?,
                    status: r.get(8)?,
                    created_at: r.get(9)?,
                })
            })
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn clear_history(&self) -> Result<()> {
        self.conn.lock().execute("DELETE FROM processing_history", []).map_err(db_err)?;
        Ok(())
    }

    // ── 批次模板 ──
    pub fn save_profiles(&self, profiles: &[BatchWatermarkProfile]) -> Result<()> {
        let c = self.conn.lock();
        for p in profiles {
            let json = serde_json::to_string(p)
                .map_err(|e| AppError::internal(msg!("模板序列化失败", "Failed to serialize template")).with_detail(e))?;
            c.execute(
                "INSERT INTO batch_profiles(id,group_key,data,created_at) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
                params![p.id, p.group_key, json, wm_common::now_millis()],
            )
            .map_err(db_err)?;
        }
        Ok(())
    }

    pub fn load_profiles(&self, ids: &[String]) -> Result<Vec<BatchWatermarkProfile>> {
        let c = self.conn.lock();
        let mut out = Vec::new();
        for id in ids {
            if let Some(s) = c
                .query_row("SELECT data FROM batch_profiles WHERE id=?1", params![id], |r| r.get::<_, String>(0))
                .optional()
                .map_err(db_err)?
            {
                if let Ok(p) = serde_json::from_str(&s) {
                    out.push(p);
                }
            }
        }
        Ok(out)
    }

    // ── 模型版本 ──
    pub fn record_model(&self, id: &str, version: &str, state: &str) -> Result<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO model_versions(id,version,state,checked_at) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET version=excluded.version, state=excluded.state, checked_at=excluded.checked_at",
                params![id, version, state, wm_common::now_millis()],
            )
            .map_err(db_err)?;
        Ok(())
    }
}

fn migrate(c: &Connection) -> Result<()> {
    let v: i32 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(db_err)?;
    if v < 1 {
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS presets (id TEXT PRIMARY KEY, name TEXT NOT NULL, data TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS jobs (
               id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, state TEXT NOT NULL,
               total INTEGER NOT NULL, completed INTEGER NOT NULL, failed INTEGER NOT NULL, needs_review INTEGER NOT NULL,
               settings TEXT NOT NULL, output_dir TEXT);
             CREATE TABLE IF NOT EXISTS job_items (
               id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE, file_id TEXT NOT NULL,
               input TEXT NOT NULL, import_root TEXT, fingerprint TEXT NOT NULL, output TEXT, state TEXT NOT NULL,
               stage TEXT NOT NULL, review TEXT NOT NULL, progress REAL NOT NULL, error TEXT, updated_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_items_job ON job_items(job_id);
             CREATE TABLE IF NOT EXISTS model_versions (id TEXT PRIMARY KEY, version TEXT NOT NULL, sha256 TEXT, state TEXT NOT NULL, checked_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS processing_history (
               id TEXT PRIMARY KEY, job_id TEXT, input TEXT NOT NULL, output TEXT, kind TEXT NOT NULL, route TEXT,
               quality REAL, candidates INTEGER NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_history_time ON processing_history(created_at);
             CREATE TABLE IF NOT EXISTS batch_profiles (id TEXT PRIMARY KEY, group_key TEXT NOT NULL, data TEXT NOT NULL, created_at INTEGER NOT NULL);",
        )
        .map_err(db_err)?;
        c.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};")).map_err(db_err)?;
    }
    Ok(())
}

/// 缓存目录：thumbnails/、masks/、preview/、pdf/、results/。文件名使用 UUID。
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKind {
    Thumbnails,
    Masks,
    Preview,
    Pdf,
    Results,
}

impl CacheKind {
    fn dir(&self) -> &'static str {
        match self {
            CacheKind::Thumbnails => "thumbnails",
            CacheKind::Masks => "masks",
            CacheKind::Preview => "preview",
            CacheKind::Pdf => "pdf",
            CacheKind::Results => "results",
        }
    }
    pub const ALL: [CacheKind; 5] = [CacheKind::Thumbnails, CacheKind::Masks, CacheKind::Preview, CacheKind::Pdf, CacheKind::Results];
}

impl Cache {
    pub fn open(root: &Path) -> Result<Self> {
        for k in CacheKind::ALL {
            std::fs::create_dir_all(root.join(k.dir()))?;
        }
        Ok(Self { root: root.to_path_buf() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 缓存文件路径：`<kind>/<key>.<ext>`。`key` 必须是 UUID / 哈希，不含原文件名。
    pub fn path(&self, kind: CacheKind, key: &str, ext: &str) -> PathBuf {
        debug_assert!(key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        self.root.join(kind.dir()).join(format!("{key}.{ext}"))
    }

    pub fn size_bytes(&self) -> u64 {
        walk_size(&self.root)
    }

    /// 清理缓存；`protected` 中的 key 前缀（运行中的任务）不会被删除。
    pub fn clear(&self, protected: &[String]) -> Result<u64> {
        let mut freed = 0;
        for k in CacheKind::ALL {
            let dir = self.root.join(k.dir());
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if protected.iter().any(|p| name.starts_with(p.as_str())) {
                    continue;
                }
                if let Ok(m) = e.metadata() {
                    if m.is_file() && std::fs::remove_file(e.path()).is_ok() {
                        freed += m.len();
                    }
                }
            }
        }
        Ok(freed)
    }
}

fn walk_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => walk_size(&e.path()),
            Ok(m) => m.len(),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Storage, PathBuf) {
        let d = std::env::temp_dir().join(format!("wmc-store-{}", wm_common::new_id()));
        (Storage::open(&d).unwrap(), d)
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        let (s, _) = store();
        assert_eq!(s.load_settings(), AppSettings::default());
        let mut st = AppSettings::default();
        st.auto_mode = wm_core::settings::AutoMode::Conservative;
        s.save_settings(&st).unwrap();
        assert_eq!(s.load_settings().auto_mode, wm_core::settings::AutoMode::Conservative);
    }

    #[test]
    fn presets_include_builtins_and_user() {
        let (s, _) = store();
        assert_eq!(s.presets().unwrap().len(), 4);
        let p = Preset {
            id: "u1".into(),
            name: "Mine".into(),
            detection_mode: Default::default(),
            confidence_threshold: 0.9,
            removal_quality: Default::default(),
            output_format: Default::default(),
            builtin: false,
        };
        s.save_preset(&p).unwrap();
        assert_eq!(s.presets().unwrap().len(), 5);
        s.delete_preset("u1").unwrap();
        assert_eq!(s.presets().unwrap().len(), 4);
        assert!(s.save_preset(&Preset { builtin: true, ..p }).is_err());
    }

    #[test]
    fn unfinished_jobs_are_recoverable() {
        let (s, d) = store();
        s.create_job("j1", 2, &AppSettings::default()).unwrap();
        for (i, st) in ["completed", "queued"].iter().enumerate() {
            s.upsert_item(&JobItemRecord {
                id: format!("i{i}"),
                job_id: "j1".into(),
                file_id: format!("f{i}"),
                input: format!("/x/{i}.jpg"),
                import_root: None,
                fingerprint: "fp".into(),
                output: None,
                state: st.to_string(),
                stage: "queued".into(),
                review: "none".into(),
                progress: 0.0,
                error: None,
            })
            .unwrap();
        }
        s.update_job_counts("j1", "processing").unwrap();
        drop(s);
        // 模拟重启
        let s2 = Storage::open(&d).unwrap();
        let jobs = s2.unfinished_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].completed, 1);
        assert_eq!(s2.job_items("j1").unwrap().len(), 2);
        s2.discard_job("j1").unwrap();
        assert!(s2.unfinished_jobs().unwrap().is_empty());
    }

    #[test]
    fn cache_clear_respects_protected() {
        let (s, _) = store();
        let a = s.cache.path(CacheKind::Preview, "keep123", "jpg");
        let b = s.cache.path(CacheKind::Preview, "drop456", "jpg");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"bb").unwrap();
        let freed = s.cache.clear(&["keep".into()]).unwrap();
        assert_eq!(freed, 2);
        assert!(a.exists() && !b.exists());
    }
}
