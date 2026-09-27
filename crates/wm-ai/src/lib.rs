//! # wm-ai
//!
//! ONNX Runtime 封装（`ModelRuntime`）、Model Manager、Tensor 与 Execution Provider（规格 §10）。
//!
//! - 模型以 `models/manifest.json` 描述，每次加载前校验 SHA-256；
//! - Execution Provider 不暴露给上层。V1 先保证 CPU 正确运行，CoreML / WinML 待验证后开启；
//! - 模型缺失时 **不使用假结果**：对应能力显示为“未安装”，由经典算法路径或复核兜底；
//! - Adapter 按 `models/README.md` 中的输入输出契约实现：检测器、分割器、Inpainting。

pub mod manifest;
pub mod ocr;
#[cfg(feature = "onnx")]
mod onnx;

pub use manifest::{Manifest, ModelEntry, ModelRole};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wm_core::traits::{AiInpaintBackend, OcrEngine, WatermarkDetector, WatermarkSegmenter};
use wm_core::{msg, tr};
use wm_core::{AppError, Result, Tensor};

/// 推理执行后端（抽象配置；具体接入由 Adapter 验证）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProvider {
    #[default]
    Cpu,
    CoreMl,
    WinMl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ModelState {
    /// 校验通过并已加载。
    Ready,
    /// 模型文件不存在（未安装模型包）。
    Missing,
    /// manifest 中的校验值仍是占位符（打包时未替换）。
    NotPackaged,
    /// 哈希不匹配：文件损坏或被替换。
    ChecksumMismatch,
    /// 文件有效但运行时加载失败。
    LoadFailed { reason: wm_core::Msg },
    /// 本构建未启用 ONNX Runtime。
    RuntimeUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub id: String,
    pub role: ModelRole,
    pub version: String,
    pub file: String,
    pub state: ModelState,
    pub provider: ExecutionProvider,
}

impl ModelStatus {
    pub fn is_ready(&self) -> bool {
        self.state == ModelState::Ready
    }
    /// 面向用户的状态描述。
    pub fn label(&self) -> &'static str {
        match self.state {
            ModelState::Ready => tr!("已就绪", "Ready"),
            ModelState::Missing => tr!("未安装", "Not installed"),
            ModelState::NotPackaged => tr!("未安装", "Not installed"),
            ModelState::ChecksumMismatch => tr!("校验失败", "Checksum failed"),
            ModelState::LoadFailed { .. } => tr!("加载失败", "Failed to load"),
            ModelState::RuntimeUnavailable => tr!("运行时不可用", "Runtime unavailable"),
        }
    }
}

/// 统一模型运行接口：加载、推理、错误封装。上层不感知 Execution Provider。
pub trait ModelRuntime: Send + Sync {
    fn input_names(&self) -> Vec<String>;
    fn run(&self, inputs: Vec<(String, Tensor)>) -> Result<Vec<Tensor>>;
}

struct Loaded {
    entry: ModelEntry,
    runtime: Arc<dyn ModelRuntime>,
    /// 字库（仅文字识别模型）。
    dict: Option<Arc<Vec<String>>>,
}

/// 模型管理器：读取 manifest、校验、按需加载。
pub struct ModelManager {
    dir: PathBuf,
    manifest: Manifest,
    statuses: RwLock<Vec<ModelStatus>>,
    loaded: RwLock<HashMap<String, Arc<Loaded>>>,
    provider: ExecutionProvider,
}

impl ModelManager {
    /// 从模型目录（包含 manifest.json）初始化并校验全部模型。
    pub fn open(dir: &Path) -> Self {
        let manifest = Manifest::load(&dir.join("manifest.json")).unwrap_or_default();
        let m = Self {
            dir: dir.to_path_buf(),
            manifest,
            statuses: RwLock::new(Vec::new()),
            loaded: RwLock::new(HashMap::new()),
            provider: ExecutionProvider::Cpu,
        };
        m.refresh();
        m
    }

