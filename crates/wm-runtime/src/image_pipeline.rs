//! 图片处理链（规格 §8，图 8-1）：
//! File → Decode → EXIF Orientation → Thumbnail → Detection → Candidate Fusion →
//! Segmentation → Mask → Removal Strategy → Region Crop → Full-resolution Processing →
//! Quality Evaluation → Encode → Metadata Restore → Output。

use crate::model::{FileEntry, MediaInfo};
use crate::PipelineCtx;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wm_core::decision::{self, RemovalRoute, RouteContext, RouteDecision};
use wm_core::msg;
use wm_core::quality::QualityReport;
use wm_core::settings::MaskParams;
use wm_core::traits::{AlphaMatte, DetectionInput, MaskHint, RemovalOptions, SegmentContext};
use wm_core::{
    AppError, BoundingBox, CancellationToken, CandidateDecision, GrayF32, GrayU8, ImageBuffer, MaskRegion, PixelRect, Result,
    WatermarkCandidate, WatermarkMask, WatermarkType,
};
use wm_image::{ops, ImageFormatKind, ImageMetadata};
use wm_removal::{quality, region};
use wm_storage::CacheKind;

pub const MASK_COLOR: [u8; 3] = [255, 70, 110];

pub struct ScanOutput {
    pub info: MediaInfo,
    pub candidates: Vec<WatermarkCandidate>,
    pub hints: HashMap<String, MaskHint>,
    pub mask: WatermarkMask,
    pub thumb: PathBuf,
    pub preview: PathBuf,
    pub preview_size: (u32, u32),
    pub overlay: Option<PathBuf>,
    pub batch_matched: bool,
    pub notes: Vec<wm_core::Msg>,
}

/// 在全分辨率图上为候选生成像素 Mask。
pub fn segment_candidates(
    ctx: &PipelineCtx,
    img: &ImageBuffer,
    file_id: &str,
    candidates: &mut [WatermarkCandidate],
    hints: &HashMap<String, MaskHint>,
    cancel: &CancellationToken,
) -> Result<(WatermarkMask, Vec<wm_core::Msg>)> {
    let (w, h) = (img.width, img.height);
    let mut mask = WatermarkMask::new(wm_common::new_id(), file_id.to_string(), w, h);
    let mut notes = Vec::new();
    for c in candidates.iter_mut() {
        cancel.check()?;
        let px = c.bbox.to_pixels(w, h);
        let pad = (px.width.min(px.height) * 0.25).max(8.0);
        let rect = px.expand(pad, pad).to_pixel_rect(w, h);
        if rect.is_empty() {
            continue;
        }
        let crop = img.crop(&rect);
        let sctx = SegmentContext {
            image: &crop,
            crop: rect,
            full_width: w,
            full_height: h,
            params: &ctx.settings.mask,
            hint: hints.get(&c.id),
            quality: ctx.settings.quality_mode,
        };
        let mut region = None;
        if let Some(ai) = &ctx.ai_segmenter {
            region = ai.segment(c, &sctx).unwrap_or_else(|e| {
                tracing::warn!(code = e.code(), "ai segmenter failed, falling back");
                None
            });
        }
        if region.is_none() {
            region = ctx.segmenter.segment(c, &sctx)?;
        }
        match region {
            Some(r) => {
                c.mask_id = Some(mask.id.clone());
                mask.regions.push(r);
            }
            None => {
                // 无法生成像素 Mask：不能自动去除，转为复核（用户可手动绘制）
                if c.decision == CandidateDecision::Auto {
                    c.decision = CandidateDecision::Review;
                }
                let t = c.watermark_type.msg();
                notes.push(msg!(
                    format!("{} 未能自动生成像素 Mask，需要复核或手动绘制", t.zh),
                    format!("Could not build a pixel mask for a {}; review it or draw the mask manually", t.en.to_lowercase())
                ));
            }
        }
    }
    Ok((mask, notes))
}

