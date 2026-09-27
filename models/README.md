# 模型包（Model Package）

本目录随应用分发（`tauri.conf.json` → `bundle.resources`），运行时由 `wm-ai::ModelManager` 读取
`manifest.json`，**每次加载前校验 SHA-256**。权重文件（`*.onnx`）体积大，不入库，
用 `scripts/fetch-models.sh` 下载并校验。清单中仍为 `<actual-sha256-at-package-build>` 占位符的模型
显示为“未安装”，对应能力走经典算法路径，不会产生假结果。

## 当前状态

| 模型 | 状态 | 来源与许可 |
|---|---|---|
| `lama`（AI 修复） | 已接入 | 代码 Apache-2.0（advimman/lama）；ONNX 导出 Carve/LaMa-ONNX `lama_fp32.onnx`（Apache-2.0，opset 17，固定 512×512，输出 0..255）。**权重训练数据 Places365 限非商业研究与教学用途，商业分发前需法务确认** |
| `ocr_det` / `ocr_rec`（文字检测 / 识别） | 已接入 | PaddleOCR PP-OCRv4 中英文轻量模型（Apache-2.0），ONNX 转换 SWHL/RapidOCR；字库 `ocr-recognizer/ppocr_keys_v1.txt` |
| `wm_detector` / `wm_segmenter` | 未提供 | 设置页不显示；需用 `magies-cli synth` 生成的数据自行训练后放入 |

LaMa 固定 512×512 输入：`AiInpaintRemover` 在原图分辨率下以 512 窗口分块修复（每块约 2 秒，CPU），
大面积 Mask 不会被整体缩小再放大。

## 目录约定

```
models/
  manifest.json
  watermark-detector/wm_detector.onnx
  watermark-segmenter/wm_segmenter.onnx
  ocr-detector/ocr_det.onnx
  ocr-recognizer/ocr_rec.onnx
  inpainting/lama.onnx
```

## 输入输出契约（Adapter 按此实现，见 `crates/wm-ai/src/lib.rs`）

| 角色 | 输入 | 输出 |
|---|---|---|
| detector | `[1,3,S,S]` float32，0..1，等比缩放后左上对齐、灰边（114）填充 | `[1,N,6]`：`x1,y1,x2,y2,score,class`（输入像素坐标）；class 0 文字 / 1 Logo / 2 平铺 / 3 半透明 |
| segmenter | 候选裁剪 `[1,3,S,S]` float32，0..1 | `[1,1,S,S]` 概率（0..1）或 logits |
| inpainting | `image [1,3,H,W]` 0..1（洞区置 0）、`mask [1,1,H,W]` 0/1 | `[1,3,H,W]`，取值范围由 `outputScale` 声明 |
| ocr_detector | `x [1,3,H,W]` BGR，ImageNet 均值方差归一化，H/W 为 32 的倍数（`inputSize` 为最长边上限） | `[1,1,H,W]` 文字概率图（DB 后处理） |
| ocr_recognizer | `x [N,3,48,W]` BGR，归一化到 -1..1，右侧补 0；`dict` 指向字库 | `[N,T,C]` 类别概率：0 空白、1..=字库长度、最后一类空格（CTC 解码） |

## 打包步骤

1. 将经过评估的 ONNX 权重放入对应目录；
2. `shasum -a 256 <file>` 计算哈希，写入 `manifest.json`；
3. 记录数据许可、权重许可、版本、预处理参数、输入尺寸与 opset（见 `docs/ai-models.md`）；
4. 运行 `magies-cli models` 确认状态为“已就绪”；
5. 新模型可先用 `cargo run -p wm-ai --release --example probe_model -- <file.onnx>` 查看输入输出名称、形状与取值范围，
   据此填写 `inputSize` 与 `outputScale`。

模型更新与应用更新独立版本化；旧缓存随模型版本变化失效。
