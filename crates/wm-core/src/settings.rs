//! 设置（规格 §1.3、§2.5、§2.6、§5、§8.2）。
//!
//! 自动模式与质量模式是两组 **独立** 设置，分别保存。

use crate::i18n::Language;
use crate::tr;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 自动模式：决定哪些候选可进入处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoMode {
    Conservative,
    #[default]
    Standard,
    Aggressive,
}

/// 质量模式：决定检测、分割和修复预算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QualityMode {
    Fast,
    #[default]
    Balanced,
    Best,
}

/// 置信度门槛：`confidence >= auto` 自动处理；`review <= confidence < auto` 进入复核；更低则忽略。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AutoThresholds {
    pub auto: f32,
    pub review: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdTable {
    pub conservative: AutoThresholds,
    pub standard: AutoThresholds,
    /// 原方案未指定数值：此处为可配置的默认值，界面需给出风险提示。
    pub aggressive: AutoThresholds,
}

impl Default for ThresholdTable {
    fn default() -> Self {
        Self {
            conservative: AutoThresholds { auto: 0.95, review: 0.60 },
            standard: AutoThresholds { auto: 0.85, review: 0.70 },
            aggressive: AutoThresholds { auto: 0.75, review: 0.55 },
        }
    }
}

impl ThresholdTable {
    pub fn for_mode(&self, mode: AutoMode) -> AutoThresholds {
        match mode {
            AutoMode::Conservative => self.conservative,
            AutoMode::Standard => self.standard,
            AutoMode::Aggressive => self.aggressive,
        }
    }
}

/// Mask 后处理参数。像素量以 **参考分辨率（长边 2048 px）** 为单位，实际使用时按分辨率换算。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskParams {
    /// 软 Mask 二值化阈值（0..1）。
    pub mask_threshold: f32,
    /// 膨胀半径（参考像素）。
    pub mask_dilation: f32,
    /// 羽化半径（参考像素）。
    pub mask_feather: f32,
    /// 最小连通域面积（参考像素²）。
    pub min_component_size: f32,
}

impl Default for MaskParams {
    fn default() -> Self {
        Self { mask_threshold: 0.5, mask_dilation: 2.0, mask_feather: 1.5, min_component_size: 6.0 }
    }
}

pub const REFERENCE_LONG_SIDE: f32 = 2048.0;

impl MaskParams {
    /// 参考像素 → 实际像素的比例。
    pub fn scale_for(long_side: u32) -> f32 {
        (long_side as f32 / REFERENCE_LONG_SIDE).max(0.05)
    }
}

