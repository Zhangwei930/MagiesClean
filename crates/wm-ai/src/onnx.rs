//! ONNX Runtime Adapter（`ort`）。Execution Provider 在此内部选择，不暴露给上层。

use crate::{ExecutionProvider, ModelRuntime};
use ort::session::Session;
use ort::value::Tensor as OrtTensor;
use parking_lot::Mutex;
use std::path::Path;
use wm_core::msg;
use wm_core::{AppError, Result, Tensor};

pub struct OnnxRuntime {
    session: Mutex<Session>,
    inputs: Vec<String>,
}

impl OnnxRuntime {
    pub fn load(path: &Path, provider: ExecutionProvider) -> Result<Self> {
        if provider != ExecutionProvider::Cpu {
            tracing::warn!(?provider, "non-CPU provider not validated in V1, falling back to CPU");
        }
        // 实测 LaMa 在全部核心上最快（含能效核）
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 16);
        let session = Session::builder()
            .and_then(|b| b.with_intra_threads(threads))
            .and_then(|b| b.commit_from_file(path))
            .map_err(|e| AppError::model(msg!("模型加载失败", "Failed to load the model")).with_detail(e))?;
        let inputs = session.inputs.iter().map(|i| i.name.clone()).collect();
        Ok(Self { session: Mutex::new(session), inputs })
    }
}

impl ModelRuntime for OnnxRuntime {
    fn input_names(&self) -> Vec<String> {
        self.inputs.clone()
    }

    fn run(&self, inputs: Vec<(String, Tensor)>) -> Result<Vec<Tensor>> {
        let map = |e: ort::Error| AppError::inference(msg!("模型推理失败", "Model inference failed")).with_detail(e);
        let mut values = Vec::with_capacity(inputs.len());
        for (name, t) in inputs {
            let shape: Vec<i64> = t.shape.iter().map(|&d| d as i64).collect();
            let v = OrtTensor::from_array((shape, t.data)).map_err(map)?;
            values.push((name, v));
        }
        let mut session = self.session.lock();
        let outputs = session.run(values).map_err(map)?;
        let mut out = Vec::new();
        for (_, v) in outputs.iter() {
            let (shape, data) = v.try_extract_tensor::<f32>().map_err(map)?;
            let shape: Vec<usize> = shape.iter().map(|&d| d.max(0) as usize).collect();
            out.push(
                Tensor::new(shape, data.to_vec())
                    .ok_or_else(|| AppError::inference(msg!("模型输出张量形状无效", "Model output tensor has an invalid shape")))?,
            );
        }
        Ok(out)
    }
}
