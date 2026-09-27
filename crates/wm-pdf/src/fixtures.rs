//! 测试与 Golden 样本生成：用 lopdf 构造具有代表性的 PDF。
//! 仅用于测试、benchmark 与合成数据集；不参与正常处理流程。

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, ObjectId, Stream, StringFormat};

/// 旋转 45° 的矩阵系数（cos 45° = sin 45°）。
const R45: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn s(t: &str) -> Object {
    Object::String(t.as_bytes().to_vec(), StringFormat::Literal)
}

fn op(o: &str, args: Vec<Object>) -> Operation {
    Operation::new(o, args)
}

fn text(font_size: i64, x: f32, y: f32, t: &str) -> Vec<Operation> {
    vec![
        op("BT", vec![]),
        op("Tf", vec!["F1".into(), font_size.into()]),
        op("Td", vec![x.into(), y.into()]),
        op("Tj", vec![s(t)]),
        op("ET", vec![]),
    ]
}

struct Builder {
    doc: Document,
    pages_id: ObjectId,
    kids: Vec<Object>,
}

impl Builder {
    fn new() -> Self {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        Self { doc, pages_id, kids: Vec::new() }
    }

    fn font(&mut self) -> ObjectId {
        self.doc
            .add_object(dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding"})
    }

    fn page(&mut self, ops: Vec<Operation>, resources: Object, extra: Vec<(&str, Object)>) -> ObjectId {
        let content = Content { operations: ops }.encode().unwrap();
        let cid = self.doc.add_object(Stream::new(dictionary! {}, content));
        let mut d = dictionary! {
            "Type" => "Page",
            "Parent" => self.pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => cid,
            "Resources" => resources,
        };
        for (k, v) in extra {
            d.set(k, v);
        }
        let id = self.doc.add_object(d);
        self.kids.push(id.into());
        id
    }

    fn finish(mut self, catalog_extra: Vec<(&str, Object)>) -> Document {
        let count = self.kids.len() as i64;
        self.doc.objects.insert(self.pages_id, Object::Dictionary(dictionary! {"Type" => "Pages", "Kids" => self.kids, "Count" => count}));
        let mut cat = dictionary! {"Type" => "Catalog", "Pages" => self.pages_id};
        for (k, v) in catalog_extra {
            cat.set(k, v);
        }
        let cat_id = self.doc.add_object(cat);
        self.doc.trailer.set("Root", cat_id);
        self.doc
    }
}

fn to_bytes(mut doc: Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

/// 原生 PDF：每页有页眉、正文、页码，以及以共享 Form XObject 绘制的 45° 半透明 “CONFIDENTIAL” 水印。
pub fn native_with_xobject_watermark(pages: usize) -> Vec<u8> {
    let mut b = Builder::new();
    let f1 = b.font();
    let form_content = Content { operations: text(64, 0.0, 10.0, "CONFIDENTIAL") }.encode().unwrap();
    let form = b.doc.add_object(Stream::new(
        dictionary! {"Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 480.into(), 80.into()], "Resources" => dictionary!{"Font" => dictionary!{"F1" => f1}}},
        form_content,
    ));
    let gs = b.doc.add_object(dictionary! {"Type" => "ExtGState", "ca" => 0.2, "CA" => 0.2});
    let res = b.doc.add_object(
        dictionary! {"Font" => dictionary!{"F1" => f1}, "XObject" => dictionary!{"Fm0" => form}, "ExtGState" => dictionary!{"GS0" => gs}},
    );
    for p in 0..pages {
        let mut ops = Vec::new();
        ops.extend(text(10, 72.0, 750.0, "ACME Corp Quarterly Report"));
        for (k, line) in [
            format!("Section {} revenue grew steadily across regions", p + 1),
            format!("Operating margin note {} remains within guidance", p + 1),
        ]
        .iter()
        .enumerate()
        {
            ops.extend(text(12, 72.0, 680.0 - 20.0 * k as f32, line));
        }
        ops.extend(text(9, 290.0, 30.0, &format!("Page {}", p + 1)));
        ops.push(op("q", vec![]));
        ops.push(op("gs", vec!["GS0".into()]));
        ops.push(op("cm", vec![R45.into(), R45.into(), (-R45).into(), R45.into(), 160.into(), 220.into()]));
        ops.push(op("Do", vec!["Fm0".into()]));
        ops.push(op("Q", vec![]));
        b.page(ops, Object::Reference(res), vec![]);
    }
    to_bytes(b.finish(vec![]))
}

/// 单页：Acrobat 风格的 `/Artifact /Watermark` 标记内容 + Watermark 注释 + 普通链接注释。
pub fn native_with_artifact_and_annotation() -> Vec<u8> {
    let mut b = Builder::new();
    let f1 = b.font();
    let gs = b.doc.add_object(dictionary! {"Type" => "ExtGState", "ca" => 0.3});
    let res = dictionary! {"Font" => dictionary!{"F1" => f1}, "ExtGState" => dictionary!{"GS0" => gs}};
    let mut ops = text(12, 72.0, 700.0, "Contract terms and conditions apply to all parties");
    ops.push(op("BDC", vec!["Artifact".into(), Object::Dictionary(dictionary! {"Type" => "Pagination", "Subtype" => "Watermark"})]));
    ops.push(op("q", vec![]));
    ops.push(op("gs", vec!["GS0".into()]));
    ops.push(op("BT", vec![]));
    ops.push(op("Tf", vec!["F1".into(), 72.into()]));
    ops.push(op("Tm", vec![R45.into(), R45.into(), (-R45).into(), R45.into(), 200.into(), 300.into()]));
    ops.push(op("Tj", vec![s("DRAFT")]));
    ops.push(op("ET", vec![]));
    ops.push(op("Q", vec![]));
    ops.push(op("EMC", vec![]));
    let ap = b.doc.add_object(Stream::new(
        dictionary! {"Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 200.into(), 50.into()]},
        b"".to_vec(),
    ));
    let wm_annot = b.doc.add_object(dictionary! {"Type" => "Annot", "Subtype" => "Watermark", "Rect" => vec![100.into(), 100.into(), 300.into(), 150.into()], "AP" => dictionary!{"N" => ap}});
    let link = b
        .doc
        .add_object(dictionary! {"Type" => "Annot", "Subtype" => "Link", "Rect" => vec![72.into(), 690.into(), 200.into(), 710.into()]});
    b.page(ops, Object::Dictionary(res), vec![("Annots", vec![Object::Reference(link), Object::Reference(wm_annot)].into())]);
    to_bytes(b.finish(vec![]))
}

/// 扫描型：每页一张整页 JPEG（带右下角半透明文字水印）。
pub fn scanned(pages: usize) -> Vec<u8> {
    let mut b = Builder::new();
    for p in 0..pages {
        let (w, h) = (510u32, 660u32);
        let mut img = wm_image::synth::photo_like(w, h, 700 + p as u64);
        let mut t = wm_image::synth::Overlay::new(w, h);
        let a = wm_image::synth::render_text("SCAN.EXAMPLE", 2.0);
        wm_image::synth::overlay(&mut img, &mut t, &a, (w - a.width - 16) as i64, (h - a.height - 16) as i64, [255, 255, 255], 0.5);
        let jpg = wm_image::encode(
            &img,
            &wm_image::EncodeOptions { format: wm_image::ImageFormatKind::Jpeg, jpeg_quality: 90, metadata: None, color: None },
        )
        .unwrap()
        .bytes;
        let im = b.doc.add_object(Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode"},
            jpg,
        ));
        let ops = vec![
            op("q", vec![]),
            op("cm", vec![612.into(), 0.into(), 0.into(), 792.into(), 0.into(), 0.into()]),
            op("Do", vec!["Im0".into()]),
            op("Q", vec![]),
        ];
        b.page(ops, Object::Dictionary(dictionary! {"XObject" => dictionary!{"Im0" => im}}), vec![]);
    }
    to_bytes(b.finish(vec![]))
}

/// 需要用户密码的加密 PDF（RC4 128）。
pub fn encrypted(user_password: &str) -> Vec<u8> {
    let mut b = Builder::new();
    let f1 = b.font();
    b.page(text(14, 72.0, 700.0, "Encrypted document body"), Object::Dictionary(dictionary! {"Font" => dictionary!{"F1" => f1}}), vec![]);
    let mut doc = b.finish(vec![]);
    let id = Object::String(b"0123456789abcdef".to_vec(), StringFormat::Hexadecimal);
    doc.trailer.set("ID", vec![id.clone(), id]);
    let version = lopdf::EncryptionVersion::V2 {
        document: &doc,
        owner_password: "owner-pass",
        user_password,
        key_length: 128,
        permissions: lopdf::Permissions::all(),
    };
    let state = lopdf::EncryptionState::try_from(version).unwrap();
    doc.encrypt(&state).unwrap();
    to_bytes(doc)
}

/// 带数字签名字段的 PDF。
pub fn signed() -> Vec<u8> {
    let mut b = Builder::new();
    let f1 = b.font();
    b.page(text(14, 72.0, 700.0, "Signed agreement"), Object::Dictionary(dictionary! {"Font" => dictionary!{"F1" => f1}}), vec![]);
    let sig = b.doc.add_object(dictionary! {"Type" => "Sig", "Filter" => "Adobe.PPKLite", "SubFilter" => "adbe.pkcs7.detached"});
    let field = b.doc.add_object(dictionary! {"FT" => "Sig", "T" => s("Signature1"), "V" => sig});
    to_bytes(b.finish(vec![("AcroForm", Object::Dictionary(dictionary! {"Fields" => vec![Object::Reference(field)], "SigFlags" => 3}))]))
}