/// 扫描单张图片。
pub fn scan_image(ctx: &PipelineCtx, entry: &FileEntry, cancel: &CancellationToken) -> Result<ScanOutput> {
    let decoded = wm_image::decode(&entry.path, Some(ctx.memory_budget))?;
    let img = decoded.buffer;
    cancel.check()?;
    let (thumb, _) = ops::thumbnail(&img, 256);
    let thumb_path = ctx.cache.path(CacheKind::Thumbnails, &entry.id, "jpg");
    wm_image::write_preview_jpeg(&thumb, &thumb_path, 256)?;
    let preview_path = ctx.cache.path(CacheKind::Preview, &format!("{}-orig", entry.id), "jpg");
    let preview_size = wm_image::write_preview_jpeg(&img, &preview_path, wm_image::PREVIEW_MAX_SIDE)?;

    let (det_img, _) = ops::thumbnail(&img, wm_image::DETECTION_MAX_SIDE);
    let input = DetectionInput {
        file_id: &entry.id,
        image: &det_img,
        original_width: img.width,
        original_height: img.height,
        profiles: &ctx.profiles,
        quality: ctx.settings.quality_mode,
        cancel,
    };
    let detections = ctx.detectors.detect(&input)?;
    let batch_matched = detections.iter().any(|d| d.candidate.batch_profile_id.is_some());
    let mut candidates = Vec::new();
    let mut hints = HashMap::new();
    for d in detections {
        if let Some(hn) = d.hint {
            hints.insert(d.candidate.id.clone(), hn);
        }
        candidates.push(d.candidate);
    }
    decision::apply_decisions(&mut candidates, &ctx.settings.thresholds());
    let (mask, notes) = segment_candidates(ctx, &img, &entry.id, &mut candidates, &hints, cancel)?;
    let overlay = write_overlay(ctx, &entry.id, &mask, preview_size)?;
    Ok(ScanOutput {
        info: MediaInfo::Image {
            width: img.width,
            height: img.height,
            format: decoded.info.format.label().to_string(),
            has_alpha: decoded.info.has_alpha,
        },
        candidates,
        hints,
        mask,
        thumb: thumb_path,
        preview: preview_path,
        preview_size,
        overlay,
        batch_matched,
        notes,
    })
}

/// 写 Mask 叠加层（预览尺寸 PNG）。
pub fn write_overlay(ctx: &PipelineCtx, file_id: &str, mask: &WatermarkMask, preview_size: (u32, u32)) -> Result<Option<PathBuf>> {
    if mask.is_empty() {
        return Ok(None);
    }
    let ov = wm_image::maskops::mask_overlay_rgba(mask, preview_size.0, preview_size.1, MASK_COLOR);
    let p = ctx.cache.path(CacheKind::Masks, &format!("{file_id}-v{}", mask.version), "png");
    wm_image::write_png(&ov, &p)?;
    Ok(Some(p))
}

/// 需要去除的 Mask：只包含 should_remove 的候选与手动区域。
pub fn removal_mask(entry: &FileEntry) -> Option<WatermarkMask> {
    let m = entry.mask.as_ref()?;
    let keep: Vec<&str> = entry.candidates.iter().filter(|c| c.should_remove()).map(|c| c.id.as_str()).collect();
    let mut out = WatermarkMask { regions: Vec::new(), ..m.clone() };
    for r in &m.regions {
        let manual = r.candidate_id.as_deref() == Some(wm_image::maskops::MANUAL_REGION);
        if manual || r.candidate_id.as_deref().is_some_and(|id| keep.contains(&id)) {
            out.regions.push(r.clone());
        }
    }
    (!out.is_empty()).then_some(out)
}

pub struct ImageResult {
    pub image: ImageBuffer,
    pub metadata: ImageMetadata,
    pub format: ImageFormatKind,
    pub route: RouteDecision,
    pub quality: QualityReport,
}

