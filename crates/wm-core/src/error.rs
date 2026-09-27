//! 统一错误类型（规格 §12）。
//!
//! 每个错误都映射为：稳定错误码、用户可理解的说明、是否可重试、下一步操作。
//! UI 只展示这些业务信息，内部细节（`detail`）只进入受控日志。

use crate::i18n::Msg;
use crate::msg;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Io,
    UnsupportedFormat,
    Decode,
    Encode,
    Pdf,
    Model,
    Inference,
    OutOfMemory,
    Permission,
    Cancelled,
    InvalidPassword,
    /// 需要用户确认后才能继续（例如：数字签名 PDF）。
    NeedsConfirmation,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
pub struct AppError {
    pub kind: ErrorKind,
    /// 面向用户的简短说明（双语，按界面语言输出）。
    pub message: Msg,
    /// 仅用于诊断日志；不得包含图片内容、OCR 全文或密码。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub type Result<T, E = AppError> = std::result::Result<T, E>;

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(d) => write!(f, "{:?}: {} ({})", self.kind, self.message, d),
            None => write!(f, "{:?}: {}", self.kind, self.message),
        }
    }
}

impl AppError {
    pub fn new(kind: ErrorKind, message: impl Into<Msg>) -> Self {
        Self { kind, message: message.into(), detail: None }
    }

    pub fn with_detail(mut self, detail: impl fmt::Display) -> Self {
        self.detail = Some(detail.to_string());
        self
    }

