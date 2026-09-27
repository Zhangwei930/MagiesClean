//! 去除质量评估结果（规格 §7.2）。
//!
//! 完成状态与质量状态分开：一次修复调用成功不代表可交付。

use crate::i18n::Msg;
use crate::tr;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityIssueKind {
    /// 水印残留。
    Residual,
    /// 修复区域异常模糊。
    Blur,
    /// 颜色不连续。
    ColorShift,
    /// Mask 边缘伪影。
    EdgeArtifact,
    /// 候选外区域发生了意外变化。
    OutsideChange,
    /// 文档结构异常（页数、文本等）。
    Structure,
}

impl QualityIssueKind {
    pub fn label(&self) -> &'static str {
        match self {
            QualityIssueKind::Residual => tr!("可能有水印残留", "Possible watermark residue"),
            QualityIssueKind::Blur => tr!("修复区域偏模糊", "Repaired area looks blurry"),
            QualityIssueKind::ColorShift => tr!("修复区域颜色不连续", "Color mismatch in the repaired area"),
            QualityIssueKind::EdgeArtifact => tr!("修复边缘有伪影", "Artifacts along the repair edge"),
            QualityIssueKind::OutsideChange => tr!("水印区域外出现意外改动", "Unexpected changes outside the watermark"),
            QualityIssueKind::Structure => tr!("文档结构异常", "Document structure problem"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityIssue {
    pub kind: QualityIssueKind,
    /// 0..1 严重程度。
    pub severity: f32,
    pub message: Msg,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityReport {
    /// 0..1，越高越好。复核辅助信号，不能证明被遮挡细节已真实恢复。
    pub score: f32,
    pub passed: bool,
    pub issues: Vec<QualityIssue>,
}

impl QualityReport {
    pub fn from_issues(issues: Vec<QualityIssue>, threshold: f32) -> Self {
        // 严重问题（结构/区域外改动）直接判为不合格。
        let blocking =
            issues.iter().any(|i| matches!(i.kind, QualityIssueKind::OutsideChange | QualityIssueKind::Structure) && i.severity > 0.0);
        let penalty: f32 = issues.iter().map(|i| i.severity * weight(i.kind)).sum();
        let score = (1.0 - penalty).clamp(0.0, 1.0);
        Self { score, passed: !blocking && score >= threshold, issues }
    }

    pub fn perfect() -> Self {
        Self { score: 1.0, passed: true, issues: Vec::new() }
    }
}

fn weight(k: QualityIssueKind) -> f32 {
    match k {
        QualityIssueKind::Residual => 0.6,
        QualityIssueKind::Blur => 0.3,
        QualityIssueKind::ColorShift => 0.4,
        QualityIssueKind::EdgeArtifact => 0.3,
        QualityIssueKind::OutsideChange => 1.0,
        QualityIssueKind::Structure => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_issue_fails_even_with_high_score() {
        let r = QualityReport::from_issues(
            vec![QualityIssue { kind: QualityIssueKind::OutsideChange, severity: 0.01, message: Msg::new("", "") }],
            0.5,
        );
        assert!(r.score > 0.9);
        assert!(!r.passed);
    }

    #[test]
    fn residual_lowers_score() {
        let r = QualityReport::from_issues(
            vec![QualityIssue { kind: QualityIssueKind::Residual, severity: 0.9, message: Msg::new("", "") }],
            0.6,
        );
        assert!(!r.passed);
    }
}