/// alpha 蒙版：由批次模板的 alpha 缩放到候选在原图中的位置。上采样倍数过大时降低可信度。
fn alpha_mattes(ctx: &PipelineCtx, entry: &FileEntry, w: u32, h: u32) -> (Vec<AlphaMatte>, Option<f32>, bool) {
    let mut mattes = Vec::new();
    let mut min_q: Option<f32> = None;
    let mut all_transparent = true;
    for c in entry.candidates.iter().filter(|c| c.should_remove()) {
        let profile = c.batch_profile_id.as_ref().and_then(|id| ctx.profiles.iter().find(|p| &p.id == id));
        let Some(p) = profile
            .filter(|p| c.watermark_type == WatermarkType::Transparent && !p.template.alpha.is_empty() && p.template.alpha_quality > 0.0)
        else {
            all_transparent = false;
            continue;
        };
        // 融合后的 bbox 可能是多个来源的并集：alpha 必须对齐到模板自身的匹配位置
        let matched = entry.hints.get(&c.id).map(|hn| hn.bbox).unwrap_or(c.bbox);
        let rect = matched.to_pixels(w, h).to_pixel_rect(w, h);
        if rect.is_empty() {
            continue;
        }
        let t = &p.template;
        let upscale = rect.width as f32 / t.width.max(1) as f32;
        let factor = if upscale <= 1.6 {
            1.0
        } else if upscale <= 3.0 {
            0.75
        } else {
            0.35
        };
        let q = t.alpha_quality * factor;
        let src = GrayF32 { width: t.width, height: t.height, data: t.alpha.clone() };
        let alpha = ops::resize_gray(&src, rect.width, rect.height);
        min_q = Some(min_q.map_or(q, |m: f32| m.min(q)));
        mattes.push(AlphaMatte { rect, alpha, color: t.color, quality: q });
    }
    let has_manual =
        entry.mask.as_ref().is_some_and(|m| m.regions.iter().any(|r| r.candidate_id.as_deref() == Some(wm_image::maskops::MANUAL_REGION)));
    (mattes, min_q, all_transparent && !has_manual)
}

fn evaluate_clusters(originals: &[(PixelRect, ImageBuffer)], img: &ImageBuffer, mask: &WatermarkMask, threshold: f32) -> QualityReport {
    let mut issues = Vec::new();
    let mut score = 1.0f32;
    let mut passed = true;
    for (rect, orig) in originals {
        let res = img.crop(rect);
        let m = mask.rasterize(rect);
        let q = quality::evaluate(orig, &res, &m, threshold);
        score = score.min(q.score);
        passed &= q.passed;
        issues.extend(q.issues);
    }
    QualityReport { score, passed, issues }
}

/// 全分辨率处理（不导出）。
pub fn process_image(ctx: &PipelineCtx, entry: &FileEntry, cancel: &CancellationToken) -> Result<ImageResult> {
    let mask = removal_mask(entry).ok_or_else(|| AppError::internal(msg!("没有需要去除的区域", "Nothing to remove")))?;
    let decoded = wm_image::decode(&entry.path, Some(ctx.memory_budget))?;
    let mut img = decoded.buffer;
    if (img.width, img.height) != (mask.width, mask.height) {
        return Err(AppError::io(msg!("文件在扫描后已改变，请重新扫描", "The file changed after scanning. Please rescan")));
    }
    let (w, h) = (img.width, img.height);
    let scale = MaskParams::scale_for(w.max(h));

    // 路由上下文
    let bounds = mask.bounds().unwrap_or(PixelRect::new(0, 0, w, h));
    let ring = (12.0 * scale).max(4.0);
    let ctx_rect = bounds.pad(ring as u32 + 2, w, h);
    let complexity = {
        let crop = img.crop(&ctx_rect);
        // 大区域降采样估计，避免对整幅大图做梯度
        let m = mask.rasterize(&ctx_rect);
        if crop.width.max(crop.height) > 1200 {
            let s = 1200.0 / crop.width.max(crop.height) as f32;
            let (cw, ch) = (((crop.width as f32 * s) as u32).max(1), ((crop.height as f32 * s) as u32).max(1));
            ops::ring_complexity(&ops::resize(&crop, cw, ch), &ops::resize_mask_nearest(&m, cw, ch), (ring * s).max(3.0))
        } else {
            ops::ring_complexity(&crop, &m, ring)
        }
    };
    let (mattes, alpha_q, all_transparent) = alpha_mattes(ctx, entry, w, h);
    let rctx = RouteContext {
        is_pdf_native: false,
        is_transparent: all_transparent && !mattes.is_empty(),
        alpha_quality: alpha_q,
        mask_area_ratio: mask.area_ratio(),
        scene_complexity: complexity,
        quality_mode: ctx.settings.quality_mode,
        ai_inpaint_available: ctx.ai_inpaint.is_some(),
        hole_thickness: hole_thickness(&mask, &bounds) / scale,
    };
    let mut route = decision::route(&rctx, &ctx.settings.router);

    // 保存待评估区域的原始像素（只保存局部，避免复制整幅大图）
    let originals: Vec<(PixelRect, ImageBuffer)> =
        region::clusters(&mask, (8.0 * scale).max(6.0) as u32).into_iter().map(|r| (r, img.crop(&r))).collect();

    let opts = RemovalOptions {
        quality: ctx.settings.quality_mode,
        alpha: mattes,
        cancel: cancel.clone(),
        unknown: entry.mask.clone().map(Arc::new),
    };
    let remover = wm_removal::remover_for(route.route, ctx.ai_inpaint.clone(), Some(ctx.ai_patches.clone()))?;
    remover.remove(&mut img, &mask, &opts)?;
    let threshold = ctx.settings.router.quality_review_threshold;
    let mut q = evaluate_clusters(&originals, &img, &mask, threshold);

    // AlphaRestore / AI 结果不理想时尝试备用路径，取质量更高者
    if !q.passed && matches!(route.route, RemovalRoute::AlphaRestore | RemovalRoute::FastInpaint) {
        let alt = if complexity >= ctx.settings.router.simple_scene_complexity || route.route == RemovalRoute::FastInpaint {
            RemovalRoute::TextureSynthesis
        } else {
            RemovalRoute::FastInpaint
        };
        let mut alt_img = img.clone();
        for (r, o) in &originals {
            alt_img.paste(o, r.x, r.y);
        }
        let alt_remover = wm_removal::remover_for(alt, None, None)?;
        alt_remover.remove(&mut alt_img, &mask, &RemovalOptions { alpha: Vec::new(), ..opts.clone() })?;
        let q2 = evaluate_clusters(&originals, &alt_img, &mask, threshold);
        if q2.score > q.score {
            let a = alt.msg();
            route = RouteDecision {
                route: alt,
                reason: msg!(
                    format!("{}；首选路径质量不足，已改用{}", route.reason.zh, a.zh),
                    format!(
                        "{}. The first attempt did not pass the quality check, so {} was used instead",
                        route.reason.en,
                        a.en.to_lowercase()
                    )
                ),
            };
            img = alt_img;
            q = q2;
        }
    }
    Ok(ImageResult { image: img, metadata: decoded.metadata, format: decoded.info.format, route, quality: q })
}