    pub fn empty() -> Self {
        Self {
            dir: PathBuf::new(),
            manifest: Manifest::default(),
            statuses: RwLock::new(Vec::new()),
            loaded: RwLock::new(HashMap::new()),
            provider: ExecutionProvider::Cpu,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 重新校验模型文件（安装模型包后调用）。
    pub fn refresh(&self) {
        let mut out = Vec::new();
        let mut loaded = self.loaded.write();
        loaded.clear();
        for e in &self.manifest.models {
            let state = self.verify_and_load(e, &mut loaded);
            if !matches!(state, ModelState::Ready) {
                tracing::info!(model = %e.id, state = ?state, "model not available");
            }
            out.push(ModelStatus {
                id: e.id.clone(),
                role: e.role,
                version: e.version.clone(),
                file: e.file.clone(),
                state,
                provider: self.provider,
            });
        }
        *self.statuses.write() = out;
    }

    fn verify_and_load(&self, e: &ModelEntry, loaded: &mut HashMap<String, Arc<Loaded>>) -> ModelState {
        let path = self.dir.join(&e.file);
        if !path.exists() {
            return ModelState::Missing;
        }
        if e.sha256.starts_with('<') || e.sha256.len() != 64 {
            return ModelState::NotPackaged;
        }
        let dict = match &e.dict {
            Some(d) => match std::fs::read_to_string(self.dir.join(d)) {
                Ok(text) => Some(Arc::new(ocr::OnnxOcr::load_dict(&text))),
                Err(_) => return ModelState::Missing,
            },
            None => None,
        };
        match wm_common::sha256_file(&path) {
            Ok(h) if h.eq_ignore_ascii_case(&e.sha256) => {}
            Ok(_) => return ModelState::ChecksumMismatch,
            Err(err) => return ModelState::LoadFailed { reason: wm_core::Msg::new(err.to_string(), err.to_string()) },
        }
        #[cfg(feature = "onnx")]
        {
            match onnx::OnnxRuntime::load(&path, self.provider) {
                Ok(rt) => {
                    loaded.insert(e.id.clone(), Arc::new(Loaded { entry: e.clone(), runtime: Arc::new(rt), dict }));
                    ModelState::Ready
                }
                Err(err) => ModelState::LoadFailed { reason: err.message },
            }
        }
        #[cfg(not(feature = "onnx"))]
        {
            let _ = (loaded, dict);
            ModelState::RuntimeUnavailable
        }
    }

    pub fn statuses(&self) -> Vec<ModelStatus> {
        self.statuses.read().clone()
    }

    /// 已加载模型的版本签名（用于缓存失效）。
    pub fn version_signature(&self) -> String {
        let mut v: Vec<String> = self.statuses.read().iter().filter(|s| s.is_ready()).map(|s| format!("{}@{}", s.id, s.version)).collect();
        v.sort();
        v.join(",")
    }

    fn by_role(&self, role: ModelRole) -> Option<Arc<Loaded>> {
        self.loaded.read().values().find(|l| l.entry.role == role).cloned()
    }

    pub fn inpaint_backend(&self) -> Option<Arc<dyn AiInpaintBackend>> {
        let l = self.by_role(ModelRole::Inpainting)?;
        let size = l.entry.input_size.unwrap_or([512, 512]);
        Some(Arc::new(adapters::OnnxInpaint {
            rt: l.runtime.clone(),
            size: (size[0], size[1]),
            output_scale: l.entry.output_scale.unwrap_or(1.0),
        }))
    }

    pub fn detector(&self) -> Option<Arc<dyn WatermarkDetector>> {
        let l = self.by_role(ModelRole::Detector)?;
        let size = l.entry.input_size.unwrap_or([640, 640]);
        Some(Arc::new(adapters::OnnxDetector { rt: l.runtime.clone(), size: (size[0], size[1]), min_score: 0.25 }))
    }

    pub fn segmenter(&self) -> Option<Arc<dyn WatermarkSegmenter>> {
        let l = self.by_role(ModelRole::Segmenter)?;
        let size = l.entry.input_size.unwrap_or([320, 320]);
        Some(Arc::new(adapters::OnnxSegmenter { rt: l.runtime.clone(), size: (size[0], size[1]) }))
    }

    /// 文字检测 + 识别：两个模型与字库都就绪时可用。
    pub fn ocr(&self) -> Option<Arc<dyn OcrEngine>> {
        let det = self.by_role(ModelRole::OcrDetector)?;
        let rec = self.by_role(ModelRole::OcrRecognizer)?;
        let dict = rec.dict.clone().filter(|d| !d.is_empty())?;
        let [rec_w, rec_h] = rec.entry.input_size.unwrap_or([320, 48]);
        Some(Arc::new(ocr::OnnxOcr {
            det: det.runtime.clone(),
            rec: rec.runtime.clone(),
            dict: dict.as_ref().clone(),
            det_limit: det.entry.input_size.map_or(960, |s| s[0].max(s[1])),
            rec_h,
            rec_min_w: rec_w,
        }))
    }

    /// 某角色的模型是否可用。
    pub fn has(&self, role: ModelRole) -> bool {
        self.by_role(role).is_some()
    }
}

pub mod adapters {
    //! 模型 Adapter：把 ModelRuntime 适配为 core 的检测、分割、修复契约。

    use super::ModelRuntime;
    use std::sync::Arc;
    use wm_core::msg;
    use wm_core::settings::MaskParams;
    use wm_core::traits::{AiInpaintBackend, Detection, DetectionInput, SegmentContext, WatermarkDetector, WatermarkSegmenter};
    use wm_core::{
        AppError, BoundingBox, DetectorSource, GrayU8, ImageBuffer, MaskRegion, Result, Tensor, WatermarkCandidate, WatermarkType,
    };
    use wm_image::ops;

    fn to_nchw(img: &ImageBuffer) -> Tensor {
        let plane = (img.width * img.height) as usize;
        let mut t = Tensor::zeros(vec![1, 3, img.height as usize, img.width as usize]);
        for i in 0..plane {
            for c in 0..3 {
                t.data[c * plane + i] = img.data[i * 4 + c] as f32 / 255.0;
            }
        }
        t
    }

    /// LaMa 兼容 Inpainting：输入 image [1,3,H,W] 0..1、mask [1,1,H,W] 0/1；输出 [1,3,H,W]。
    pub struct OnnxInpaint {
        pub rt: Arc<dyn ModelRuntime>,
        pub size: (u32, u32),
        /// 输出取值范围：1.0 表示 0..1，255.0 表示 0..255。
        pub output_scale: f32,
    }

    impl AiInpaintBackend for OnnxInpaint {
        fn input_size(&self) -> (u32, u32) {
            self.size
        }
        fn inpaint(&self, image: &Tensor, mask: &Tensor) -> Result<Tensor> {
            let names = self.rt.input_names();
            let (ni, nm) =
                (names.first().cloned().unwrap_or_else(|| "image".into()), names.get(1).cloned().unwrap_or_else(|| "mask".into()));
            let mut out = self.rt.run(vec![(ni, image.clone()), (nm, mask.clone())])?;
            let mut t =
                out.pop().ok_or_else(|| AppError::inference(msg!("修复模型没有输出", "The inpainting model returned no output")))?;
            if (self.output_scale - 1.0).abs() > 1e-3 {
                for v in &mut t.data {
                    *v /= self.output_scale;
                }
            }
            Ok(t)
        }
    }

    /// 检测模型：输入 [1,3,S,S]（等比缩放 + 灰边填充），输出 [1,N,6] = (x1,y1,x2,y2,score,class)。
    /// class：0=文字 1=Logo 2=平铺 3=半透明。
    pub struct OnnxDetector {
        pub rt: Arc<dyn ModelRuntime>,
        pub size: (u32, u32),
        pub min_score: f32,
    }

    impl WatermarkDetector for OnnxDetector {
        fn name(&self) -> &'static str {
            "onnx-detector"
        }
        fn source(&self) -> DetectorSource {
            DetectorSource::AiDetector
        }
        fn detect(&self, input: &DetectionInput) -> Result<Vec<Detection>> {
            let (sw, sh) = self.size;
            let img = input.image;
            let s = (sw as f32 / img.width as f32).min(sh as f32 / img.height as f32);
            let (rw, rh) = (((img.width as f32 * s).round() as u32).max(1), ((img.height as f32 * s).round() as u32).max(1));
            let resized = ops::resize(img, rw, rh);
            let mut canvas = ImageBuffer::filled(sw, sh, [114, 114, 114, 255]);
            canvas.paste(&resized, 0, 0);
            let name = self.rt.input_names().first().cloned().unwrap_or_else(|| "images".into());
            let out = self.rt.run(vec![(name, to_nchw(&canvas))])?;
            let t = out.first().ok_or_else(|| AppError::inference(msg!("检测模型没有输出", "The detection model returned no output")))?;
            if t.shape.last() != Some(&6) {
                return Err(AppError::inference(msg!(
                    "检测模型输出不符合 [1,N,6] 契约",
                    "Detection model output does not match the [1,N,6] contract"
                )));
            }
            let mut dets = Vec::new();
            for row in t.data.chunks_exact(6) {
                let score = row[4];
                if score < self.min_score {
                    continue;
                }
                let b = BoundingBox::from_corners(row[0] / s, row[1] / s, row[2] / s, row[3] / s)
                    .clamp_to(img.width as f32, img.height as f32)
                    .to_normalized(img.width, img.height);
                if b.is_empty() {
                    continue;
                }
                let ty = match row[5].round() as i32 {
                    1 => WatermarkType::Logo,
                    2 => WatermarkType::Repeated,
                    3 => WatermarkType::Transparent,
                    _ => WatermarkType::Text,
                };
                dets.push(Detection::new(WatermarkCandidate::new(wm_common::new_id(), ty, score, b, DetectorSource::AiDetector)));
            }
            Ok(dets)
        }
    }

    /// 分割模型：输入候选裁剪 [1,3,S,S]，输出 [1,1,S,S] 概率（0..1）或 logits。
    pub struct OnnxSegmenter {
        pub rt: Arc<dyn ModelRuntime>,
        pub size: (u32, u32),
    }

    impl WatermarkSegmenter for OnnxSegmenter {
        fn name(&self) -> &'static str {
            "onnx-segmenter"
        }
        fn segment(&self, candidate: &WatermarkCandidate, ctx: &SegmentContext) -> Result<Option<MaskRegion>> {
            let (sw, sh) = self.size;
            let small = ops::resize(ctx.image, sw, sh);
            let name = self.rt.input_names().first().cloned().unwrap_or_else(|| "input".into());
            let out = self.rt.run(vec![(name, to_nchw(&small))])?;
            let t =
                out.first().ok_or_else(|| AppError::inference(msg!("分割模型没有输出", "The segmentation model returned no output")))?;
            if t.data.len() != (sw * sh) as usize {
                return Err(AppError::inference(msg!(
                    "分割模型输出不符合 [1,1,S,S] 契约",
                    "Segmentation model output does not match the [1,1,S,S] contract"
                )));
            }
            let logits = t.data.iter().any(|v| *v < 0.0 || *v > 1.0);
            let prob: Vec<u8> = t
                .data
                .iter()
                .map(|&v| {
                    let p = if logits { 1.0 / (1.0 + (-v).exp()) } else { v };
                    (p * 255.0).round().clamp(0.0, 255.0) as u8
                })
                .collect();
            let small_mask = GrayU8 { width: sw, height: sh, data: prob };
            let full = ops::resize_mask_bilinear(&small_mask, ctx.crop.width, ctx.crop.height);
            let thr = (ctx.params.mask_threshold * 255.0) as u8;
            let mut bin = ops::binarize(&full, thr.max(1));
            let scale = MaskParams::scale_for(ctx.full_width.max(ctx.full_height));
            bin = ops::remove_small_components(&bin, (ctx.params.min_component_size * scale * scale).max(2.0) as u32);
            if bin.count_nonzero() == 0 {
                return Ok(None);
            }
            let dil = ctx.params.mask_dilation * scale;
            if dil >= 0.25 {
                bin = ops::dilate(&bin, dil.max(1.0));
            }
            let m = ops::feather(&bin, ctx.params.mask_feather * scale);
            Ok(Some(MaskRegion::new(ctx.crop, m, Some(candidate.id.clone()))))
        }
    }
}

