//! PdfAnalyzer（规格 §9.1）：Annotation、XObject、Text Object、Image Object、Content Stream、
//! OCG、ExtGState、Transformation Matrix、Transparency 与资源使用分析。
//!
//! 优先识别 Watermark Annotation 与 `/Artifact <</Subtype /Watermark>>` 标记内容，
//! 再结合对象复用、页面位置、透明度和旋转综合判定。不因“每页出现”就判定为水印：
//! 页眉、页脚、页码与公司模板信息需要保护。

use crate::{PageKind, PdfObjectRef, RefKind};
use lopdf::content::Operation;
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashMap;

/// 2D 仿射矩阵 [a b c d e f]。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix(pub [f32; 6]);

impl Matrix {
    pub const IDENTITY: Matrix = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    /// self × other（先应用 self，再应用 other）。
    pub fn mul(&self, o: &Matrix) -> Matrix {
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = o.0;
        Matrix([a * a2 + b * c2, a * b2 + b * d2, c * a2 + d * c2, c * b2 + d * d2, e * a2 + f * c2 + e2, e * b2 + f * d2 + f2])
    }
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }
    pub fn rotation_deg(&self) -> f32 {
        self.0[1].atan2(self.0[0]).to_degrees()
    }
    pub fn scale(&self) -> f32 {
        (self.0[0] * self.0[0] + self.0[1] * self.0[1]).sqrt()
    }
    /// 变换矩形后的轴对齐包围盒 (x0, y0, x1, y1)。
    pub fn bbox(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> (f32, f32, f32, f32) {
        let pts = [self.apply(x0, y0), self.apply(x1, y0), self.apply(x0, y1), self.apply(x1, y1)];
        let xs = pts.iter().map(|p| p.0);
        let ys = pts.iter().map(|p| p.1);
        (xs.clone().fold(f32::MAX, f32::min), ys.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max), ys.fold(f32::MIN, f32::max))
    }
}

pub fn num(o: &Object) -> Option<f32> {
    match o {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

fn matrix_from(ops: &[Object]) -> Option<Matrix> {
    if ops.len() != 6 {
        return None;
    }
    let v: Vec<f32> = ops.iter().filter_map(num).collect();
    (v.len() == 6).then(|| Matrix([v[0], v[1], v[2], v[3], v[4], v[5]]))
}

#[derive(Debug, Clone, Copy)]
struct GState {
    ctm: Matrix,
    fill_alpha: f32,
    stroke_alpha: f32,
}

/// 页面上一次 XObject 调用。
#[derive(Debug, Clone)]
pub struct XObjectUse {
    pub page: u32,
    pub op_index: usize,
    pub name: Vec<u8>,
    pub id: Option<ObjectId>,
    pub is_image: bool,
    pub ctm: Matrix,
    pub alpha: f32,
    /// 页面坐标下的包围盒 (x0,y0,x1,y1)。
    pub bbox: (f32, f32, f32, f32),
    pub in_watermark_artifact: bool,
    pub image_size: Option<(i64, i64)>,
}

/// 一个 BT..ET 文字块。
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub page: u32,
    pub start: usize,
    pub end: usize,
    pub raw: Vec<u8>,
    pub font_size: f32,
    pub rotation: f32,
    pub alpha: f32,
    pub bbox: (f32, f32, f32, f32),
    pub in_watermark_artifact: bool,
    pub invisible: bool,
}

/// `/Artifact … /Watermark` 或名为 Watermark 的 OCG 的标记内容范围。
#[derive(Debug, Clone)]
pub struct ArtifactRange {
    pub page: u32,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct PageScan {
    pub page: u32,
    pub id: ObjectId,
    pub width: f32,
    pub height: f32,
    pub rotate: i64,
    pub xobjects: Vec<XObjectUse>,
    pub texts: Vec<TextBlock>,
    pub artifacts: Vec<ArtifactRange>,
    pub watermark_annots: Vec<(usize, Option<ObjectId>, (f32, f32, f32, f32))>,
    pub kind: PageKind,
    pub decode_ok: bool,
}

fn resolve<'a>(doc: &'a Document, o: &'a Object) -> Option<&'a Object> {
    doc.dereference(o).ok().map(|(_, o)| o)
}

fn dict_of<'a>(doc: &'a Document, o: &'a Object) -> Option<&'a Dictionary> {
    resolve(doc, o).and_then(|o| o.as_dict().ok())
}

/// 页面资源字典（处理继承）。
pub fn page_resources(doc: &Document, page_id: ObjectId) -> Option<Dictionary> {
    let mut cur = doc.get_dictionary(page_id).ok()?;
    loop {
        if let Ok(r) = cur.get(b"Resources") {
            return dict_of(doc, r).cloned();
        }
        let parent = cur.get(b"Parent").ok()?.as_reference().ok()?;
        cur = doc.get_dictionary(parent).ok()?;
    }
}

fn inherited<'a>(doc: &'a Document, page_id: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut cur = doc.get_dictionary(page_id).ok()?;
    loop {
        if let Ok(v) = cur.get(key) {
            return resolve(doc, v);
        }
        let parent = cur.get(b"Parent").ok()?.as_reference().ok()?;
        cur = doc.get_dictionary(parent).ok()?;
    }
}

pub fn page_box(doc: &Document, page_id: ObjectId) -> (f32, f32, f32, f32) {
    let b = inherited(doc, page_id, b"CropBox").or_else(|| inherited(doc, page_id, b"MediaBox"));
    if let Some(Object::Array(a)) = b {
        let v: Vec<f32> = a.iter().filter_map(|o| resolve(doc, o).and_then(num)).collect();
        if v.len() == 4 {
            return (v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3]));
        }
    }
    (0.0, 0.0, 612.0, 792.0)
}