/// 洞的厚度：Mask 内像素到最近背景的最大距离（像素）。
fn hole_thickness(mask: &WatermarkMask, bounds: &PixelRect) -> f32 {
    let m = mask.rasterize(&bounds.pad(1, mask.width, mask.height));
    let known = GrayU8 { width: m.width, height: m.height, data: m.data.iter().map(|&v| if v > 0 { 0 } else { 255 }).collect() };
    ops::distance_to_nonzero(&known).data.iter().fold(0.0f32, |a, &v| a.max(v))
}

/// 预览结果仍然有效（Mask 与候选决定都没变）时直接复用，导出不再重复推理。
/// 只读取原图的元数据与格式；结果像素来自缓存的无损 PNG。
pub fn load_cached_result(ctx: &PipelineCtx, entry: &FileEntry) -> Result<Option<ImageResult>> {
    if !entry.result_valid() {
        return Ok(None);
    }
    let (Some(path), Some(route), Some(quality)) = (&entry.result_file, &entry.route, &entry.quality) else {
        return Ok(None);
    };
    let cached = wm_image::decode(path, Some(ctx.memory_budget))?;
    let original = wm_image::decode(&entry.path, Some(ctx.memory_budget))?;
    if (cached.buffer.width, cached.buffer.height) != (original.buffer.width, original.buffer.height) {
        return Ok(None);
    }
    Ok(Some(ImageResult {
        image: cached.buffer,
        metadata: original.metadata,
        format: original.info.format,
        route: route.clone(),
        quality: quality.clone(),
    }))
}

/// 缓存处理结果（无损 PNG + 预览 JPEG）。
pub fn cache_result(ctx: &PipelineCtx, entry_id: &str, version: u32, img: &ImageBuffer) -> Result<(PathBuf, PathBuf)> {
    let full = ctx.cache.path(CacheKind::Results, &format!("{entry_id}-{version:x}"), "png");
    wm_image::write_png(img, &full)?;
    let prev = ctx.cache.path(CacheKind::Preview, &format!("{entry_id}-res-{version:x}"), "jpg");
    wm_image::write_preview_jpeg(img, &prev, wm_image::PREVIEW_MAX_SIDE)?;
    Ok((full, prev))
}

