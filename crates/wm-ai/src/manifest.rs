//! `models/manifest.json`（规格 §10.2）。校验值占位符必须在打包时替换。

use serde::{Deserialize, Serialize};
use std::path::Path;
use wm_core::{msg, tr};
use wm_core::{AppError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Detector,
    Segmenter,
    OcrDetector,
    OcrRecognizer,
    Inpainting,
}

impl ModelRole {
    pub fn label(&self) -> &'static str {
        match self {
            ModelRole::Detector => tr!("水印检测", "Watermark detection"),
            ModelRole::Segmenter => tr!("水印分割", "Watermark segmentation"),
            ModelRole::OcrDetector => tr!("文字检测", "Text detection"),
            ModelRole::OcrRecognizer => tr!("文字识别", "Text recognition"),
            ModelRole::Inpainting => tr!("AI 修复", "AI inpainting"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    pub role: ModelRole,
    pub version: String,
    /// 相对模型目录的路径。
    pub file: String,
    pub sha256: String,
    #[serde(default)]
    pub input_size: Option<[u32; 2]>,
    /// 输出取值范围（1.0 = 0..1，255.0 = 0..255）。
    #[serde(default)]
    pub output_scale: Option<f32>,
    #[serde(default)]
    pub opset: Option<u32>,
    #[serde(default)]
    pub license: Option<String>,
    /// 附带的字库文件（相对模型目录，文字识别模型使用）。
    #[serde(default)]
    pub dict: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub models: Vec<ModelEntry>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path).map_err(|e| {
            AppError::model(msg!("找不到模型清单 manifest.json", "Model manifest (manifest.json) not found")).with_detail(e)
        })?;
        serde_json::from_str(&s).map_err(|e| AppError::model(msg!("模型清单格式错误", "Model manifest is malformed")).with_detail(e))
    }
}
