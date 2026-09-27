//! Synthetic Dataset Generator 与 Benchmark（规格 §10.3、§14）。
//!
//! 生成：无水印原图（程序化背景）+ 文字水印（随机 Opacity、Rotation、Scale、平铺）→
//! 输出 clean image、watermarked image、ground truth mask 与 metadata。
//! Benchmark：统计检测 Precision / Recall、Mask IoU、负样本误检与耗时分位数。
//! 这些数字只代表固定版本的合成基准，不能宣传为全场景准确率。

use crate::engine::Engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use wm_core::{AppError, BoundingBox, Result};
use wm_image::synth::{self, Rng};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetKind {
    /// 同一水印出现在一批不同图片的固定位置。
    Batch,
    /// 单图随机位置文字水印。
    Single,
    /// 平铺重复水印。
    Repeated,
    /// 负样本：含正常文字（招牌、正文）但无水印。
    Negatives,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleMeta {
    pub file: String,
    pub kind: DatasetKind,
    pub width: u32,
    pub height: u32,
    pub text: Option<String>,
    pub opacity: Option<f32>,
    pub rotation: Option<f32>,
    /// 真值包围盒（归一化）。负样本为空。
    pub bbox: Option<BoundingBox>,
}

const WORDS: &[&str] = &["SHOP.EXAMPLE", "@LENSMASTER", "WWW.PHOTO.CN", "SAMPLE", "PREVIEW", "COPYRIGHT 2026", "STUDIO.IO"];

fn save_rgba(img: &wm_core::ImageBuffer, p: &Path) -> Result<()> {
    let enc = wm_image::encode(
        img,
        &wm_image::EncodeOptions {
            format: wm_image::ImageFormatKind::Png,
            jpeg_quality: 95,
            metadata: None,
            color: Some(wm_image::SourceColor::Rgb8),
        },
    )?;
    std::fs::write(p, enc.bytes)?;
    Ok(())
}

fn save_jpeg(img: &wm_core::ImageBuffer, p: &Path, q: u8) -> Result<()> {
    let enc = wm_image::encode(
        img,
        &wm_image::EncodeOptions { format: wm_image::ImageFormatKind::Jpeg, jpeg_quality: q, metadata: None, color: None },
    )?;
    std::fs::write(p, enc.bytes)?;
    Ok(())
}

/// 生成数据集到 `out/{clean,watermarked,mask,meta}`。
pub fn generate(out: &Path, kind: DatasetKind, count: usize, seed: u64) -> Result<Vec<SampleMeta>> {
    for d in ["clean", "watermarked", "mask", "meta"] {
        std::fs::create_dir_all(out.join(d))?;
    }
    let mut rng = Rng::new(seed);
    let batch_text = WORDS[(seed as usize) % WORDS.len()];
    let batch_opacity = rng.range(0.35, 0.6);
    let mut metas = Vec::new();
    for i in 0..count {
        let (w, h) = match kind {
            DatasetKind::Batch => [(1200, 900), (1000, 750), (1200, 900)][i % 3],
            _ => (rng.int(700, 1400) as u32, rng.int(600, 1100) as u32),
        };
        let clean = synth::photo_like(w, h, seed.wrapping_mul(31).wrapping_add(i as u64));
        let mut img = clean.clone();
        let mut truth = synth::Overlay::new(w, h);
        let mut meta = SampleMeta {
            file: format!("{kind:?}_{i:04}").to_lowercase(),
            kind,
            width: w,
            height: h,
            text: None,
            opacity: None,
            rotation: None,
            bbox: None,
        };
        match kind {
            DatasetKind::Batch => {
                let a = synth::render_text(batch_text, w as f32 / 260.0);
                let (x, y) = (w as i64 - a.width as i64 - w as i64 / 30, h as i64 - a.height as i64 - h as i64 / 30);
                synth::overlay(&mut img, &mut truth, &a, x, y, [255, 255, 255], batch_opacity);
                meta.text = Some(batch_text.into());
                meta.opacity = Some(batch_opacity);
                meta.rotation = Some(0.0);
            }
            DatasetKind::Single => {
                let text = WORDS[rng.int(0, WORDS.len() as i64 - 1) as usize];
                let rot = if rng.f32() < 0.3 { rng.range(-35.0, 35.0) } else { 0.0 };
                let a = synth::rotate(&synth::render_text(text, rng.range(2.0, 4.0)), rot);
                let corner = rng.int(0, 3);
                let m = 20i64;
                let (x, y) = match corner {
                    0 => (m, m),
                    1 => (w as i64 - a.width as i64 - m, m),
                    2 => (m, h as i64 - a.height as i64 - m),
                    _ => (w as i64 - a.width as i64 - m, h as i64 - a.height as i64 - m),
                };
                let op = rng.range(0.35, 0.75);
                let color = if rng.f32() < 0.8 { [255, 255, 255] } else { [20, 20, 20] };
                synth::overlay(&mut img, &mut truth, &a, x, y, color, op);
                meta.text = Some(text.into());
                meta.opacity = Some(op);
                meta.rotation = Some(rot);
            }
            DatasetKind::Repeated => {
                let text = WORDS[rng.int(0, WORDS.len() as i64 - 1) as usize];
                let rot = rng.range(-40.0, -20.0);
                let a = synth::rotate(&synth::render_text(text, rng.range(2.0, 3.0)), rot);
                let (sx, sy) = (a.width as i64 + rng.int(40, 90), a.height as i64 + rng.int(40, 90));
                let op = rng.range(0.25, 0.45);
                let mut y = -(a.height as i64) / 2;
                let mut row = 0;
                while y < h as i64 {
                    let mut x = -(a.width as i64) / 3 + (row % 2) * sx / 2;
                    while x < w as i64 {
                        synth::overlay(&mut img, &mut truth, &a, x, y, [255, 255, 255], op);
                        x += sx;
                    }
                    y += sy;
                    row += 1;
                }
                meta.text = Some(text.into());
                meta.opacity = Some(op);
                meta.rotation = Some(rot);
            }
            DatasetKind::Negatives => {
                // 画面中央的不透明“招牌”文字：正常内容，不应被当作水印自动去除
                let text = ["OPEN", "CAFE 24H", "EXIT", "SALE 50"][rng.int(0, 3) as usize];
                let a = synth::render_text(text, rng.range(5.0, 9.0));
                let (x, y) = ((w as i64 - a.width as i64) / 2, (h as i64 - a.height as i64) / 2);
                let mut plate = synth::Overlay::new(w, h);
                let board = wm_core::GrayF32 {
                    width: a.width + 40,
                    height: a.height + 30,
                    data: vec![1.0; ((a.width + 40) * (a.height + 30)) as usize],
                };
                synth::overlay(&mut img, &mut plate, &board, x - 20, y - 15, [200, 30, 40], 1.0);
                synth::overlay(&mut img, &mut plate, &a, x, y, [250, 250, 240], 1.0);
            }
        }
        if let Some(b) = truth.mask.nonzero_bounds() {
            meta.bbox = Some(b.to_bbox().to_normalized(w, h));
        }
        save_rgba(&clean, &out.join("clean").join(format!("{}.png", meta.file)))?;
        save_jpeg(&img, &out.join("watermarked").join(format!("{}.jpg", meta.file)), 94)?;
        let m = image::GrayImage::from_raw(w, h, truth.mask.data.clone())
            .ok_or_else(|| AppError::internal(wm_core::msg!("掩码尺寸无效", "Invalid mask size")))?;
        m.save(out.join("mask").join(format!("{}.png", meta.file)))
            .map_err(|e| AppError::encode(wm_core::msg!("mask 写入失败", "Failed to write mask")).with_detail(e))?;
        std::fs::write(out.join("meta").join(format!("{}.json", meta.file)), serde_json::to_vec_pretty(&meta).unwrap())?;
        metas.push(meta);
    }
    Ok(metas)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchReport {
    pub samples: usize,
    pub positives: usize,
    pub negatives: usize,
    /// 以“自动处理”决策为口径。
    pub auto_precision: f32,
    pub auto_recall: f32,
    /// 以“自动 + 复核”（即被提出为候选）为口径。
    pub candidate_recall: f32,
    pub negative_auto_false_positive_rate: f32,
    pub mean_mask_iou: f32,
    /// 并行扫描下的平均每文件耗时（含批次学习）。p50/p95 需在单 Worker 模式下测量。
    pub scan_ms_avg: u64,
    pub total_ms: u64,
    pub failures: Vec<String>,
}

/// 在数据集目录上运行 Benchmark（逐文件扫描，统计检测与 Mask 指标）。
pub fn bench(engine: &Engine, dataset: &Path) -> Result<BenchReport> {
    let wm_dir = dataset.join("watermarked");
    let mut files: Vec<PathBuf> =
        std::fs::read_dir(&wm_dir)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| wm_common::is_supported_image(p)).collect();
    files.sort();
    engine.clear_workspace()?;
    let started = std::time::Instant::now();
    engine.import(files.clone())?;
    engine.scan_blocking(None)?;
    let total_ms = started.elapsed().as_millis() as u64;
    let views = engine.files();

    let mut r = BenchReport { samples: views.len(), total_ms, ..Default::default() };
    let (mut tp_auto, mut fp_auto, mut fn_auto, mut tp_cand) = (0usize, 0usize, 0usize, 0usize);
    let mut ious = Vec::new();
    let mut neg_fp = 0usize;
    for v in &views {
        let stem = Path::new(&v.path).file_stem().unwrap().to_string_lossy().to_string();
        let meta: SampleMeta = serde_json::from_slice(&std::fs::read(dataset.join("meta").join(format!("{stem}.json")))?)
            .map_err(|e| AppError::internal(wm_core::msg!("样本元数据无效", "Invalid sample metadata")).with_detail(e))?;
        let autos: Vec<_> = v.candidates.iter().filter(|c| c.decision == wm_core::CandidateDecision::Auto).collect();
        match meta.bbox {
            Some(gt) => {
                r.positives += 1;
                let hit_auto = autos.iter().any(|c| c.bbox.iou(&gt) > 0.2 || gt.containment(&c.bbox) > 0.6);
                let hit_any = v
                    .candidates
                    .iter()
                    .any(|c| c.decision != wm_core::CandidateDecision::Ignore && (c.bbox.iou(&gt) > 0.2 || gt.containment(&c.bbox) > 0.6));
                if hit_auto {
                    tp_auto += 1;
                } else {
                    fn_auto += 1;
                    r.failures.push(format!("{stem}: {}", wm_core::tr!("未自动检出", "not detected automatically")));
                }
                if hit_any {
                    tp_cand += 1;
                }
                fp_auto += autos.iter().filter(|c| !(c.bbox.iou(&gt) > 0.2 || gt.containment(&c.bbox) > 0.6)).count();
                // Mask IoU（全分辨率 GT vs 候选 Mask，按预览尺寸比较）
                if let Some(iou) = mask_iou_for(engine, &v.id, &dataset.join("mask").join(format!("{stem}.png"))) {
                    ious.push(iou);
                }
            }
            None => {
                r.negatives += 1;
                if !autos.is_empty() {
                    neg_fp += 1;
                    r.failures
                        .push(format!("{stem}: {}", wm_core::tr!("负样本被自动判定为水印", "negative sample auto-flagged as watermark")));
                }
                fp_auto += autos.len();
            }
        }
    }
    r.auto_precision = if tp_auto + fp_auto == 0 { 1.0 } else { tp_auto as f32 / (tp_auto + fp_auto) as f32 };
    r.auto_recall = if tp_auto + fn_auto == 0 { 1.0 } else { tp_auto as f32 / (tp_auto + fn_auto) as f32 };
    r.candidate_recall = if r.positives == 0 { 1.0 } else { tp_cand as f32 / r.positives as f32 };
    r.negative_auto_false_positive_rate = if r.negatives == 0 { 0.0 } else { neg_fp as f32 / r.negatives as f32 };
    r.mean_mask_iou = if ious.is_empty() { 0.0 } else { ious.iter().sum::<f32>() / ious.len() as f32 };
    r.scan_ms_avg = (total_ms as f64 / views.len().max(1) as f64) as u64;
    Ok(r)
}

fn mask_iou_for(engine: &Engine, file_id: &str, gt_path: &Path) -> Option<f32> {
    let gt = image::open(gt_path).ok()?.to_luma8();
    let mask = engine.debug_mask(file_id)?;
    let (w, h) = gt.dimensions();
    if (mask.width, mask.height) != (w, h) {
        return None;
    }
    let pred = mask.rasterize(&wm_core::PixelRect::new(0, 0, w, h));
    let truth = wm_core::GrayU8 { width: w, height: h, data: gt.into_raw() };
    Some(wm_segmentation::mask_iou(&pred, &truth).0)
}
