//! 自动决策与去除路由（规格 §2.5、§7）。
//!
//! 路由属于 core 契约与后端编排，UI 只提交模式和用户意图。

use crate::candidate::{CandidateDecision, WatermarkCandidate};
use crate::i18n::Msg;
use crate::settings::{AutoThresholds, QualityMode, RouterConfig};
use crate::{msg, tr};
use serde::{Deserialize, Serialize};

/// 门槛针对 **候选** 而非整张图片。
pub fn decide(confidence: f32, t: &AutoThresholds) -> CandidateDecision {
    if confidence >= t.auto {
        CandidateDecision::Auto
    } else if confidence >= t.review {
        CandidateDecision::Review
    } else {
        CandidateDecision::Ignore
    }
}

pub fn apply_decisions(cands: &mut [WatermarkCandidate], t: &AutoThresholds) {
    for c in cands {
        c.decision = decide(c.confidence, t);
    }
}

/// 一个文件的候选决策汇总，用于界面明确告知“全部去除”包含哪些候选。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionSummary {
    pub total: usize,
    /// 将被去除（自动 + 用户确认）。
    pub to_remove: usize,
    /// 等待人工复核。
    pub needs_review: usize,
    /// 忽略（低置信度或用户忽略）。
    pub ignored: usize,
}

pub fn summarize(cands: &[WatermarkCandidate]) -> DecisionSummary {
    let mut s = DecisionSummary { total: cands.len(), ..Default::default() };
    for c in cands {
        if c.should_remove() {
            s.to_remove += 1;
        } else if c.awaiting_review() {
            s.needs_review += 1;
        } else {
            s.ignored += 1;
        }
    }
    s
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalRoute {
    PdfObjectRemove,
    AlphaRestore,
    FastInpaint,
    /// 无 AI 模型时的复杂纹理路径：多尺度样本块合成（PatchMatch）。
    TextureSynthesis,
    AiInpaint,
}

impl RemovalRoute {
    /// 稳定的机器可读键（历史记录中保存，由界面翻译）。
    pub fn key(&self) -> &'static str {
        match self {
            RemovalRoute::PdfObjectRemove => "pdf_object_remove",
            RemovalRoute::AlphaRestore => "alpha_restore",
            RemovalRoute::FastInpaint => "fast_inpaint",
            RemovalRoute::TextureSynthesis => "texture_synthesis",
            RemovalRoute::AiInpaint => "ai_inpaint",
        }
    }

    pub fn msg(&self) -> Msg {
        let (zh, en) = match self {
            RemovalRoute::PdfObjectRemove => ("移除 PDF 水印对象", "Remove PDF watermark object"),
            RemovalRoute::AlphaRestore => ("透明度反推还原", "Alpha restore"),
            RemovalRoute::FastInpaint => ("快速修复", "Fast inpaint"),
            RemovalRoute::TextureSynthesis => ("纹理合成修复", "Texture synthesis"),
            RemovalRoute::AiInpaint => ("AI 修复", "AI inpainting"),
        };
        Msg::new(zh, en)
    }

    pub fn label(&self) -> &'static str {
        match self {
            RemovalRoute::PdfObjectRemove => tr!("移除 PDF 水印对象", "Remove PDF watermark object"),
            RemovalRoute::AlphaRestore => tr!("透明度反推还原", "Alpha restore"),
            RemovalRoute::FastInpaint => tr!("快速修复", "Fast inpaint"),
            RemovalRoute::TextureSynthesis => tr!("纹理合成修复", "Texture synthesis"),
            RemovalRoute::AiInpaint => tr!("AI 修复", "AI inpainting"),
        }
    }
}

/// 路由输入。所有数值由后端分析得到。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteContext {
    pub is_pdf_native: bool,
    pub is_transparent: bool,
    /// alpha / 水印颜色估计质量（0..1），无估计时为 None。
    pub alpha_quality: Option<f32>,
    pub mask_area_ratio: f32,
    /// Mask 周围背景复杂度（0..1）。
    pub scene_complexity: f32,
    pub quality_mode: QualityMode,
    pub ai_inpaint_available: bool,
    /// 洞的厚度：Mask 内像素到最近背景的最大距离，按分辨率换算到参考尺度（像素）。
    #[serde(default)]
    pub hole_thickness: f32,
}

/// 快速修复只适合细笔画：洞的厚度（参考尺度像素）不超过此值。
/// 更厚的实心块（标签底板、粗体大字）用快速修复会糊成一片，改走 AI / 纹理合成。
pub const THIN_HOLE_PX: f32 = 6.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteDecision {
    pub route: RemovalRoute,
    /// 面向用户的可解释原因（验收 A06：路由可解释）。
    pub reason: Msg,
}

