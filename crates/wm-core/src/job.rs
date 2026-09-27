//! 批量任务与状态机（规格 §11）。
//!
//! `JobState` 为单项任务主状态；暂停是调度控制状态；复核是候选/质量状态，
//! 用独立的 [`ReviewStatus`] 表示，以区分“待人工决策”和“执行失败”。

use crate::error::ErrorView;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Scanning,
    Ready,
    Processing,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Scanning => "scanning",
            JobState::Ready => "ready",
            JobState::Processing => "processing",
            JobState::Completed => "completed",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => JobState::Queued,
            "scanning" => JobState::Scanning,
            "ready" => JobState::Ready,
            "processing" => JobState::Processing,
            "completed" => JobState::Completed,
            "failed" => JobState::Failed,
            "cancelled" => JobState::Cancelled,
            _ => return None,
        })
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, JobState::Completed | JobState::Failed | JobState::Cancelled)
    }

    /// 合法状态转换（Queued → Scanning → Ready → Processing → Completed/Failed/Cancelled）。
    pub fn can_transition_to(&self, next: JobState) -> bool {
        use JobState::*;
        match (self, next) {
            (a, b) if *a == b => true,
            (_, Cancelled) => !self.is_terminal() || *self == Failed,
            (_, Failed) => !self.is_terminal(),
            (Queued, Scanning) | (Scanning, Ready) | (Ready, Processing) | (Processing, Completed) => true,
            // 重新扫描 / 重试 / 从恢复点继续
            (Ready, Scanning) | (Failed, Queued) | (Cancelled, Queued) | (Failed, Ready) | (Cancelled, Ready) => true,
            (Completed, Ready) | (Completed, Scanning) => true,
            (Processing, Ready) => true, // 暂停在安全边界后回到 Ready
            (Queued, Ready) => true,     // 命中缓存
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    #[default]
    None,
    /// 有候选需要人工决定，或质量检查未通过。
    NeedsReview,
    /// 用户已复核。
    Approved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStage {
    #[default]
    Queued,
    Decode,
    Analyze,
    Detect,
    Segment,
    Remove,
    Evaluate,
    Encode,
    Commit,
    Done,
}

impl ProcessingStage {
    /// 阶段对应的进度基线（用于按阶段汇总进度）。
    pub fn progress_base(&self) -> f32 {
        match self {
            ProcessingStage::Queued => 0.0,
            ProcessingStage::Decode => 0.05,
            ProcessingStage::Analyze => 0.1,
            ProcessingStage::Detect => 0.2,
            ProcessingStage::Segment => 0.35,
            ProcessingStage::Remove => 0.5,
            ProcessingStage::Evaluate => 0.8,
            ProcessingStage::Encode => 0.85,
            ProcessingStage::Commit => 0.95,
            ProcessingStage::Done => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Image,
    Pdf,
}

/// 文件列表筛选使用的展示状态（规格 §2.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Waiting,
    Scanning,
    /// 检测到水印且无需复核。
    Detected,
    /// 扫描完成，未发现水印。
    Clean,
    NeedsReview,
    Processing,
    Completed,
    Failed,
}

impl FileStatus {
    pub fn derive(state: JobState, review: ReviewStatus, candidate_count: usize) -> FileStatus {
        match state {
            JobState::Queued => FileStatus::Waiting,
            JobState::Scanning => FileStatus::Scanning,
            JobState::Processing => FileStatus::Processing,
            JobState::Failed => FileStatus::Failed,
            JobState::Cancelled => FileStatus::Waiting,
            JobState::Completed => {
                if review == ReviewStatus::NeedsReview {
                    FileStatus::NeedsReview
                } else {
                    FileStatus::Completed
                }
            }
            JobState::Ready => {
                if review == ReviewStatus::NeedsReview {
                    FileStatus::NeedsReview
                } else if candidate_count == 0 {
                    FileStatus::Clean
                } else {
                    FileStatus::Detected
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingJob {
    pub id: String,
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub state: JobState,
    pub progress: f32,
    pub current_stage: ProcessingStage,
    pub review_status: ReviewStatus,
    pub error: Option<ErrorView>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_path_transitions_are_valid() {
        use JobState::*;
        let path = [Queued, Scanning, Ready, Processing, Completed];
        for w in path.windows(2) {
            assert!(w[0].can_transition_to(w[1]), "{:?} -> {:?}", w[0], w[1]);
        }
        assert!(!Completed.can_transition_to(Processing));
        assert!(!Queued.can_transition_to(Completed));
        assert!(Failed.can_transition_to(Queued));
    }

    #[test]
    fn needs_review_is_distinct_from_failed() {
        assert_eq!(FileStatus::derive(JobState::Ready, ReviewStatus::NeedsReview, 2), FileStatus::NeedsReview);
        assert_eq!(FileStatus::derive(JobState::Failed, ReviewStatus::None, 0), FileStatus::Failed);
        assert_eq!(FileStatus::derive(JobState::Ready, ReviewStatus::None, 0), FileStatus::Clean);
    }

    #[test]
    fn state_string_roundtrip() {
        for s in [JobState::Queued, JobState::Processing, JobState::Cancelled] {
            assert_eq!(JobState::parse(s.as_str()), Some(s));
        }
    }
}