fn is_watermark_props(doc: &Document, props: &Object, res: Option<&Dictionary>) -> bool {
    let d = match props {
        Object::Dictionary(d) => Some(d.clone()),
        Object::Name(n) => res
            .and_then(|r| r.get(b"Properties").ok())
            .and_then(|p| dict_of(doc, p))
            .and_then(|p| p.get(n).ok())
            .and_then(|o| dict_of(doc, o))
            .cloned(),
        _ => None,
    };
    let Some(d) = d else { return false };
    let name_is = |k: &[u8], v: &[u8]| d.get(k).ok().and_then(|o| o.as_name().ok()).is_some_and(|n| n.eq_ignore_ascii_case(v));
    if name_is(b"Subtype", b"Watermark") {
        return true;
    }
    // OCG：名称包含 watermark / 水印
    if name_is(b"Type", b"OCG") {
        if let Ok(Object::String(s, _)) = d.get(b"Name") {
            let n = String::from_utf8_lossy(s).to_lowercase();
            return n.contains("watermark") || n.contains("水印");
        }
    }
    false
}

/// 扫描一页：记录 XObject 调用、文字块、Watermark 标记内容与注释。
pub fn scan_page(doc: &Document, page: u32, page_id: ObjectId) -> PageScan {
    let (x0, y0, x1, y1) = page_box(doc, page_id);
    let (pw, ph) = (x1 - x0, y1 - y0);
    let rotate = inherited(doc, page_id, b"Rotate").and_then(|o| o.as_i64().ok()).unwrap_or(0);
    let res = page_resources(doc, page_id);
    let mut scan = PageScan {
        page,
        id: page_id,
        width: pw,
        height: ph,
        rotate,
        xobjects: Vec::new(),
        texts: Vec::new(),
        artifacts: Vec::new(),
        watermark_annots: Vec::new(),
        kind: PageKind::Empty,
        decode_ok: true,
    };

    // 注释
    if let Ok(page_dict) = doc.get_dictionary(page_id) {
        if let Ok(annots) = page_dict.get(b"Annots") {
            if let Some(Object::Array(arr)) = resolve(doc, annots) {
                for (i, a) in arr.iter().enumerate() {
                    let id = a.as_reference().ok();
                    if let Some(d) = dict_of(doc, a) {
                        let sub = d.get(b"Subtype").ok().and_then(|o| o.as_name().ok()).unwrap_or(b"");
                        if sub.eq_ignore_ascii_case(b"Watermark") {
                            let rect = d
                                .get(b"Rect")
                                .ok()
                                .and_then(|o| resolve(doc, o))
                                .and_then(|o| o.as_array().ok())
                                .map(|v| v.iter().filter_map(num).collect::<Vec<f32>>())
                                .filter(|v| v.len() == 4)
                                .map(|v| (v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])))
                                .unwrap_or((x0, y0, x1, y1));
                            scan.watermark_annots.push((i, id, rect));
                        }
                    }
                }
            }
        }
    }

    let content = match doc.get_and_decode_page_content(page_id) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(page, error = %e, "content stream decode failed");
            scan.decode_ok = false;
            return scan;
        }
    };
    let ops: &[Operation] = &content.operations;
    let ext_g = res.as_ref().and_then(|r| r.get(b"ExtGState").ok()).and_then(|o| dict_of(doc, o)).cloned();
    let xobjs = res.as_ref().and_then(|r| r.get(b"XObject").ok()).and_then(|o| dict_of(doc, o)).cloned();

    let mut stack: Vec<GState> = Vec::new();
    let mut gs = GState { ctm: Matrix::IDENTITY, fill_alpha: 1.0, stroke_alpha: 1.0 };
    // 标记内容栈：(起始 op, 是否水印)
    let mut mc: Vec<(usize, bool)> = Vec::new();
    let in_wm = |mc: &Vec<(usize, bool)>| mc.iter().any(|m| m.1);

    // 文字状态
    let mut text_start: Option<usize> = None;
    let mut tm = Matrix::IDENTITY;
    let mut tlm = Matrix::IDENTITY;
    let mut font_size = 12.0f32;
    let mut leading = 0.0f32;
    let mut render_mode = 0i64;
    let mut raw = Vec::new();
    let mut first_show: Option<(Matrix, Matrix)> = None;
    let mut text_len = 0usize;

    let mut text_ops = 0usize;
    let mut image_cover = 0.0f32;

    for (i, op) in ops.iter().enumerate() {
        let o = &op.operands;
        match op.operator.as_str() {
            "q" => stack.push(gs),
            "Q" => {
                if let Some(s) = stack.pop() {
                    gs = s;
                }
            }
            "cm" => {
                if let Some(m) = matrix_from(o) {
                    gs.ctm = m.mul(&gs.ctm);
                }
            }
            "gs" => {
                if let (Some(Object::Name(n)), Some(eg)) = (o.first(), ext_g.as_ref()) {
                    if let Some(d) = eg.get(n).ok().and_then(|x| dict_of(doc, x)) {
                        if let Some(a) = d.get(b"ca").ok().and_then(|x| resolve(doc, x)).and_then(num) {
                            gs.fill_alpha = a;
                        }
                        if let Some(a) = d.get(b"CA").ok().and_then(|x| resolve(doc, x)).and_then(num) {
                            gs.stroke_alpha = a;
                        }
                    }
                }
            }
            "BDC" | "BMC" => {
                let wm = op.operator == "BDC"
                    && o.first().and_then(|t| t.as_name().ok()).is_some_and(|t| t == b"Artifact" || t == b"OC")
                    && o.get(1).is_some_and(|p| is_watermark_props(doc, p, res.as_ref()));
                mc.push((i, wm));
            }
            "EMC" => {
                if let Some((s, wm)) = mc.pop() {
                    if wm {
                        scan.artifacts.push(ArtifactRange { page, start: s, end: i + 1 });
                    }
                }
            }
            "BT" => {
                text_start = Some(i);
                tm = Matrix::IDENTITY;
                tlm = Matrix::IDENTITY;
                raw.clear();
                first_show = None;
                text_len = 0;
            }
            "Tf" => {
                if let Some(s) = o.get(1).and_then(num) {
                    font_size = s;
                }
            }
            "TL" => leading = o.first().and_then(num).unwrap_or(leading),
            "Tr" => render_mode = o.first().and_then(|x| x.as_i64().ok()).unwrap_or(0),
            "Tm" => {
                if let Some(m) = matrix_from(o) {
                    tm = m;
                    tlm = m;
                }
            }
            "Td" | "TD" => {
                let (tx, ty) = (o.first().and_then(num).unwrap_or(0.0), o.get(1).and_then(num).unwrap_or(0.0));
                if op.operator == "TD" {
                    leading = -ty;
                }
                tlm = Matrix([1.0, 0.0, 0.0, 1.0, tx, ty]).mul(&tlm);
                tm = tlm;
            }
            "T*" => {
                tlm = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, -leading]).mul(&tlm);
                tm = tlm;
            }
            "Tj" | "TJ" | "'" | "\"" => {
                text_ops += 1;
                if first_show.is_none() {
                    first_show = Some((tm, gs.ctm));
                }
                for x in o {
                    match x {
                        Object::String(s, _) => {
                            raw.extend_from_slice(s);
                            text_len += s.len();
                        }
                        Object::Array(a) => {
                            for y in a {
                                if let Object::String(s, _) = y {
                                    raw.extend_from_slice(s);
                                    text_len += s.len();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "ET" => {
                if let (Some(start), Some((tm0, ctm0))) = (text_start.take(), first_show) {
                    let m = tm0.mul(&ctm0);
                    let fs = font_size * m.scale();
                    // 近似文字宽度：平均字宽 0.5em
                    let w_units = font_size * 0.5 * text_len.max(1) as f32;
                    let bb = tm0.mul(&ctm0).bbox(0.0, -font_size * 0.2, w_units, font_size * 0.9);
                    scan.texts.push(TextBlock {
                        page,
                        start,
                        end: i + 1,
                        raw: raw.clone(),
                        font_size: fs,
                        rotation: m.rotation_deg(),
                        alpha: gs.fill_alpha,
                        bbox: bb,
                        in_watermark_artifact: in_wm(&mc),
                        invisible: render_mode == 3,
                    });
                }
            }
            "Do" => {
                if let (Some(Object::Name(n)), Some(xd)) = (o.first(), xobjs.as_ref()) {
                    let entry = xd.get(n).ok();
                    let id = entry.and_then(|e| e.as_reference().ok());
                    let stream = entry.and_then(|e| resolve(doc, e)).and_then(|s| s.as_stream().ok());
                    let is_image = stream.is_some_and(|s| s.dict.get(b"Subtype").ok().and_then(|x| x.as_name().ok()) == Some(b"Image"));
                    let (bx0, by0, bx1, by1, isize_) = if is_image {
                        let s = stream.unwrap();
                        let w = s.dict.get(b"Width").ok().and_then(|x| x.as_i64().ok()).unwrap_or(0);
                        let h = s.dict.get(b"Height").ok().and_then(|x| x.as_i64().ok()).unwrap_or(0);
                        (0.0, 0.0, 1.0, 1.0, Some((w, h)))
                    } else {
                        let bb = stream
                            .and_then(|s| s.dict.get(b"BBox").ok())
                            .and_then(|b| b.as_array().ok())
                            .map(|a| a.iter().filter_map(num).collect::<Vec<f32>>())
                            .filter(|v| v.len() == 4)
                            .unwrap_or(vec![0.0, 0.0, pw, ph]);
                        let fm = stream
                            .and_then(|s| s.dict.get(b"Matrix").ok())
                            .and_then(|m| m.as_array().ok())
                            .and_then(|a| matrix_from(a))
                            .unwrap_or(Matrix::IDENTITY);
                        let (a, b, c, d) = fm.bbox(bb[0], bb[1], bb[2], bb[3]);
                        (a, b, c, d, None)
                    };
                    let bbox = gs.ctm.bbox(bx0, by0, bx1, by1);
                    if is_image {
                        let area = ((bbox.2 - bbox.0) * (bbox.3 - bbox.1)).max(0.0);
                        image_cover = image_cover.max(area / (pw * ph).max(1.0));
                    }
                    scan.xobjects.push(XObjectUse {
                        page,
                        op_index: i,
                        name: n.clone(),
                        id,
                        is_image,
                        ctm: gs.ctm,
                        alpha: gs.fill_alpha.min(gs.stroke_alpha),
                        bbox,
                        in_watermark_artifact: in_wm(&mc),
                        image_size: isize_,
                    });
                }
            }
            _ => {}
        }
    }
    let visible_text = scan.texts.iter().filter(|t| !t.invisible).count();
    scan.kind = if image_cover >= 0.85 && visible_text <= 2 {
        PageKind::Scanned
    } else if image_cover >= 0.2 {
        PageKind::Mixed
    } else if text_ops > 0 || !scan.xobjects.is_empty() {
        PageKind::Native
    } else {
        PageKind::Empty
    };
    let _ = text_ops;
    scan
}

/// 一个原生水印判定。
#[derive(Debug, Clone)]
pub struct NativeFinding {
    pub page: u32,
    pub reference: PdfObjectRef,
    pub confidence: f32,
    pub bbox: (f32, f32, f32, f32),
    pub rotation: f32,
    pub opacity: Option<f32>,
    pub text: Option<String>,
    pub evidence: Vec<(&'static str, f32)>,
}

fn in_header_footer(b: (f32, f32, f32, f32), ph: f32) -> bool {
    let band = ph * 0.12;
    b.1 >= ph - band || b.3 <= band
}

fn rotated(r: f32) -> bool {
    let a = r.abs() % 180.0;
    a > 8.0 && a < 172.0
}

pub fn decode_display_text(raw: &[u8]) -> String {
    // 仅用于界面展示：UTF-16BE（带 BOM）或按 Latin-1 解码
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        let u: Vec<u16> = raw[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    raw.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '·' }).collect::<String>().trim().to_string()
}

/// 跨页综合判定原生水印。
pub fn find_native_watermarks(pages: &[PageScan]) -> Vec<NativeFinding> {
    let n_pages = pages.len().max(1);
    let mut out = Vec::new();

    for p in pages {
        // 1) Watermark 注释
        for (idx, id, rect) in &p.watermark_annots {
            out.push(NativeFinding {
                page: p.page,
                reference: PdfObjectRef { page: p.page, kind: RefKind::Annotation { index: *idx, id: id.map(|i| (i.0, i.1)) } },
                confidence: 0.97,
                bbox: *rect,
                rotation: 0.0,
                opacity: None,
                text: None,
                evidence: vec![("watermark_annotation", 1.0)],
            });
        }
        // 2) 水印标记内容
        for a in &p.artifacts {
            let bb = union_bbox(
                p.xobjects
                    .iter()
                    .filter(|x| x.op_index >= a.start && x.op_index < a.end)
                    .map(|x| x.bbox)
                    .chain(p.texts.iter().filter(|t| t.start >= a.start && t.end <= a.end).map(|t| t.bbox)),
            )
            .unwrap_or((0.0, 0.0, p.width, p.height));
            let text = p.texts.iter().find(|t| t.start >= a.start && t.end <= a.end).map(|t| decode_display_text(&t.raw));
            out.push(NativeFinding {
                page: p.page,
                reference: PdfObjectRef { page: p.page, kind: RefKind::OpRange { start: a.start, end: a.end, what: "artifact".into() } },
                confidence: 0.96,
                bbox: bb,
                rotation: 0.0,
                opacity: None,
                text,
                evidence: vec![("watermark_marked_content", 1.0)],
            });
        }
    }

    // 3) 重复 XObject 调用
    let mut by_obj: HashMap<(u32, u16), Vec<&XObjectUse>> = HashMap::new();
    for p in pages {
        for x in &p.xobjects {
            if x.in_watermark_artifact {
                continue;
            }
            if let Some(id) = x.id {
                by_obj.entry((id.0, id.1)).or_default().push(x);
            }
        }
    }
    for uses in by_obj.values() {
        let pages_hit: std::collections::BTreeSet<u32> = uses.iter().map(|u| u.page).collect();
        let freq = pages_hit.len() as f32 / n_pages as f32;
        for u in uses {
            let p = &pages[u.page as usize];
            // 扫描件的整页图片不是水印
            let area = (u.bbox.2 - u.bbox.0) * (u.bbox.3 - u.bbox.1);
            let page_area = (p.width * p.height).max(1.0);
            if u.is_image && area / page_area > 0.8 && u.alpha >= 0.95 {
                continue;
            }
            let first = uses.iter().find(|v| v.page == *pages_hit.iter().next().unwrap()).unwrap();
            let consistent = (u.bbox.0 - first.bbox.0).abs() < 3.0 && (u.bbox.1 - first.bbox.1).abs() < 3.0;
            let rot = rotated(u.ctm.rotation_deg());
            let transparent = u.alpha < 0.9;
            let big = area / page_area > 0.08;
            let hf = in_header_footer(u.bbox, p.height) && !rot && !transparent;
            let mut s = 0.3f32;
            let mut ev = Vec::new();
            if n_pages >= 2 {
                s += 0.25 * freq;
                ev.push(("page_frequency", freq));
                if consistent && pages_hit.len() >= 2 {
                    s += 0.08;
                    ev.push(("position_consistency", 1.0));
                }
            }
            if rot {
                s += 0.17;
                ev.push(("rotation", 1.0));
            }
            if transparent {
                s += 0.2;
                ev.push(("transparency", 1.0 - u.alpha));
            }
            if big {
                s += 0.07;
                ev.push(("large_stamp", 1.0));
            }
            if hf {
                // 页眉页脚模板信息保护
                s -= 0.25;
                ev.push(("header_footer_template", 1.0));
            }
            // 单一证据（只有“每页出现”）不足以判定
            if !rot && !transparent {
                s = s.min(0.6);
            }
            let conf = s.clamp(0.0, 0.95);
            if conf < 0.5 {
                continue;
            }
            out.push(NativeFinding {
                page: u.page,
                reference: PdfObjectRef {
                    page: u.page,
                    kind: RefKind::OpRange {
                        start: u.op_index,
                        end: u.op_index + 1,
                        what: format!("xobject:{}", String::from_utf8_lossy(&u.name)),
                    },
                },
                confidence: conf,
                bbox: u.bbox,
                rotation: u.ctm.rotation_deg(),
                opacity: Some(u.alpha),
                text: None,
                evidence: ev,
            });
        }
    }

    // 4) 文字水印：相同文字、相同位置/旋转、低透明度、大字体、跨页重复
    let mut by_text: HashMap<(Vec<u8>, i32, i32), Vec<&TextBlock>> = HashMap::new();
    for p in pages {
        for t in &p.texts {
            if t.in_watermark_artifact || t.invisible || t.raw.len() < 2 {
                continue;
            }
            by_text.entry((t.raw.clone(), t.font_size.round() as i32, (t.rotation / 5.0).round() as i32)).or_default().push(t);
        }
    }
    for blocks in by_text.values() {
        let pages_hit: std::collections::BTreeSet<u32> = blocks.iter().map(|b| b.page).collect();
        let freq = pages_hit.len() as f32 / n_pages as f32;
        for b in blocks {
            let p = &pages[b.page as usize];
            let rot = rotated(b.rotation);
            let transparent = b.alpha < 0.9;
            let large = b.font_size >= 28.0;
            let hf = in_header_footer(b.bbox, p.height) && !rot && !transparent && b.font_size < 16.0;
            let mut s = 0.25f32;
            let mut ev = Vec::new();
            if n_pages >= 2 && pages_hit.len() >= 2 {
                s += 0.22 * freq;
                ev.push(("page_frequency", freq));
            }
            if rot {
                s += 0.2;
                ev.push(("rotation", 1.0));
            }
            if transparent {
                s += 0.22;
                ev.push(("transparency", 1.0 - b.alpha));
            }
            if large {
                s += 0.12;
                ev.push(("large_font", 1.0));
            }
            if hf {
                s -= 0.3;
                ev.push(("header_footer_template", 1.0));
            }
            if !rot && !transparent {
                s = s.min(0.55);
            }
            let conf = s.clamp(0.0, 0.95);
            if conf < 0.5 {
                continue;
            }
            out.push(NativeFinding {
                page: b.page,
                reference: PdfObjectRef { page: b.page, kind: RefKind::OpRange { start: b.start, end: b.end, what: "text".into() } },
                confidence: conf,
                bbox: b.bbox,
                rotation: b.rotation,
                opacity: Some(b.alpha),
                text: Some(decode_display_text(&b.raw)),
                evidence: ev,
            });
        }
    }
    out.sort_by(|a, b| (a.page, a.bbox.0 as i64).cmp(&(b.page, b.bbox.0 as i64)));
    out
}

fn union_bbox(it: impl Iterator<Item = (f32, f32, f32, f32)>) -> Option<(f32, f32, f32, f32)> {
    it.reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}
