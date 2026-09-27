//! # wm-core
//!
//! Magies Clean 的领域模型、traits、Pipeline 契约与错误类型。
//! 本 crate 不依赖 Vue、Tauri、OpenCV、ONNX Runtime 或 PDF SDK（规格 §3.2）。

pub mod batch;
pub mod buffer;
pub mod candidate;
pub mod decision;
pub mod error;
pub mod geometry;
pub mod i18n;
pub mod job;
pub mod mask;
pub mod quality;
pub mod settings;
pub mod traits;

pub use buffer::{GrayF32, GrayU8, ImageBuffer, Tensor};
pub use candidate::{CandidateDecision, DetectorSource, Evidence, UserAction, WatermarkCandidate, WatermarkType};
pub use error::{AppError, ErrorKind, ErrorView, Result};
pub use geometry::{BoundingBox, CoordSpace, PixelRect, Point};
pub use i18n::{Language, Msg};
pub use mask::{MaskOp, MaskRegion, WatermarkMask};
pub use traits::CancellationToken;
