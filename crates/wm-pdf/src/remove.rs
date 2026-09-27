//! 删除策略与保存事务（规格 §9.2–§9.4）。
//!
//! 删除优先级：Remove Annotation → 修改 Content Stream 操作 → 移除重复 XObject 调用 →
//! 替换嵌入图片 →（栅格降级需用户明确选择，V1 未提供）。
//! 优先移除目标页面中的调用；只有确认不再使用时才删除共享资源。
//! 保存后重新打开，校验页数、页面尺寸、旋转、内容流可解析与正文文本保留。

use crate::analyze::page_box;
use crate::{PdfDocument, PdfObjectRef, RefKind};
use lopdf::content::Content;
use lopdf::{Document, Object, ObjectId, Stream};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use wm_core::msg;
use wm_core::quality::{QualityIssue, QualityIssueKind, QualityReport};
use wm_core::{AppError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveReport {
    pub removed_annotations: usize,
    pub removed_ops: usize,
    pub replaced_images: usize,
    pub pruned_objects: usize,
    pub quality: QualityReport,
    pub notes: Vec<wm_core::Msg>,
}

/// 按引用移除原生水印对象（原地修改文档）。返回（移除的注释数, 移除的操作数, 被移除的文字块原文）。
pub fn remove_native(pdf: &mut PdfDocument, refs: &[PdfObjectRef]) -> Result<(usize, usize, Vec<Vec<u8>>)> {
    let doc = &mut pdf.doc;
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    let mut by_page: BTreeMap<u32, Vec<&PdfObjectRef>> = BTreeMap::new();
    for r in refs {
        by_page.entry(r.page).or_default().push(r);
    }
    let (mut n_annot, mut n_ops) = (0usize, 0usize);
    let mut removed_text = Vec::new();
    for (page, rs) in by_page {
        let Some(&page_id) = page_ids.get(page as usize) else {
            return Err(AppError::pdf(msg!(format!("第 {} 页不存在", page + 1), format!("Page {} does not exist", page + 1))));
        };
        // 注释：按下标从大到小删除，避免下标漂移
        let mut annot_idx: Vec<usize> =
            rs.iter().filter_map(|r| if let RefKind::Annotation { index, .. } = r.kind { Some(index) } else { None }).collect();
        annot_idx.sort_unstable_by(|a, b| b.cmp(a));
        annot_idx.dedup();
        if !annot_idx.is_empty() {
            n_annot += remove_annotations(doc, page_id, &annot_idx)?;
        }
        // 内容流操作
        let mut ranges: Vec<(usize, usize)> =
            rs.iter().filter_map(|r| if let RefKind::OpRange { start, end, .. } = r.kind { Some((start, end)) } else { None }).collect();
        if ranges.is_empty() {
            continue;
        }
        ranges.sort_unstable();
        let mut content = doc.get_and_decode_page_content(page_id).map_err(|e| {
            AppError::pdf(msg!(
                format!("第 {} 页内容无法安全解析", page + 1),
                format!("The content of page {} cannot be parsed safely", page + 1)
            ))
            .with_detail(e)
        })?;
        let len = content.operations.len();
        let mut drop = vec![false; len];
        for (s, e) in &ranges {
            if *s >= *e || *e > len {
                return Err(AppError::pdf(msg!(
                    "水印对象位置已失效（文件可能在分析后被修改），请重新扫描",
                    "Watermark object references are stale (the file may have changed after analysis). Please rescan"
                )));
            }
            for d in drop.iter_mut().take(*e).skip(*s) {
                *d = true;
            }
        }
        // 记录被移除的文字（用于正文保留校验）
        for (s, e) in &ranges {
            for op in &content.operations[*s..*e] {
                if matches!(op.operator.as_str(), "Tj" | "TJ" | "'" | "\"") {
                    for o in &op.operands {
                        collect_strings(o, &mut removed_text);
                    }
                }
            }
        }
        n_ops += drop.iter().filter(|&&d| d).count();
        let ops = std::mem::take(&mut content.operations);
        content.operations = ops.into_iter().zip(drop).filter(|(_, d)| !d).map(|(o, _)| o).collect();
        let bytes = Content { operations: content.operations }
            .encode()
            .map_err(|e| AppError::pdf(msg!("内容流编码失败", "Failed to encode the content stream")).with_detail(e))?;
        let new_id = doc.add_object(Stream::new(lopdf::Dictionary::new(), bytes));
        let page_dict = doc
            .get_object_mut(page_id)
            .and_then(|o| o.as_dict_mut())
            .map_err(|e| AppError::pdf(msg!("页面对象损坏", "The page object is damaged")).with_detail(e))?;
        page_dict.set("Contents", Object::Reference(new_id));
    }
    prune_unused_xobject_names(doc);
    Ok((n_annot, n_ops, removed_text))
}

fn collect_strings(o: &Object, out: &mut Vec<Vec<u8>>) {
    match o {
        Object::String(s, _) => out.push(s.clone()),
        Object::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

fn remove_annotations(doc: &mut Document, page_id: ObjectId, idx_desc: &[usize]) -> Result<usize> {
    let annots = doc.get_dictionary(page_id).ok().and_then(|d| d.get(b"Annots").ok()).cloned();
    let Some(annots) = annots else { return Ok(0) };
    let mut n = 0;
    match annots {
        Object::Reference(r) => {
            if let Ok(Object::Array(a)) = doc.get_object_mut(r) {
                for &i in idx_desc {
                    if i < a.len() {
                        a.remove(i);
                        n += 1;
                    }
                }
            }
        }
        Object::Array(mut a) => {
            for &i in idx_desc {
                if i < a.len() {
                    a.remove(i);
                    n += 1;
                }
            }
            if let Ok(d) = doc.get_object_mut(page_id).and_then(|o| o.as_dict_mut()) {
                d.set("Annots", Object::Array(a));
            }
        }
        _ => {}
    }
    Ok(n)
}

/// 清理资源字典中不再被任何使用者引用的 XObject 名称（共享资源只在所有使用页都不再使用时删除）。
fn prune_unused_xobject_names(doc: &mut Document) {
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    // 资源字典（按引用 id 或页面内联）→ 使用它的页面
    let mut users: HashMap<Option<ObjectId>, Vec<ObjectId>> = HashMap::new();
    for &pid in &page_ids {
        let r = doc.get_dictionary(pid).ok().and_then(|d| d.get(b"Resources").ok()).cloned();
        match r {
            Some(Object::Reference(rid)) => users.entry(Some(rid)).or_default().push(pid),
            Some(Object::Dictionary(_)) => users.entry(None).or_default().push(pid),
            _ => {} // 继承的资源：不修改
        }
    }
    let used_names = |doc: &Document, pid: ObjectId| -> HashSet<Vec<u8>> {
        doc.get_and_decode_page_content(pid)
            .map(|c| {
                c.operations
                    .iter()
                    .filter(|o| o.operator == "Do")
                    .filter_map(|o| o.operands.first().and_then(|n| n.as_name().ok()).map(|n| n.to_vec()))
                    .collect()
            })
            .unwrap_or_default()
    };
    for (rid, pages) in users {
        let names: HashSet<Vec<u8>> = pages.iter().flat_map(|&p| used_names(doc, p)).collect();
        let targets: Vec<(Option<ObjectId>, ObjectId)> = match rid {
            Some(r) => vec![(Some(r), pages[0])],
            None => pages.iter().map(|&p| (None, p)).collect(),
        };
        for (r, pid) in targets {
            let own_names: HashSet<Vec<u8>> = if r.is_some() { names.clone() } else { used_names(doc, pid) };
            let res: Option<&mut lopdf::Dictionary> = match r {
                Some(rid) => doc.get_object_mut(rid).ok().and_then(|o| o.as_dict_mut().ok()),
                None => doc
                    .get_object_mut(pid)
                    .ok()
                    .and_then(|o| o.as_dict_mut().ok())
                    .and_then(|d| d.get_mut(b"Resources").ok())
                    .and_then(|o| o.as_dict_mut().ok()),
            };
            let Some(res) = res else { continue };
            // 只处理内联的 XObject 字典，引用的 XObject 字典可能被其它资源共享
            if let Ok(Object::Dictionary(x)) = res.get_mut(b"XObject") {
                let unused: Vec<Vec<u8>> = x.iter().map(|(k, _)| k.clone()).filter(|k| !own_names.contains(k)).collect();
                for k in unused {
                    // 仍被 Form XObject 内部引用的名称无法在此判断：只删除页面级从未调用的名称
                    x.remove(&k);
                }
            }
        }
    }
}

/// 保存事务：清理未引用对象 → 序列化 → 重新打开校验。
pub fn save_validated(
    pdf: &mut PdfDocument,
    original: &Document,
    removed_text: &[Vec<u8>],
    mut notes: Vec<wm_core::Msg>,
) -> Result<(Vec<u8>, SaveReport)> {
    let pruned = pdf.doc.prune_objects().len();
    pdf.doc.compress();
    if let Some(n) = pdf.prepare_encryption()? {
        notes.push(n);
    }
    let mut bytes = Vec::new();
    pdf.doc.save_to(&mut bytes).map_err(|e| AppError::pdf(msg!("PDF 保存失败", "Failed to save the PDF")).with_detail(e))?;
    let password = pdf.password().map(str::to_string);
    let quality = validate(original, &bytes, removed_text, password.as_deref())?;
    let report = SaveReport { removed_annotations: 0, removed_ops: 0, replaced_images: 0, pruned_objects: pruned, quality, notes };
    Ok((bytes, report))
}

/// 结构校验（规格 §9.4）：页数、页面尺寸、旋转与顺序；内容流可解析；正文文本保留。
pub fn validate(original: &Document, saved: &[u8], removed_text: &[Vec<u8>], password: Option<&str>) -> Result<QualityReport> {
    let after = crate::open_bytes(saved, password)
        .map_err(|e| AppError::pdf(msg!("保存后的 PDF 无法重新打开", "The saved PDF could not be reopened")).with_detail(e))?
        .doc;
    let mut issues = Vec::new();
    let a_pages: Vec<ObjectId> = original.get_pages().values().copied().collect();
    let b_pages: Vec<ObjectId> = after.get_pages().values().copied().collect();
    if a_pages.len() != b_pages.len() {
        issues.push(QualityIssue {
            kind: QualityIssueKind::Structure,
            severity: 1.0,
            message: msg!(
                format!("页数变化：{} → {}", a_pages.len(), b_pages.len()),
                format!("Page count changed: {} → {}", a_pages.len(), b_pages.len())
            ),
        });
    }
    for (i, (&pa, &pb)) in a_pages.iter().zip(&b_pages).enumerate() {
        let (ba, bb) = (page_box(original, pa), page_box(&after, pb));
        if (ba.2 - ba.0 - (bb.2 - bb.0)).abs() > 0.5 || (ba.3 - ba.1 - (bb.3 - bb.1)).abs() > 0.5 {
            issues.push(QualityIssue {
                kind: QualityIssueKind::Structure,
                severity: 1.0,
                message: msg!(format!("第 {} 页尺寸变化", i + 1), format!("Page {} changed size", i + 1)),
            });
        }
        let rot = |d: &Document, p| d.get_dictionary(p).ok().and_then(|x| x.get(b"Rotate").ok()).and_then(|o| o.as_i64().ok()).unwrap_or(0);
        if rot(original, pa) != rot(&after, pb) {
            issues.push(QualityIssue {
                kind: QualityIssueKind::Structure,
                severity: 1.0,
                message: msg!(format!("第 {} 页旋转变化", i + 1), format!("Page {} changed rotation", i + 1)),
            });
        }
        if after.get_and_decode_page_content(pb).is_err() {
            issues.push(QualityIssue {
                kind: QualityIssueKind::Structure,
                severity: 1.0,
                message: msg!(format!("第 {} 页内容无法解析", i + 1), format!("The content of page {} cannot be parsed", i + 1)),
            });
        }
    }
    // 正文保留：原文中除被移除文字外的词，应在输出中仍然存在
    let removed: HashSet<String> = removed_text.iter().flat_map(|t| words(&String::from_utf8_lossy(t)).collect::<Vec<_>>()).collect();
    let pages_a: Vec<u32> = original.get_pages().keys().copied().collect();
    let text_a = original.extract_text(&pages_a).unwrap_or_default();
    let pages_b: Vec<u32> = after.get_pages().keys().copied().collect();
    let text_b = after.extract_text(&pages_b).unwrap_or_default();
    let before: BTreeSet<String> = words(&text_a).filter(|w| !removed.contains(w)).collect();
    let after_w: HashSet<String> = words(&text_b).collect();
    if !before.is_empty() {
        let kept = before.iter().filter(|w| after_w.contains(*w)).count();
        let ratio = kept as f32 / before.len() as f32;
        if ratio < 0.98 {
            issues.push(QualityIssue {
                kind: QualityIssueKind::Structure,
                severity: 1.0 - ratio,
                message: msg!(
                    format!("正文文字保留率 {:.0}%，低于预期", ratio * 100.0),
                    format!("Only {:.0}% of body text was preserved, lower than expected", ratio * 100.0)
                ),
            });
        }
    }
    Ok(QualityReport::from_issues(issues, 0.6))
}

fn words(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 2).map(|w| w.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze, fixtures, open_bytes, PdfKind};

    #[test]
    fn removes_xobject_watermark_and_keeps_body_text() {
        let bytes = fixtures::native_with_xobject_watermark(3);
        let mut pdf = open_bytes(&bytes, None).unwrap();
        let original = pdf.doc.clone();
        let a = analyze(&pdf);
        let refs: Vec<PdfObjectRef> = a
            .candidates
            .iter()
            .filter(|c| c.confidence >= 0.85)
            .filter_map(|c| c.pdf_object_ref.as_deref())
            .filter_map(PdfObjectRef::decode)
            .collect();
        assert_eq!(refs.len(), 3);
        let (_, n_ops, removed) = remove_native(&mut pdf, &refs).unwrap();
        assert_eq!(n_ops, 3);
        let (out, report) = save_validated(&mut pdf, &original, &removed, vec![]).unwrap();
        assert!(report.quality.passed, "{:?}", report.quality);
        // 输出中不再有高置信度水印，正文仍可提取
        let re = open_bytes(&out, None).unwrap();
        let a2 = analyze(&re);
        assert_eq!(a2.info.kind, PdfKind::Native);
        assert!(a2.candidates.iter().all(|c| c.confidence < 0.85));
        let text = re.doc.extract_text(&[1, 2, 3]).unwrap();
        assert!(text.contains("Quarterly"), "body text lost: {text}");
        // 共享的水印 Form XObject 已不再被引用并被清理
        assert!(report.pruned_objects >= 1);
    }

    #[test]
    fn removes_artifact_and_annotation() {
        let bytes = fixtures::native_with_artifact_and_annotation();
        let mut pdf = open_bytes(&bytes, None).unwrap();
        let original = pdf.doc.clone();
        let a = analyze(&pdf);
        let refs: Vec<PdfObjectRef> =
            a.candidates.iter().filter_map(|c| c.pdf_object_ref.as_deref()).filter_map(PdfObjectRef::decode).collect();
        let (n_annot, n_ops, removed) = remove_native(&mut pdf, &refs).unwrap();
        assert_eq!(n_annot, 1);
        assert!(n_ops >= 3);
        let (out, report) = save_validated(&mut pdf, &original, &removed, vec![]).unwrap();
        assert!(report.quality.passed, "{:?}", report.quality);
        let a2 = analyze(&open_bytes(&out, None).unwrap());
        assert!(a2.candidates.is_empty());
    }

    #[test]
    fn encrypted_output_keeps_password_protection() {
        let bytes = fixtures::encrypted("s3cret");
        let mut pdf = open_bytes(&bytes, Some("s3cret")).unwrap();
        let original = pdf.doc.clone();
        let (out, report) = save_validated(&mut pdf, &original, &[], vec![]).unwrap();
        assert!(report.quality.passed, "{:?}", report.quality);
        assert!(report.notes.iter().any(|n| n.contains("重新加密")));
        assert_eq!(open_bytes(&out, None).err().unwrap().kind, wm_core::ErrorKind::InvalidPassword);
        let re = open_bytes(&out, Some("s3cret")).unwrap();
        assert!(re.doc.extract_text(&[1]).unwrap().contains("Encrypted"));
    }

    #[test]
    fn stale_reference_is_rejected() {
        let mut pdf = open_bytes(&fixtures::native_with_xobject_watermark(1), None).unwrap();
        let bad = PdfObjectRef { page: 0, kind: RefKind::OpRange { start: 9999, end: 10000, what: "x".into() } };
        assert!(remove_native(&mut pdf, &[bad]).is_err());
    }
}
