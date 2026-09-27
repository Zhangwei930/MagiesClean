//! 预览与导出：预览结果仍有效时，导出直接复用缓存，不重复推理。

use std::sync::Arc;

use wm_core::candidate::UserAction;
use wm_image::synth;
use wm_runtime::{Engine, NullSink};

/// 底部左侧带时间、地址信息块的合成照片。
fn stamp_photo(path: &std::path::Path) {
    stamp_photo_seeded(path, 11);
}

fn stamp_photo_seeded(path: &std::path::Path, seed: u64) {
    let (w, h) = (1200u32, 1600u32);
    let mut img = synth::photo_like(w, h, seed);
    let mut t = synth::Overlay::new(w, h);
    let big = synth::render_text("09:47 2026/07/06", 5.0);
    let small = synth::render_text("SHANGHAI PUDONG ROAD 88", 2.5);
    let y_small = (h - small.height - 24) as i64;
    let y_big = y_small - big.height as i64 - 14;
    for (a, y) in [(&big, y_big), (&small, y_small)] {
        synth::overlay(&mut img, &mut t, a, 26, y + 2, [20, 20, 20], 0.8);
        synth::overlay(&mut img, &mut t, a, 24, y, [255, 255, 255], 1.0);
    }
    wm_image::write_png(&img, path).unwrap();
}

#[test]
fn export_reuses_a_current_preview() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("stamp.png");
    stamp_photo(&src);
    let engine = Engine::new(&dir.path().join("data"), &dir.path().join("models"), Arc::new(NullSink)).unwrap();
    let id = engine.import(vec![src]).unwrap().added[0].id.clone();
    engine.scan_blocking(None).unwrap();
    let f = engine.file(&id).unwrap();
    assert!(f.candidates.iter().any(|c| c.watermark_type == wm_core::WatermarkType::InfoStamp), "info stamp detected");

    engine.resolve_pending(&id, UserAction::Remove).unwrap();
    let preview = engine.generate_preview(&id).unwrap();
    let cached = std::path::PathBuf::from(preview.result_file.expect("preview result"));
    let written = std::fs::metadata(&cached).unwrap().modified().unwrap();

    let exported = engine.export_file(&id).unwrap();
    let out = std::path::PathBuf::from(exported.output.expect("exported"));
    // 缓存结果没有被重新生成，导出内容与预览一致
    assert_eq!(exported.result_file.as_deref(), Some(cached.to_string_lossy().as_ref()));
    assert_eq!(std::fs::metadata(&cached).unwrap().modified().unwrap(), written);
    let a = wm_image::decode(&cached, None).unwrap().buffer;
    let b = wm_image::decode(&out, None).unwrap().buffer;
    assert_eq!((a.width, a.height), (b.width, b.height));
    // RGB 完全一致（Alpha 通道可能因输出格式不同而省略）
    assert!(a.data.iter().zip(&b.data).enumerate().all(|(i, (p, q))| i % 4 == 3 || p == q));
}

#[test]
fn batch_can_include_pending_detections() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<_> = (0..2)
        .map(|i| {
            let p = dir.path().join(format!("stamp{i}.png"));
            stamp_photo_seeded(&p, 20 + i);
            p
        })
        .collect();
    let engine = Engine::new(&dir.path().join("data"), &dir.path().join("models"), Arc::new(NullSink)).unwrap();
    engine.import(files).unwrap();
    engine.scan_blocking(None).unwrap();
    // 单图启发式结果都在复核中：默认的批量处理不会处理它们
    assert_eq!(engine.summary().ready_to_process, 0);
    assert!(engine.summary().candidates_pending >= 2);

    // 用户选择“待复核的一起去除”
    engine.approve_pending(None);
    assert_eq!(engine.summary().candidates_pending, 0);
    let r = engine.process_blocking(None).unwrap();
    assert_eq!(r.completed, 2, "{r:?}");
}
