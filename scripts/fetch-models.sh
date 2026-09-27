#!/usr/bin/env bash
# 下载模型权重到 models/，并按 models/manifest.json 中的 SHA-256 校验。
#
#   scripts/fetch-models.sh
#
# 包含：LaMa（AI 修复）、PP-OCRv4 中英文文字检测与识别。
# 水印检测、水印分割模型没有随应用提供（对应能力由内置算法完成）。
set -euo pipefail

cd "$(dirname "$0")/.."

fetch() {
  local id="$1" url="$2" dest="models/$3"
  local want
  want=$(python3 -c "import json;print(next(m['sha256'] for m in json.load(open('models/manifest.json'))['models'] if m['id']=='$id'))")
  if [[ -f "$dest" ]] && [[ "$(shasum -a 256 "$dest" | cut -d' ' -f1)" == "$want" ]]; then
    echo "✓ $id 已存在且校验通过"
    return
  fi
  mkdir -p "$(dirname "$dest")"
  echo "↓ 下载 $id ..."
  curl -L --fail --progress-bar -o "$dest.part" "$url"
  local got
  got=$(shasum -a 256 "$dest.part" | cut -d' ' -f1)
  if [[ "$got" != "$want" ]]; then
    rm -f "$dest.part"
    echo "✗ $id 校验失败：期望 $want，实际 $got" >&2
    exit 1
  fi
  mv "$dest.part" "$dest"
  echo "✓ $id 校验通过"
}

# LaMa：代码 Apache-2.0（advimman/lama），ONNX 导出 Carve/LaMa-ONNX（Apache-2.0）。
# 权重训练数据 Places365 限非商业研究与教学用途，商业分发前需法务确认。
fetch lama "https://huggingface.co/Carve/LaMa-ONNX/resolve/main/lama_fp32.onnx" "inpainting/lama.onnx"

# PP-OCRv4 中英文轻量模型：Apache-2.0（PaddleOCR），ONNX 转换 SWHL/RapidOCR。字库 ppocr_keys_v1.txt 已在仓库中。
fetch ocr_det "https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx" "ocr-detector/ocr_det.onnx"
fetch ocr_rec "https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_rec_infer.onnx" "ocr-recognizer/ocr_rec.onnx"