/// 校验某个张量形状（便于 Adapter 给出契约错误）。
pub fn expect_shape(t: &Tensor, rank: usize) -> Result<()> {
    if t.shape.len() != rank {
        return Err(AppError::inference(msg!(
            format!("模型输出维度应为 {rank}，实际为 {}", t.shape.len()),
            format!("Model output rank should be {rank}, got {}", t.shape.len())
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("wmc-ai-{}", wm_common::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_and_placeholder_models_are_reported_not_faked() {
        let d = tmpdir();
        std::fs::write(
            d.join("manifest.json"),
            r#"{"models":[
              {"id":"wm_detector","role":"detector","version":"1.0.0","file":"watermark-detector/wm_detector.onnx","sha256":"<actual-sha256-at-package-build>","inputSize":[640,640]},
              {"id":"lama","role":"inpainting","version":"1.0.0","file":"inpainting/lama.onnx","sha256":"<actual-sha256-at-package-build>"}
            ]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(d.join("inpainting")).unwrap();
        std::fs::write(d.join("inpainting/lama.onnx"), b"not a model").unwrap();
        let m = ModelManager::open(&d);
        let st = m.statuses();
        assert_eq!(st[0].state, ModelState::Missing);
        assert_eq!(st[1].state, ModelState::NotPackaged);
        assert!(m.inpaint_backend().is_none());
        assert!(m.detector().is_none());
    }

    #[test]
    fn checksum_mismatch_is_detected() {
        let d = tmpdir();
        std::fs::create_dir_all(d.join("inpainting")).unwrap();
        std::fs::write(d.join("inpainting/lama.onnx"), b"tampered").unwrap();
        let wrong = "0".repeat(64);
        std::fs::write(
            d.join("manifest.json"),
            format!(r#"{{"models":[{{"id":"lama","role":"inpainting","version":"1","file":"inpainting/lama.onnx","sha256":"{wrong}"}}]}}"#),
        )
        .unwrap();
        let m = ModelManager::open(&d);
        assert_eq!(m.statuses()[0].state, ModelState::ChecksumMismatch);
    }

    #[cfg(feature = "onnx")]
    #[test]
    fn valid_hash_but_corrupt_model_fails_to_load_cleanly() {
        let d = tmpdir();
        std::fs::create_dir_all(d.join("inpainting")).unwrap();
        let p = d.join("inpainting/lama.onnx");
        std::fs::write(&p, b"garbage bytes").unwrap();
        let h = wm_common::sha256_file(&p).unwrap();
        std::fs::write(
            d.join("manifest.json"),
            format!(r#"{{"models":[{{"id":"lama","role":"inpainting","version":"1","file":"inpainting/lama.onnx","sha256":"{h}"}}]}}"#),
        )
        .unwrap();
        let m = ModelManager::open(&d);
        assert!(matches!(m.statuses()[0].state, ModelState::LoadFailed { .. }), "{:?}", m.statuses()[0].state);
    }
}