/// 导出（输出事务）：编码 → 临时文件 → 重新打开校验 → 原子提交。
pub fn export_image(
    ctx: &PipelineCtx,
    entry: &FileEntry,
    img: &ImageBuffer,
    metadata: &ImageMetadata,
    input_format: ImageFormatKind,
) -> Result<Option<PathBuf>> {
    let out = &ctx.settings.output;
    let fmt = ImageFormatKind::from_output(out.format, input_format);
    let q = out.jpeg_quality.fixed_value().unwrap_or_else(|| metadata.jpeg_quality.unwrap_or(95).clamp(60, 100));
    let color = if metadata.color.is_gray() && !wm_image::codec::is_grayscale(img) { None } else { Some(metadata.color) };
    let enc = wm_image::encode(
        img,
        &wm_image::EncodeOptions { format: fmt, jpeg_quality: q, metadata: out.keep_metadata.then_some(metadata), color },
    )?;
    let plan = wm_image::output::plan_output_path(&entry.path, entry.import_root.as_deref(), out, fmt.extension())?;
    let (dest, replace) = match plan {
        wm_image::output::PlannedOutput::Write { path, replace } => (path, replace),
        wm_image::output::PlannedOutput::Skip { .. } => return Ok(None),
    };
    let (w, h) = (img.width, img.height);
    let committed = wm_image::output::commit_atomic(&dest, &enc.bytes, replace, |tmp: &Path| {
        let info = wm_image::probe(tmp)?;
        if (info.width, info.height) != (w, h) {
            return Err(AppError::encode(msg!(
                "导出校验失败：输出尺寸与原图不一致",
                "Export check failed: output size differs from the original"
            )));
        }
        Ok(())
    })?;
    Ok(Some(committed))
}

/// 用单个候选区域构造模板，用于“应用到相似文件”。
pub fn profile_from_candidate(
    img: &ImageBuffer,
    c: &WatermarkCandidate,
    mask_region: Option<&MaskRegion>,
) -> Option<wm_core::batch::BatchWatermarkProfile> {
    let detail = 1024u32.min(img.width.max(img.height));
    let (canvas, _) = ops::thumbnail(img, detail);
    let (cw, ch) = (canvas.width, canvas.height);
    let r = c.bbox.to_pixels(cw, ch).to_pixel_rect(cw, ch);
    if r.width < 6 || r.height < 6 {
        return None;
    }
    let (gx, gy) = ops::sobel(&canvas.to_luma_f32());
    let take = |g: &GrayF32| -> Vec<f32> {
        (r.y..r.bottom()).flat_map(|y| (r.x..r.right()).map(move |x| (x, y))).map(|(x, y)| g.get(x, y)).collect()
    };
    let support: Vec<f32> = match mask_region {
        Some(mr) => {
            let full = WatermarkMask {
                id: String::new(),
                file_id: String::new(),
                width: img.width,
                height: img.height,
                space: wm_core::CoordSpace::Normalized,
                version: 1,
                regions: vec![mr.clone()],
            };
            let bb = c.bbox.to_pixels(img.width, img.height).to_pixel_rect(img.width, img.height);
            let m = full.rasterize(&bb);
            let small = ops::resize_mask_bilinear(&m, r.width, r.height);
            small.data.iter().map(|&v| v as f32 / 255.0).collect()
        }
        None => vec![0.5; (r.width * r.height) as usize],
    };
    let anchor = {
        let ce = c.bbox.center();
        wm_core::batch::Anchor::from_center(ce.x, ce.y)
    };
    Some(wm_core::batch::BatchWatermarkProfile {
        id: wm_common::new_id(),
        normalized_bbox: c.bbox,
        template_hash: String::new(),
        feature_embedding: Vec::new(),
        confidence: c.confidence.max(0.9),
        sample_count: 1,
        group_key: "similar".into(),
        anchor,
        scale_mode: wm_core::batch::ScaleMode::Relative,
        watermark_type: c.watermark_type,
        template: wm_core::batch::ProfileTemplate {
            width: r.width,
            height: r.height,
            canvas_long_side: detail,
            grad_x: take(&gx),
            grad_y: take(&gy),
            support,
            alpha: Vec::new(),
            color: [255.0; 3],
            alpha_quality: 0.0,
        },
    })
}

/// 合并同一位置多个候选 bbox（归一化）。
pub fn union_bbox(cs: &[&WatermarkCandidate]) -> Option<BoundingBox> {
    cs.iter().map(|c| c.bbox).reduce(|a, b| a.union(&b))
}