    pub fn io(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::Io, message)
    }
    pub fn unsupported(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::UnsupportedFormat, message)
    }
    pub fn decode(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::Decode, message)
    }
    pub fn encode(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::Encode, message)
    }
    pub fn pdf(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::Pdf, message)
    }
    pub fn model(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::Model, message)
    }
    pub fn inference(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::Inference, message)
    }
    pub fn oom(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::OutOfMemory, message)
    }
    pub fn permission(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::Permission, message)
    }
    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, msg!("任务已取消", "Task cancelled"))
    }
    pub fn invalid_password() -> Self {
        Self::new(ErrorKind::InvalidPassword, msg!("PDF 密码不正确", "Incorrect PDF password"))
    }
    pub fn needs_confirmation(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::NeedsConfirmation, message)
    }
    pub fn internal(message: impl Into<Msg>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            ErrorKind::Io => "E_IO",
            ErrorKind::UnsupportedFormat => "E_UNSUPPORTED_FORMAT",
            ErrorKind::Decode => "E_DECODE",
            ErrorKind::Encode => "E_ENCODE",
            ErrorKind::Pdf => "E_PDF",
            ErrorKind::Model => "E_MODEL",
            ErrorKind::Inference => "E_INFERENCE",
            ErrorKind::OutOfMemory => "E_OOM",
            ErrorKind::Permission => "E_PERMISSION",
            ErrorKind::Cancelled => "E_CANCELLED",
            ErrorKind::InvalidPassword => "E_INVALID_PASSWORD",
            ErrorKind::NeedsConfirmation => "E_NEEDS_CONFIRMATION",
            ErrorKind::Internal => "E_INTERNAL",
        }
    }

    /// 是否可以直接重试（不需要用户先改变输入或环境）。
    pub fn retryable(&self) -> bool {
        matches!(self.kind, ErrorKind::Io | ErrorKind::OutOfMemory | ErrorKind::Inference | ErrorKind::Cancelled)
    }

    /// 给用户的下一步建议。
    pub fn next_step(&self) -> Msg {
        match self.kind {
            ErrorKind::Io => msg!(
                "检查文件是否仍在原位置、磁盘空间是否充足，然后重试。",
                "Check that the file is still in place and that there is enough disk space, then try again."
            ),
            ErrorKind::UnsupportedFormat => msg!(
                "该文件格式或子类型暂不支持，可转换为 JPG / PNG / PDF 后再导入。",
                "This format or variant is not supported yet. Convert it to JPG, PNG or PDF and import it again."
            ),
            ErrorKind::Decode => {
                msg!("文件可能已损坏，请用其它软件确认能否正常打开。", "The file may be damaged. Check whether it opens in another app.")
            }
            ErrorKind::Encode => msg!("尝试更换导出格式或导出目录后重试。", "Try a different output format or folder."),
            ErrorKind::Pdf => msg!(
                "PDF 结构无法安全修改，可在复核中查看原因或跳过该文件。",
                "The PDF structure cannot be edited safely. Review the reason or skip this file."
            ),
            ErrorKind::Model => {
                msg!("在 设置 → 模型 中检查模型包是否完整安装。", "Check that the model package is fully installed in Settings → Models.")
            }
            ErrorKind::Inference => msg!("可切换为 快速 质量模式后重试。", "Switch to Fast quality mode and try again."),
            ErrorKind::OutOfMemory => {
                msg!("降低并发数或切换为 快速 模式后重试。", "Lower the number of parallel jobs or switch to Fast mode, then try again.")
            }
            ErrorKind::Permission => msg!(
                "请确认有该文件或目录的读写权限；如果文件来自其它应用（例如微信），先另存到“下载”或“桌面”，或在 系统设置 → 隐私与安全性 中授权。",
                "Make sure you can read and write this file or folder. If it came from another app (e.g. WeChat), save it to Downloads or Desktop first, or grant access in System Settings → Privacy & Security."
            ),
            ErrorKind::Cancelled => msg!("可在需要时重新开始处理。", "You can start processing again at any time."),
            ErrorKind::InvalidPassword => msg!("请输入正确的 PDF 打开密码。", "Enter the correct password to open the PDF."),
            ErrorKind::NeedsConfirmation => msg!("确认后继续处理，或跳过该文件。", "Confirm to continue, or skip this file."),
            ErrorKind::Internal => msg!(
                "请重试；如果问题持续，可在设置中导出诊断日志。",
                "Please try again. If the problem persists, check the diagnostic log in Settings."
            ),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        match e.kind() {
            K::PermissionDenied => {
                AppError::permission(msg!("没有访问该文件或目录的权限", "No permission to access this file or folder")).with_detail(e)
            }
            K::NotFound => AppError::io(msg!("找不到文件", "File not found")).with_detail(e),
            K::OutOfMemory => AppError::oom(msg!("内存不足", "Out of memory")).with_detail(e),
            _ => {
                // StorageFull 在稳定版 Rust 中按 raw os error 识别（ENOSPC=28 / ERROR_DISK_FULL=112）
                if matches!(e.raw_os_error(), Some(28) | Some(112)) {
                    AppError::io(msg!("磁盘空间不足", "Not enough disk space")).with_detail(e)
                } else {
                    AppError::io(msg!("无法读取或写入文件", "Could not read or write the file")).with_detail(e)
                }
            }
        }
    }
}

/// 发送给前端的错误视图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorView {
    pub code: String,
    pub kind: ErrorKind,
    pub message: Msg,
    pub retryable: bool,
    pub next_step: Msg,
}

impl From<&AppError> for ErrorView {
    fn from(e: &AppError) -> Self {
        Self { code: e.code().to_string(), kind: e.kind, message: e.message.clone(), retryable: e.retryable(), next_step: e.next_step() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_permission_maps_to_permission_kind() {
        let e: AppError = std::io::Error::from(std::io::ErrorKind::PermissionDenied).into();
        assert_eq!(e.kind, ErrorKind::Permission);
        assert!(!e.retryable());
    }

    #[test]
    fn error_view_has_no_internal_detail() {
        let e = AppError::decode(msg!("无法读取图片", "Could not read the image")).with_detail("panicked at xyz");
        let v = ErrorView::from(&e);
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("panicked"));
        assert_eq!(v.code, "E_DECODE");
    }
}