/// Removal Strategy Router（规格 §7 伪代码的实现）。
pub fn route(ctx: &RouteContext, cfg: &RouterConfig) -> RouteDecision {
    if ctx.is_pdf_native {
        return RouteDecision {
            route: RemovalRoute::PdfObjectRemove,
            reason: msg!("PDF 原生水印对象，直接修改文档结构", "Native PDF watermark object; the document structure is edited directly"),
        };
    }
    if ctx.is_transparent {
        if let Some(q) = ctx.alpha_quality {
            if q >= cfg.alpha_quality_min {
                return RouteDecision {
                    route: RemovalRoute::AlphaRestore,
                    reason: msg!(format!("半透明水印且透明度估计可靠（{:.0}%）：内部反推原始背景，笔画边缘带快速修复", q * 100.0), format!("Semi-transparent watermark with a reliable alpha estimate ({:.0}%): the background is recovered inside strokes, and stroke edges are inpainted", q * 100.0)),
                };
            }
        }
    }
    let small = ctx.mask_area_ratio < cfg.small_mask_area_ratio;
    let simple = ctx.scene_complexity < cfg.simple_scene_complexity;
    let thin = ctx.hole_thickness <= THIN_HOLE_PX;
    if ctx.quality_mode == QualityMode::Fast || (small && simple && thin) {
        let why = if ctx.quality_mode == QualityMode::Fast && !(small && simple && thin) {
            msg!("快速模式", "Fast mode")
        } else {
            msg!("水印面积小、笔画细且背景简单", "Small, thin watermark on a simple background")
        };
        return RouteDecision {
            route: RemovalRoute::FastInpaint,
            reason: msg!(format!("{}，使用快速修复", why.zh), format!("{}; using fast inpaint", why.en)),
        };
    }
    if ctx.ai_inpaint_available {
        RouteDecision {
            route: RemovalRoute::AiInpaint,
            reason: msg!("背景纹理复杂或水印较大，使用本地 AI 修复", "Complex texture or large watermark; using local AI inpainting"),
        }
    } else {
        RouteDecision {
            route: RemovalRoute::TextureSynthesis,
            reason: msg!(
                "背景纹理复杂或水印较大；AI 修复模型未安装，使用纹理合成修复",
                "Complex texture or large watermark; the AI inpainting model is not installed, so texture synthesis is used"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ThresholdTable;

    #[test]
    fn standard_threshold_boundaries() {
        let t = ThresholdTable::default().standard;
        assert_eq!(decide(0.85, &t), CandidateDecision::Auto);
        assert_eq!(decide(0.8499, &t), CandidateDecision::Review);
        assert_eq!(decide(0.70, &t), CandidateDecision::Review);
        assert_eq!(decide(0.6999, &t), CandidateDecision::Ignore);
    }

    #[test]
    fn conservative_threshold_boundaries() {
        let t = ThresholdTable::default().conservative;
        assert_eq!(decide(0.95, &t), CandidateDecision::Auto);
        assert_eq!(decide(0.94, &t), CandidateDecision::Review);
    }

    fn ctx() -> RouteContext {
        RouteContext {
            is_pdf_native: false,
            is_transparent: false,
            alpha_quality: None,
            mask_area_ratio: 0.001,
            scene_complexity: 0.1,
            quality_mode: QualityMode::Balanced,
            ai_inpaint_available: false,
            hole_thickness: 2.0,
        }
    }

    #[test]
    fn router_follows_spec_order() {
        let cfg = RouterConfig::default();
        let mut c = ctx();
        c.is_pdf_native = true;
        assert_eq!(route(&c, &cfg).route, RemovalRoute::PdfObjectRemove);

        let mut c = ctx();
        c.is_transparent = true;
        c.alpha_quality = Some(0.9);
        assert_eq!(route(&c, &cfg).route, RemovalRoute::AlphaRestore);
        c.alpha_quality = Some(0.2);
        assert_eq!(route(&c, &cfg).route, RemovalRoute::FastInpaint);

        let mut c = ctx();
        c.scene_complexity = 0.8;
        assert_eq!(route(&c, &cfg).route, RemovalRoute::TextureSynthesis);
        c.ai_inpaint_available = true;
        assert_eq!(route(&c, &cfg).route, RemovalRoute::AiInpaint);
        c.quality_mode = QualityMode::Fast;
        assert_eq!(route(&c, &cfg).route, RemovalRoute::FastInpaint);
    }

    #[test]
    fn thick_holes_skip_fast_inpaint_even_when_small_and_simple() {
        let cfg = RouterConfig::default();
        let mut c = ctx();
        assert_eq!(route(&c, &cfg).route, RemovalRoute::FastInpaint);
        c.hole_thickness = 20.0;
        assert_eq!(route(&c, &cfg).route, RemovalRoute::TextureSynthesis);
        c.ai_inpaint_available = true;
        assert_eq!(route(&c, &cfg).route, RemovalRoute::AiInpaint);
    }
}
