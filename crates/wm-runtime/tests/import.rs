//! 导入阶段的可读性检查：无法读取的文件应立即标记为失败并给出原因，而不是等到扫描时才报错。

use std::sync::Arc;

use wm_core::job::FileStatus;
use wm_runtime::{Engine, NullSink};

fn write_png(path: &std::path::Path) {
    image::RgbImage::from_pixel(32, 24, image::Rgb([200, 180, 160])).save(path).unwrap();
}

#[test]
fn readable_image_is_imported_for_scanning() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("ok.png");
    write_png(&img);
    let engine = Engine::new(&dir.path().join("data"), &dir.path().join("models"), Arc::new(NullSink)).unwrap();
    let r = engine.import(vec![img]).unwrap();
    assert_eq!(r.added.len(), 1);
    assert!(r.blocked.is_empty());
    assert_ne!(r.added[0].status, FileStatus::Failed);
}

#[cfg(unix)]
#[test]
fn unreadable_image_is_blocked_at_import() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("locked.png");
    write_png(&img);
    std::fs::set_permissions(&img, std::fs::Permissions::from_mode(0o000)).unwrap();
    // root 不受文件权限限制，这种环境下无法构造不可读文件
    if std::fs::File::open(&img).is_ok() {
        return;
    }
    let engine = Engine::new(&dir.path().join("data"), &dir.path().join("models"), Arc::new(NullSink)).unwrap();
    let r = engine.import(vec![img.clone()]).unwrap();
    std::fs::set_permissions(&img, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(r.added.len(), 1);
    assert_eq!(r.blocked, vec![img.to_string_lossy().to_string()]);
    let f = &r.added[0];
    assert_eq!(f.status, FileStatus::Failed);
    let err = f.error.as_ref().expect("error view");
    assert_eq!(err.kind, wm_core::ErrorKind::Permission);
}
