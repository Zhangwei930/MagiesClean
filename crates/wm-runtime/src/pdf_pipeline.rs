//! PDF 处理链（规格 §9，图 9-1）：
//! PDF Analyzer → 原生水印对象（Object Analysis / Remove）+ 嵌入图片内水印
//! （Image Detection / Mask / Repair）→ 保留其余对象与布局 → Validate PDF → Save as new PDF。

use crate::image_pipeline::segment_candidates;
use crate::model::{FileEntry, MediaInfo, PdfImageWork};
use crate::PipelineCtx;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use wm_core::decision::{self, RemovalRoute, RouteDecision};
use wm_core::msg;
use wm_core::quality::QualityReport;
use wm_core::traits::{DetectionInput, RemovalOptions};
use wm_core::{AppError, BoundingBox, CancellationToken, CoordSpace, ErrorKind, Result, WatermarkCandidate, WatermarkMask};
use wm_image::ops;
use wm_pdf::PdfObjectRef;
use wm_storage::CacheKind;

pub struct PdfScanOutput {
    pub info: MediaInfo,
    pub candidates: Vec<WatermarkCandidate>,
    pub images: Vec<PdfImageWork>,
    pub notes: Vec<wm_core::Msg>,
}

pub fn image_ref(obj: (u32, u16)) -> String {
    format!("image:{}:{}", obj.0, obj.1)
}

