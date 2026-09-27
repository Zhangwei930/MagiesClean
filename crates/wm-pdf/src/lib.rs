//! # wm-pdf
//!
//! PDF 分类与结构处理（规格 §9）：Parse、Analyze、原生水印对象检测/删除、嵌入图片提取与替换、
//! 保存事务与结构校验。
//!
//! - 原生 PDF：直接修改结构，保留文本搜索/复制、矢量和布局；**禁止默认整份栅格化**；
//! - 扫描型 / 混合：提取 Image XObject → 图片处理链 → 替换原图片；
//! - 加密：需要正确密码，**不实现密码绕过**；密码不写入日志；
//! - 数字签名：编辑会使签名失效，必须用户确认后才处理。

pub mod analyze;
pub mod fixtures;
pub mod images;
pub mod remove;

use lopdf::{Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::path::Path;
use wm_core::{msg, tr};
use wm_core::{AppError, BoundingBox, CoordSpace, DetectorSource, Evidence, Result, WatermarkCandidate, WatermarkType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfKind {
    Native,
    Scanned,
    Mixed,
}

impl PdfKind {
    pub fn msg(&self) -> wm_core::Msg {
        let (zh, en) = match self {
            PdfKind::Native => ("原生 PDF", "Native PDF"),
            PdfKind::Scanned => ("扫描型 PDF", "Scanned PDF"),
            PdfKind::Mixed => ("混合 PDF", "Mixed PDF"),
        };
        wm_core::Msg::new(zh, en)
    }

    pub fn label(&self) -> &'static str {
        match self {
            PdfKind::Native => tr!("原生 PDF", "Native PDF"),
            PdfKind::Scanned => tr!("扫描型 PDF", "Scanned PDF"),
            PdfKind::Mixed => tr!("混合 PDF", "Mixed PDF"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    Native,
    Scanned,
    Mixed,
    Empty,
}

/// 原生水印对象引用。序列化为 JSON 存入 `WatermarkCandidate.pdf_object_ref`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfObjectRef {
    /// 页序号（从 0 开始）。
    pub page: u32,
    pub kind: RefKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RefKind {
    /// `/Annots` 数组中的下标。
    Annotation { index: usize, id: Option<(u32, u16)> },
    /// 页面内容流中操作的下标范围 [start, end)。
    OpRange { start: usize, end: usize, what: String },
}

impl PdfObjectRef {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    pub fn decode(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub index: u32,
    pub width_pt: f32,
    pub height_pt: f32,
    pub rotate: i64,
    pub kind: PageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfInfo {
    pub page_count: usize,
    pub kind: PdfKind,
    pub encrypted: bool,
    pub signed: bool,
    pub pages: Vec<PageInfo>,
    /// 无法解析内容流的页（只能复核或跳过）。
    pub unreadable_pages: Vec<u32>,
}

/// 嵌入图片（扫描型/混合 PDF 的图片处理链入口）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedImage {
    pub object: (u32, u16),
    pub pages: Vec<u32>,
    pub width: i64,
    pub height: i64,
    pub filter: String,
    /// 在页面上的最大覆盖率。
    pub coverage: f32,
    /// 首次出现时在页面上的位置（pt，原点左下）与页面尺寸。
    pub placement: (f32, f32, f32, f32),
    pub page_size: (f32, f32),
    /// 该编码是否在 V1 支持范围内。
    pub supported: bool,
}

pub struct PdfDocument {
    pub doc: Document,
    pub encrypted: bool,
    /// 解密使用的用户密码：仅保存在内存中用于重新加密输出，从不写入日志或数据库。
    password: Option<String>,
    permissions: Option<lopdf::Permissions>,
}

impl std::fmt::Debug for PdfDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfDocument").field("encrypted", &self.encrypted).finish_non_exhaustive()
    }
}

/// 打开 PDF。加密文件需要正确密码（不实现密码绕过）；密码错误返回 `InvalidPassword`；
/// 权限设置禁止修改时返回 `Permission`。
pub fn open(path: &Path, password: Option<&str>) -> Result<PdfDocument> {
    let bytes = std::fs::read(path)?;
    open_bytes(&bytes, password)
}

fn has_encrypt_entry(bytes: &[u8]) -> bool {
    bytes.windows(8).any(|w| w == b"/Encrypt")
}

pub fn open_bytes(bytes: &[u8], password: Option<&str>) -> Result<PdfDocument> {
    let encrypted_file = has_encrypt_entry(bytes);
    let opts = lopdf::LoadOptions { password: Some(password.unwrap_or("").to_string()), ..Default::default() };
    let doc = match Document::load_mem_with_options(bytes, opts) {
        Ok(d) => d,
        Err(e) if encrypted_file => {
            return Err(AppError::invalid_password().with_detail(if password.is_none() {
                "password required".to_string()
            } else {
                e.to_string()
            }));
        }
        Err(e) => {
            return Err(AppError::pdf(msg!(
                "PDF 结构损坏或格式不受支持，无法打开",
                "The PDF is damaged or uses an unsupported structure and cannot be opened"
            ))
            .with_detail(e))
        }
    };
    let encrypted = doc.encryption_state.is_some();
    if encrypted_file && (!encrypted || doc.get_pages().is_empty()) {
        return Err(AppError::invalid_password());
    }
    let permissions = doc.encryption_state.as_ref().map(|s| s.permissions());
    if let Some(p) = permissions {
        if !p.contains(lopdf::Permissions::MODIFIABLE) {
            return Err(AppError::permission(msg!(
                "该 PDF 的权限设置禁止修改，未做处理",
                "This PDF's permissions forbid editing, so it was left untouched"
            )));
        }
    }
    Ok(PdfDocument { doc, encrypted, password: encrypted.then(|| password.unwrap_or("").to_string()), permissions })
}

impl PdfDocument {
    /// 保存前准备加密：原文件加密时，以同一用户密码、原权限位重新加密（AES-128），
    /// 所有者密码随机生成，不扩大任何权限。
    pub(crate) fn prepare_encryption(&mut self) -> Result<Option<wm_core::Msg>> {
        self.doc.encryption_state = None;
        let Some(pw) = self.password.clone() else { return Ok(None) };
        let owner = wm_common::new_id();
        let cf: std::sync::Arc<dyn lopdf::encryption::crypt_filters::CryptFilter> =
            std::sync::Arc::new(lopdf::encryption::crypt_filters::Aes128CryptFilter);
        if self.doc.trailer.get(b"ID").is_err() {
            let id = Object::String(wm_common::new_id().into_bytes(), lopdf::StringFormat::Hexadecimal);
            self.doc.trailer.set("ID", vec![id.clone(), id]);
        }
        let version = lopdf::EncryptionVersion::V4 {
            document: &self.doc,
            encrypt_metadata: true,
            crypt_filters: std::collections::BTreeMap::from([(b"StdCF".to_vec(), cf)]),
            stream_filter: b"StdCF".to_vec(),
            string_filter: b"StdCF".to_vec(),
            owner_password: &owner,
            user_password: &pw,
            permissions: self.permissions.unwrap_or(lopdf::Permissions::all()),
        };
        let state = lopdf::EncryptionState::try_from(version)
            .map_err(|e| AppError::pdf(msg!("无法重新加密输出文件", "Could not re-encrypt the output file")).with_detail(e))?;
        self.doc
            .encrypt(&state)
            .map_err(|e| AppError::pdf(msg!("无法重新加密输出文件", "Could not re-encrypt the output file")).with_detail(e))?;
        Ok(Some(msg!(
            "输出文件已使用原打开密码重新加密（AES-128），权限设置保持不变",
            "The output was re-encrypted with the original open password (AES-128); permissions are unchanged"
        )))
    }

    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }
}

/// 是否包含数字签名。
pub fn is_signed(doc: &Document) -> bool {
    let Ok(cat) = doc.catalog() else { return false };
    let Some(acro) = cat.get(b"AcroForm").ok().and_then(|o| doc.dereference(o).ok()).and_then(|(_, o)| o.as_dict().ok()) else {
        return false;
    };
    if acro.get(b"SigFlags").ok().and_then(|o| o.as_i64().ok()).is_some_and(|f| f & 1 == 1) {
        return true;
    }
    let fields = acro.get(b"Fields").ok().and_then(|o| doc.dereference(o).ok()).and_then(|(_, o)| o.as_array().ok());
    fields.is_some_and(|fs| {
        fs.iter().any(|f| {
            doc.dereference(f)
                .ok()
                .and_then(|(_, o)| o.as_dict().ok())
                .is_some_and(|d| d.get(b"FT").ok().and_then(|o| o.as_name().ok()) == Some(b"Sig") && d.get(b"V").is_ok())
        })
    })
}

pub struct Analysis {
    pub info: PdfInfo,
    pub candidates: Vec<WatermarkCandidate>,
    pub images: Vec<EmbeddedImage>,
}

/// 分析文档：分类、原生水印候选与嵌入图片清单。
pub fn analyze(pdf: &PdfDocument) -> Analysis {
    let doc = &pdf.doc;
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    let scans: Vec<analyze::PageScan> = page_ids.iter().enumerate().map(|(i, &id)| analyze::scan_page(doc, i as u32, id)).collect();
    let findings = analyze::find_native_watermarks(&scans);

    let mut candidates = Vec::new();
    for f in findings {
        let p = &scans[f.page as usize];
        let (x0, y0, _, _) = analyze::page_box(doc, p.id);
        // PDF 坐标（原点左下）→ 归一化（原点左上）
        let bx = ((f.bbox.0 - x0) / p.width).clamp(0.0, 1.0);
        let bx1 = ((f.bbox.2 - x0) / p.width).clamp(0.0, 1.0);
        let by = (1.0 - (f.bbox.3 - y0) / p.height).clamp(0.0, 1.0);
        let by1 = (1.0 - (f.bbox.1 - y0) / p.height).clamp(0.0, 1.0);
        let mut c = WatermarkCandidate::new(
            wm_common::new_id(),
            WatermarkType::PdfNative,
            f.confidence,
            BoundingBox::from_corners(bx, by, bx1, by1),
            DetectorSource::PdfNative,
        );
        c.space = CoordSpace::PdfPage { page: f.page, width_pt: p.width, height_pt: p.height };
        c.page = Some(f.page);
        c.rotation = f.rotation;
        c.opacity = f.opacity;
        c.text = f.text;
        c.pdf_object_ref = Some(f.reference.encode());
        c.evidence = f.evidence.iter().map(|(n, v)| Evidence::new(DetectorSource::PdfNative, *n, *v)).collect();
        candidates.push(c);
    }

    // 嵌入图片清单
    let mut images: std::collections::BTreeMap<(u32, u16), EmbeddedImage> = std::collections::BTreeMap::new();
    for s in &scans {
        for x in s.xobjects.iter().filter(|x| x.is_image) {
            let Some(id) = x.id else { continue };
            let area = ((x.bbox.2 - x.bbox.0) * (x.bbox.3 - x.bbox.1)).max(0.0) / (s.width * s.height).max(1.0);
            let e = images.entry((id.0, id.1)).or_insert_with(|| {
                let (filter, supported) = images::describe(doc, id);
                let (w, h) = x.image_size.unwrap_or((0, 0));
                EmbeddedImage {
                    object: (id.0, id.1),
                    pages: Vec::new(),
                    width: w,
                    height: h,
                    filter,
                    coverage: 0.0,
                    supported,
                    placement: x.bbox,
                    page_size: (s.width, s.height),
                }
            });
            if !e.pages.contains(&s.page) {
                e.pages.push(s.page);
            }
            e.coverage = e.coverage.max(area.min(1.0));
        }
    }
    // 只处理足够大的图片（小图标、Logo 由原生链判断）
    let images: Vec<EmbeddedImage> = images.into_values().filter(|i| i.coverage >= 0.15 && i.width >= 64 && i.height >= 64).collect();

    let pages: Vec<PageInfo> =
        scans.iter().map(|s| PageInfo { index: s.page, width_pt: s.width, height_pt: s.height, rotate: s.rotate, kind: s.kind }).collect();
    let n_scanned = pages.iter().filter(|p| p.kind == PageKind::Scanned).count();
    let n_content = pages.iter().filter(|p| p.kind != PageKind::Empty).count().max(1);
    let kind = if n_scanned == n_content {
        PdfKind::Scanned
    } else if n_scanned == 0 && pages.iter().all(|p| p.kind != PageKind::Mixed) {
        PdfKind::Native
    } else {
        PdfKind::Mixed
    };
    let info = PdfInfo {
        page_count: pages.len(),
        kind,
        encrypted: pdf.encrypted,
        signed: is_signed(doc),
        pages,
        unreadable_pages: scans.iter().filter(|s| !s.decode_ok).map(|s| s.page).collect(),
    };
    Analysis { info, candidates, images }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_ref_roundtrip() {
        let r = PdfObjectRef { page: 2, kind: RefKind::OpRange { start: 10, end: 11, what: "xobject:Fm0".into() } };
        assert_eq!(PdfObjectRef::decode(&r.encode()), Some(r));
    }

    #[test]
    fn native_watermark_detected_and_body_protected() {
        let bytes = fixtures::native_with_xobject_watermark(3);
        let pdf = open_bytes(&bytes, None).unwrap();
        let a = analyze(&pdf);
        assert_eq!(a.info.kind, PdfKind::Native);
        assert_eq!(a.info.page_count, 3);
        let high: Vec<_> = a.candidates.iter().filter(|c| c.confidence >= 0.85).collect();
        assert_eq!(
            high.len(),
            3,
            "one watermark per page: {:?}",
            a.candidates.iter().map(|c| (c.page, c.confidence, c.text.clone())).collect::<Vec<_>>()
        );
        // 页眉/页码/正文不应成为高置信度候选
        assert!(a.candidates.iter().all(|c| c.text.as_deref().map_or(true, |t| !t.contains("Quarterly") && !t.starts_with("Page"))));
    }

    #[test]
    fn artifact_and_annotation_watermarks() {
        let pdf = open_bytes(&fixtures::native_with_artifact_and_annotation(), None).unwrap();
        let a = analyze(&pdf);
        let kinds: Vec<_> =
            a.candidates.iter().filter_map(|c| c.pdf_object_ref.as_deref()).filter_map(PdfObjectRef::decode).map(|r| r.kind).collect();
        assert!(kinds.iter().any(|k| matches!(k, RefKind::Annotation { .. })));
        assert!(kinds.iter().any(|k| matches!(k, RefKind::OpRange { what, .. } if what == "artifact")));
    }

    #[test]
    fn scanned_pdf_is_classified_and_images_listed() {
        let pdf = open_bytes(&fixtures::scanned(2), None).unwrap();
        let a = analyze(&pdf);
        assert_eq!(a.info.kind, PdfKind::Scanned);
        assert_eq!(a.images.len(), 2);
        assert!(a.images.iter().all(|i| i.supported && i.filter == "DCTDecode"));
        assert!(a.candidates.is_empty(), "full-page scan image is not a watermark");
    }

    #[test]
    fn encrypted_requires_correct_password() {
        let bytes = fixtures::encrypted("s3cret");
        let e = open_bytes(&bytes, None).err().unwrap();
        assert_eq!(e.kind, wm_core::ErrorKind::InvalidPassword);
        let e = open_bytes(&bytes, Some("wrong")).err().unwrap();
        assert_eq!(e.kind, wm_core::ErrorKind::InvalidPassword);
        let ok = open_bytes(&bytes, Some("s3cret")).unwrap();
        assert!(ok.encrypted);
        let a = analyze(&ok);
        assert_eq!(a.info.page_count, 1);
        assert!(a.info.encrypted);
    }

    #[test]
    fn signed_pdf_is_flagged() {
        let pdf = open_bytes(&fixtures::signed(), None).unwrap();
        assert!(analyze(&pdf).info.signed);
    }
}