/// 路由阈值（规格 §7：必须配置化并用 benchmark 校准）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouterConfig {
    /// Mask 面积占比低于该值时视为“小水印”。
    pub small_mask_area_ratio: f32,
    /// 场景复杂度（0..1）低于该值时视为“简单背景”。
    pub simple_scene_complexity: f32,
    /// alpha 估计质量（0..1）不低于该值时允许 AlphaRestore。
    pub alpha_quality_min: f32,
    /// 质量分低于该值进入复核。
    pub quality_review_threshold: f32,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self { small_mask_area_ratio: 0.02, simple_scene_complexity: 0.35, alpha_quality_min: 0.6, quality_review_threshold: 0.6 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// 与输入格式相同。
    #[default]
    Same,
    Jpeg,
    Png,
    Webp,
    Bmp,
    Tiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JpegQuality {
    /// 尽量接近原图压缩质量（估计原图量化表）；不等于逐字节无损。
    Preserve,
    Q90,
    #[default]
    Q95,
    Q100,
}

impl JpegQuality {
    pub fn fixed_value(&self) -> Option<u8> {
        match self {
            JpegQuality::Preserve => None,
            JpegQuality::Q90 => Some(90),
            JpegQuality::Q95 => Some(95),
            JpegQuality::Q100 => Some(100),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// 追加编号：`a_clean (2).jpg`。
    #[default]
    Number,
    /// 跳过已存在的输出。
    Skip,
    /// 覆盖已存在的 **输出** 文件（从不覆盖输入）。
    ReplaceOutput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSettings {
    pub format: OutputFormat,
    pub jpeg_quality: JpegQuality,
    /// 文件名后缀，默认 `_clean`。
    pub suffix: String,
    /// 导出根目录；None 时写到输入文件旁边。
    pub output_dir: Option<PathBuf>,
    /// 导入文件夹时是否在导出目录中保留子目录结构。
    pub preserve_structure: bool,
    pub conflict: ConflictPolicy,
    pub keep_metadata: bool,
    /// 高级：覆盖原文件。必须经过二次确认才能为 true。
    pub overwrite_originals: bool,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            format: OutputFormat::Same,
            jpeg_quality: JpegQuality::Q95,
            suffix: "_clean".into(),
            output_dir: None,
            preserve_structure: true,
            conflict: ConflictPolicy::Number,
            keep_metadata: true,
            overwrite_originals: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceSettings {
    /// None = 自动（CPU 核数 - 1，至少 1，上限 8）。
    pub cpu_workers: Option<usize>,
    /// 推理并发，默认 1。
    pub gpu_workers: usize,
    /// 内存预算（MB）；None = 自动（可用内存的一半，限制在 1–8 GB）。
    pub memory_budget_mb: Option<u64>,
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self { cpu_workers: None, gpu_workers: 1, memory_budget_mb: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchSettings {
    /// 覆盖默认抽样表的抽样数（None = 按规格 §6 默认表）。
    pub sample_override: Option<usize>,
    /// 是否启用批次学习。
    pub enabled: bool,
}

impl Default for BatchSettings {
    fn default() -> Self {
        Self { sample_override: None, enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub auto_mode: AutoMode,
    pub quality_mode: QualityMode,
    pub thresholds: ThresholdTable,
    pub mask: MaskParams,
    pub router: RouterConfig,
    pub output: OutputSettings,
    pub performance: PerformanceSettings,
    pub batch: BatchSettings,
    pub theme: Theme,
    /// 界面语言（默认英文）。
    pub language: Language,
    /// 高级模式：显示 Mask 参数、检测来源等技术信息。
    pub advanced_mode: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_mode: AutoMode::Standard,
            quality_mode: QualityMode::Balanced,
            thresholds: ThresholdTable::default(),
            mask: MaskParams::default(),
            router: RouterConfig::default(),
            output: OutputSettings::default(),
            performance: PerformanceSettings::default(),
            batch: BatchSettings::default(),
            theme: Theme::System,
            language: Language::En,
            advanced_mode: false,
        }
    }
}

impl AppSettings {
    pub fn thresholds(&self) -> AutoThresholds {
        self.thresholds.for_mode(self.auto_mode)
    }

    /// 校验并修正非法值（例如 review > auto）。
    pub fn sanitized(mut self) -> Self {
        for t in [&mut self.thresholds.conservative, &mut self.thresholds.standard, &mut self.thresholds.aggressive] {
            t.auto = t.auto.clamp(0.05, 1.0);
            t.review = t.review.clamp(0.0, t.auto);
        }
        self.mask.mask_threshold = self.mask.mask_threshold.clamp(0.05, 0.95);
        self.mask.mask_dilation = self.mask.mask_dilation.clamp(0.0, 20.0);
        self.mask.mask_feather = self.mask.mask_feather.clamp(0.0, 20.0);
        self.mask.min_component_size = self.mask.min_component_size.clamp(0.0, 500.0);
        if self.output.suffix.trim().is_empty() && self.output.output_dir.is_none() {
            // 没有后缀又写到原目录，会与原文件同名：强制恢复默认后缀。
            self.output.suffix = "_clean".into();
        }
        self.output.suffix = self.output.suffix.replace(['/', '\\', ':'], "_");
        self.performance.gpu_workers = self.performance.gpu_workers.clamp(1, 4);
        if let Some(w) = self.performance.cpu_workers.as_mut() {
            *w = (*w).clamp(1, 64);
        }
        self
    }
}

/// 预设（规格 §12）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub detection_mode: AutoMode,
    pub confidence_threshold: f32,
    pub removal_quality: QualityMode,
    pub output_format: OutputFormat,
    #[serde(default)]
    pub builtin: bool,
}

pub fn builtin_presets() -> Vec<Preset> {
    vec![
        Preset {
            id: "builtin-xhs".into(),
            name: tr!("XHS 社交图片", "Social media images").into(),
            detection_mode: AutoMode::Standard,
            confidence_threshold: 0.85,
            removal_quality: QualityMode::Balanced,
            output_format: OutputFormat::Jpeg,
            builtin: true,
        },
        Preset {
            id: "builtin-product".into(),
            name: tr!("Product Photos 商品图", "Product photos").into(),
            detection_mode: AutoMode::Standard,
            confidence_threshold: 0.85,
            removal_quality: QualityMode::Balanced,
            output_format: OutputFormat::Same,
            builtin: true,
        },
        Preset {
            id: "builtin-photography".into(),
            name: tr!("Photography 摄影作品", "Photography").into(),
            detection_mode: AutoMode::Conservative,
            confidence_threshold: 0.95,
            removal_quality: QualityMode::Best,
            output_format: OutputFormat::Same,
            builtin: true,
        },
        Preset {
            id: "builtin-pdf".into(),
            name: tr!("PDF Confidential 文档", "Confidential PDFs").into(),
            detection_mode: AutoMode::Conservative,
            confidence_threshold: 0.95,
            removal_quality: QualityMode::Balanced,
            output_format: OutputFormat::Same,
            builtin: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_modes_match_spec() {
        let s = AppSettings::default();
        assert_eq!(s.auto_mode, AutoMode::Standard);
        assert_eq!(s.quality_mode, QualityMode::Balanced);
        assert_eq!(s.thresholds().auto, 0.85);
        assert_eq!(s.thresholds().review, 0.70);
        assert_eq!(s.thresholds.conservative.auto, 0.95);
        assert!(!s.output.overwrite_originals);
        assert_eq!(s.language, Language::En);
    }

    #[test]
    fn sanitize_fixes_inverted_thresholds_and_empty_suffix() {
        let mut s = AppSettings::default();
        s.thresholds.standard = AutoThresholds { auto: 0.5, review: 0.9 };
        s.output.suffix = "  ".into();
        let s = s.sanitized();
        assert!(s.thresholds.standard.review <= s.thresholds.standard.auto);
        assert_eq!(s.output.suffix, "_clean");
    }

    #[test]
    fn settings_deserialize_with_missing_fields() {
        let s: AppSettings = serde_json::from_str(r#"{"autoMode":"aggressive"}"#).unwrap();
        assert_eq!(s.auto_mode, AutoMode::Aggressive);
        assert_eq!(s.quality_mode, QualityMode::Balanced);
    }
}