fn parse_image_ref(s: &str) -> Option<(u32, u16)> {
    let mut it = s.strip_prefix("image:")?.split(':');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// 扫描 PDF。需要密码时返回 `InvalidPassword`（由调用方提示用户输入）。
pub fn scan_pdf(ctx: &PipelineCtx, entry: &FileEntry, cancel: &CancellationToken) -> Result<PdfScanOutput> {
    let pdf = wm_pdf::open(&entry.path, entry.pdf_password.as_deref())?;
    let analysis = wm_pdf::analyze(&pdf);
    let mut notes = Vec::new();
    let mut candidates = analysis.candidates;
    decision::apply_decisions(&mut candidates, &ctx.settings.thresholds());

    if analysis.info.signed {
        notes.push(msg!(
            "该 PDF 含数字签名：修改会使签名失效，处理前需要确认",
            "This PDF is digitally signed: editing will invalidate the signature, so confirmation is required"
        ));
    }
    if analysis.info.encrypted {
        notes.push(msg!(
            "该 PDF 已加密：输出将使用相同打开密码重新加密",
            "This PDF is encrypted: the output will be re-encrypted with the same open password"
        ));
    }
    if !analysis.info.unreadable_pages.is_empty() {
        notes.push(msg!(
            format!("{} 页内容无法解析，这些页不会被修改", analysis.info.unreadable_pages.len()),
            format!("{} page(s) could not be parsed and will not be modified", analysis.info.unreadable_pages.len())
        ));
    }

    // 嵌入图片链
    let mut images = Vec::new();
    for ei in &analysis.images {
        cancel.check()?;
        if !ei.supported {
            notes.push(msg!(
                format!("第 {} 页的嵌入图片编码（{}）暂不支持处理", ei.pages[0] + 1, ei.filter),
                format!("The embedded image encoding on page {} ({}) is not supported yet", ei.pages[0] + 1, ei.filter)
            ));
            continue;
        }
        let (img, _) = match wm_pdf::images::extract(&pdf.doc, ei.object) {
            Ok(x) => x,
            Err(e) => {
                notes.push(msg!(
                    format!("第 {} 页嵌入图片无法读取：{}", ei.pages[0] + 1, e.message.zh),
                    format!("Could not read the embedded image on page {}: {}", ei.pages[0] + 1, e.message.en)
                ));
                continue;
            }
        };
        let (det, _) = ops::thumbnail(&img, wm_image::DETECTION_MAX_SIDE);
        let input = DetectionInput {
            file_id: &entry.id,
            image: &det,
            original_width: img.width,
            original_height: img.height,
            profiles: &ctx.profiles,
            quality: ctx.settings.quality_mode,
            cancel,
        };
        let dets = ctx.detectors.detect(&input)?;
        let mut cands = Vec::new();
        let mut hints = HashMap::new();
        for d in dets {
            if let Some(h) = d.hint {
                hints.insert(d.candidate.id.clone(), h);
            }
            cands.push(d.candidate);
        }
        if cands.is_empty() {
            continue;
        }
        decision::apply_decisions(&mut cands, &ctx.settings.thresholds());
        let (mask, n) = segment_candidates(ctx, &img, &entry.id, &mut cands, &hints, cancel)?;
        notes.extend(n);
        let page = ei.pages[0];
        let (x0, y0, x1, y1) = ei.placement;
        let (pw, ph) = ei.page_size;
        for mut c in cands {
            // 图片内坐标 → 页面归一化坐标（用于在页面预览上显示）
            let b = c.bbox;
            let px0 = x0 + b.x * (x1 - x0);
            let px1 = x0 + (b.x + b.width) * (x1 - x0);
            let top = y1 - b.y * (y1 - y0);
            let bottom = y1 - (b.y + b.height) * (y1 - y0);
            c.bbox = BoundingBox::from_corners(px0 / pw, 1.0 - top / ph, px1 / pw, 1.0 - bottom / ph);
            c.space = CoordSpace::PdfPage { page, width_pt: pw, height_pt: ph };
            c.page = Some(page);
            c.pdf_object_ref = Some(image_ref(ei.object));
            candidates.push(c);
        }
        images.push(PdfImageWork { object: ei.object, page, mask });
    }

    let info = MediaInfo::Pdf {
        page_count: analysis.info.page_count,
        pdf_kind: analysis.info.kind.msg(),
        encrypted: analysis.info.encrypted,
        signed: analysis.info.signed,
        pages: analysis.info.pages.iter().map(|p| (p.width_pt, p.height_pt)).collect(),
    };
    Ok(PdfScanOutput { info, candidates, images, notes })
}

pub struct PdfResult {
    pub bytes: Vec<u8>,
    pub route: RouteDecision,
    pub quality: QualityReport,
    pub notes: Vec<wm_core::Msg>,
}

pub fn process_pdf(ctx: &PipelineCtx, entry: &FileEntry, cancel: &CancellationToken) -> Result<PdfResult> {
    let mut pdf = wm_pdf::open(&entry.path, entry.pdf_password.as_deref())?;
    if wm_pdf::is_signed(&pdf.doc) && !entry.signature_confirmed {
        return Err(AppError::needs_confirmation(msg!(
            "该 PDF 含数字签名，修改会使签名失效，请确认后再处理",
            "This PDF is digitally signed and editing will invalidate the signature. Confirm before processing"
        )));
    }
    let original = pdf.doc.clone();
    let remove: Vec<&WatermarkCandidate> = entry.candidates.iter().filter(|c| c.should_remove()).collect();
    let native: Vec<PdfObjectRef> = remove.iter().filter_map(|c| c.pdf_object_ref.as_deref()).filter_map(PdfObjectRef::decode).collect();
    let (n_annot, n_ops, removed_text) = wm_pdf::remove::remove_native(&mut pdf, &native)?;
    cancel.check()?;

    // 嵌入图片
    let mut replaced = 0usize;
    let mut notes = Vec::new();
    for work in &entry.pdf_images {
        cancel.check()?;
        let ids: Vec<&str> = remove
            .iter()
            .filter(|c| c.pdf_object_ref.as_deref().and_then(parse_image_ref) == Some(work.object))
            .map(|c| c.id.as_str())
            .collect();
        if ids.is_empty() {
            continue;
        }
        let mut mask = WatermarkMask { regions: Vec::new(), ..work.mask.clone() };
        mask.regions =
            work.mask.regions.iter().filter(|r| r.candidate_id.as_deref().is_some_and(|id| ids.contains(&id))).cloned().collect();
        if mask.is_empty() {
            continue;
        }
        let (mut img, enc) = wm_pdf::images::extract(&pdf.doc, work.object)?;
        let complexity = {
            let b = mask.bounds().unwrap_or(img.full_rect());
            let r = b.pad(12, img.width, img.height);
            ops::ring_complexity(&img.crop(&r), &mask.rasterize(&r), 8.0)
        };
        let route = if complexity < ctx.settings.router.simple_scene_complexity {
            RemovalRoute::FastInpaint
        } else if ctx.ai_inpaint.is_some() {
            RemovalRoute::AiInpaint
        } else {
            RemovalRoute::TextureSynthesis
        };
        let remover = wm_removal::remover_for(route, ctx.ai_inpaint.clone(), Some(ctx.ai_patches.clone()))?;
        remover.remove(
            &mut img,
            &mask,
            &RemovalOptions { quality: ctx.settings.quality_mode, cancel: cancel.clone(), ..Default::default() },
        )?;
        wm_pdf::images::replace(&mut pdf.doc, work.object, &img, enc)?;
        replaced += 1;
    }
    if !remove.iter().any(|c| c.pdf_object_ref.is_some()) {
        return Err(AppError::internal(msg!("没有需要去除的 PDF 对象", "No PDF objects to remove")));
    }
    let (bytes, mut report) = wm_pdf::remove::save_validated(&mut pdf, &original, &removed_text, notes.clone())?;
    report.removed_annotations = n_annot;
    report.removed_ops = n_ops;
    report.replaced_images = replaced;
    notes = report.notes.clone();
    let mut parts = Vec::new();
    if n_annot + n_ops > 0 {
        parts.push(msg!(
            format!("移除 {} 个水印注释 / {} 个内容操作", n_annot, n_ops),
            format!("removed {} watermark annotation(s) / {} content operation(s)", n_annot, n_ops)
        ));
    }
    if replaced > 0 {
        parts.push(msg!(format!("修复并替换 {replaced} 张嵌入图片"), format!("repaired and replaced {replaced} embedded image(s)")));
    }
    let zh: Vec<&str> = parts.iter().map(|m: &wm_core::Msg| m.zh.as_ref()).collect();
    let en: Vec<&str> = parts.iter().map(|m| m.en.as_ref()).collect();
    let summary = if en.is_empty() { String::new() } else { en.join("; ") };
    let route = RouteDecision {
        route: if n_annot + n_ops > 0 { RemovalRoute::PdfObjectRemove } else { RemovalRoute::FastInpaint },
        reason: msg!(
            format!("{}；保留文档其余结构，未栅格化", zh.join("，")),
            format!(
                "{}{}; the rest of the document structure is kept and nothing is rasterized",
                summary[..1.min(summary.len())].to_uppercase(),
                summary.get(1..).unwrap_or("")
            )
        ),
    };

    Ok(PdfResult { bytes, route, quality: report.quality, notes })
}

pub fn cache_pdf(ctx: &PipelineCtx, entry_id: &str, version: u32, bytes: &[u8]) -> Result<PathBuf> {
    let p = ctx.cache.path(CacheKind::Pdf, &format!("{entry_id}-{version:x}"), "pdf");
    std::fs::write(&p, bytes)?;
    Ok(p)
}

pub fn export_pdf(ctx: &PipelineCtx, entry: &FileEntry, bytes: &[u8]) -> Result<Option<PathBuf>> {
    let plan = wm_image::output::plan_output_path(&entry.path, entry.import_root.as_deref(), &ctx.settings.output, "pdf")?;
    let (dest, replace) = match plan {
        wm_image::output::PlannedOutput::Write { path, replace } => (path, replace),
        wm_image::output::PlannedOutput::Skip { .. } => return Ok(None),
    };
    let pw = entry.pdf_password.clone();
    let committed = wm_image::output::commit_atomic(&dest, bytes, replace, |tmp: &Path| {
        let b = std::fs::read(tmp)?;
        wm_pdf::open_bytes(&b, pw.as_deref()).map(|_| ()).map_err(|e| {
            AppError::pdf(msg!("导出校验失败：输出 PDF 无法重新打开", "Export check failed: the output PDF could not be reopened"))
                .with_detail(e)
        })
    })?;
    Ok(Some(committed))
}

/// 需要密码的错误。
pub fn is_password_error(e: &AppError) -> bool {
    e.kind == ErrorKind::InvalidPassword
}
